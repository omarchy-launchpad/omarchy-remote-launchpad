//! AppKit machine list and onboarding actions.
use crate::{
    config::{self, Host},
    launcher::rect,
    machine_store::{self, Machine, MachineConnection},
    remote::{self, App, Task},
    vm_run::{self, VmRun},
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSIndexSet, NSObject, NSObjectProtocol, NSString, NSUserName,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};

pub struct CatalogueCheck(Instant, Task<Vec<App>>);

impl CatalogueCheck {
    pub fn start(host: &Host) -> Result<Self, String> {
        Ok(Self(Instant::now(), remote::catalogue(host)?))
    }

    pub fn poll(&self) -> Option<Result<usize, String>> {
        if let Some(result) = self.1.poll() {
            return Some(result.and_then(|apps| {
                if apps.is_empty() {
                    Err("No desktop apps found. Log in to Omarchy and try again.".into())
                } else {
                    Ok(apps.len())
                }
            }));
        }
        (self.0.elapsed() >= Duration::from_secs(30))
            .then(|| Err("Timed out reading desktop apps.".into()))
    }
}

fn text(value: &str) -> Retained<NSString> {
    NSString::from_str(value)
}

#[derive(Default)]
struct TableData {
    saved: Vec<Machine>,
    active: Option<Host>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<TableData>]
    struct MachineRows;
    unsafe impl NSObjectProtocol for MachineRows {}
    unsafe impl NSTableViewDataSource for MachineRows {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn count(&self, _: &NSTableView) -> isize {
            self.ivars().borrow().saved.len() as isize
        }
    }
    unsafe impl NSControlTextEditingDelegate for MachineRows {}
    unsafe impl NSTableViewDelegate for MachineRows {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn cell(&self, _: &NSTableView, column: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSView>> {
            self.make_cell(column, row)
        }
    }
);

impl MachineRows {
    fn make_cell(&self, column: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSView>> {
        let column = column?;
        let data = self.ivars().borrow();
        let machine = data.saved.get(row as usize)?;
        let value = match column.identifier().to_string().as_str() {
            "machine" => machine.name.clone(),
            "type" => machine.kind().into(),
            "address" => machine.address(),
            _ => {
                let current = data
                    .active
                    .as_ref()
                    .is_some_and(|host| machine.matches(host));
                if current { "Current" } else { "" }.into()
            }
        };
        let label = NSTextField::labelWithString(&text(&value), self.mtm());
        label.setUsesSingleLineMode(true);
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        label.setToolTip(Some(&text(&value)));
        Some(label.into_super().into_super())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NewMachine {
    Remote,
    Vm,
}

enum Operation {
    Checking(CatalogueCheck),
    Installing(Instant, Task<()>),
}

struct RuntimePreparation {
    result: Receiver<Result<PathBuf, String>>,
    queued: HashMap<u16, Machine>,
}

pub struct Machines {
    window: Retained<NSWindow>,
    rows: Retained<MachineRows>,
    table: Retained<NSTableView>,
    input: Retained<NSTextField>,
    hint: Retained<NSTextField>,
    feedback: Retained<NSTextField>,
    add_remote: Retained<NSButton>,
    create_vm: Retained<NSButton>,
    save: Retained<NSButton>,
    cancel: Retained<NSButton>,
    open_vm: Retained<NSButton>,
    setup_vm: Retained<NSButton>,
    install: Retained<NSButton>,
    connect: Retained<NSButton>,
    remove: Retained<NSButton>,
    path: PathBuf,
    target_path: PathBuf,
    load_error: Option<String>,
    editing: Option<NewMachine>,
    selection: isize,
    pending: Option<(Host, Operation)>,
    vms: HashMap<u16, VmRun>,
    runtime: Option<RuntimePreparation>,
    vm_data_directory: PathBuf,
}

impl Machines {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject, target_path: PathBuf) -> Self {
        let path = target_path.with_extension("machines.json");
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0., 0., 624., 426.),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // The controller owns the window between presentations.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&text("Manage Machines"));
        window.center();
        let view = window
            .contentView()
            .expect("machine window has a content view");
        let label = |y, height| {
            let label = NSTextField::wrappingLabelWithString(&text(""), mtm);
            label.setFrame(rect(20., y, 584., height));
            label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            view.addSubview(&label);
            label
        };
        let button = |title: &str, action: Sel, x, y, width| {
            // SAFETY: Each supplied action accepts the button sender.
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &text(title),
                    Some(target),
                    Some(action),
                    mtm,
                )
            };
            button.setFrame(rect(x, y, width, 28.));
            view.addSubview(&button);
            button
        };

        let (saved, load_error) = match machine_store::load(&path) {
            Ok(saved) => (saved, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let rows: Retained<MachineRows> = unsafe {
            msg_send![
                super(MachineRows::alloc(mtm).set_ivars(RefCell::new(TableData {
                    saved,
                    active: None
                }))),
                init
            ]
        };
        let scroll =
            NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(20., 166., 584., 204.));
        scroll.setHasVerticalScroller(true);
        scroll.setBorderType(NSBorderType::BezelBorder);
        let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0., 0., 582., 202.));
        table.setUsesAlternatingRowBackgroundColors(true);
        table.setRowHeight(23.);
        table.setAccessibilityLabel(Some(&text("Saved machines")));
        for (id, title, width) in [
            ("machine", "Machine", 192.),
            ("type", "Type", 76.),
            ("address", "SSH address", 213.),
            ("status", "Status", 81.),
        ] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &text(id));
            column.setTitle(&text(title));
            column.setWidth(width);
            table.addTableColumn(&column);
        }
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*rows)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*rows)));
        }
        scroll.setDocumentView(Some(&table));
        view.addSubview(&scroll);
        table.reloadData();

        let add_remote = button("Add Remote", sel!(addRemoteMachine:), 18., 380., 116.);
        let create_vm = button("Create VM", sel!(createLocalVM:), 140., 380., 106.);
        let remove = button("Remove", sel!(removeMachine:), 502., 380., 102.);
        let hint = label(119., 36.);
        let input = NSTextField::textFieldWithString(&text(""), mtm);
        input.setFrame(rect(20., 87., 244., 24.));
        view.addSubview(&input);
        let save = button("Save Machine", sel!(saveMachine:), 440., 85., 164.);
        let cancel = button("Cancel", sel!(cancelMachine:), 340., 85., 94.);
        let open_vm = button("Open VM", sel!(startLocalVM:), 270., 85., 96.);
        let setup_vm = button("Set up access…", sel!(setupLocalVM:), 371., 85., 126.);
        let install = button("Install agent", sel!(installRemoteAgent:), 372., 85., 125.);
        let connect = button("Connect", sel!(connectMachine:), 502., 85., 102.);
        let feedback = label(10., 63.);

        let machines = Self {
            window,
            rows,
            table,
            input,
            hint,
            feedback,
            add_remote,
            create_vm,
            save,
            cancel,
            open_vm,
            setup_vm,
            install,
            connect,
            remove,
            path,
            target_path,
            load_error,
            editing: None,
            selection: -1,
            pending: None,
            vms: HashMap::new(),
            runtime: None,
            vm_data_directory: config::directory(),
        };
        machines.update_controls();
        if let Some(error) = &machines.load_error {
            machines.message(error);
        }
        machines
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "used by the standalone AppKit integration test")]
    pub fn use_vm_data_directory(&mut self, directory: PathBuf) {
        self.vm_data_directory = directory;
    }

    pub fn present(&mut self) {
        if self.table.selectedRow() < 0 && self.editing.is_none() && self.table.numberOfRows() > 0 {
            let active = self.rows.ivars().borrow();
            let row = active
                .active
                .as_ref()
                .and_then(|host| {
                    active
                        .saved
                        .iter()
                        .position(|machine| machine.matches(host))
                })
                .unwrap_or(0);
            drop(active);
            self.select(row);
        }
        self.window.makeKeyAndOrderFront(None);
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.window.mtm()).activateIgnoringOtherApps(true);
    }

    pub fn set_current(&mut self, host: Option<&Host>) {
        if let Some(host) = host
            && self.load_error.is_none()
        {
            let mut saved = self.rows.ivars().borrow().saved.clone();
            if !saved.iter().any(|machine| machine.matches(host)) {
                saved.push(Machine::from_host(host));
                if let Err(error) = machine_store::save(&self.path, &saved) {
                    self.message(&error);
                }
                self.rows.ivars().borrow_mut().saved = saved;
            }
        }
        self.rows.ivars().borrow_mut().active = host.cloned();
        self.reload_rows();
    }

    pub fn load_current(&self) -> Result<Host, String> {
        Host::load_from(&self.target_path)
    }

    fn selected(&self) -> Option<Machine> {
        self.rows
            .ivars()
            .borrow()
            .saved
            .get(self.table.selectedRow() as usize)
            .cloned()
    }

    fn select(&mut self, row: usize) {
        self.table
            .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
        self.table.scrollRowToVisible(row as isize);
        self.selection_changed();
    }

    fn reload_rows(&mut self) {
        let last = self.table.selectedRow();
        self.table.reloadData();
        if last >= 0 && last < self.table.numberOfRows() {
            self.select(last as usize);
        }
    }

    fn selection_changed(&mut self) {
        self.selection = self.table.selectedRow();
        self.editing = None;
        if let Some(Machine {
            connection: MachineConnection::Vm { user, port, .. },
            ..
        }) = self.selected()
        {
            let suggestion = if user.is_empty() {
                mac_guest_username()
            } else {
                user
            };
            self.set_input("Guest username", &suggestion);
            if let Some(run) = self.vms.get(&port) {
                self.message(run.message());
            } else if self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.queued.contains_key(&port))
            {
                self.message("Preparing the Omarchy runtime…");
            }
        }
        self.update_controls();
    }

    fn set_input(&self, label: &str, value: &str) {
        self.input.setPlaceholderString(Some(&text(label)));
        self.input.setAccessibilityLabel(Some(&text(label)));
        self.input.setStringValue(&text(value));
    }

    fn message(&self, value: &str) {
        self.feedback.setStringValue(&text(value));
        self.feedback.setToolTip(Some(&text(value)));
    }

    fn store(&self, saved: Vec<Machine>) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(error.clone());
        }
        machine_store::save(&self.path, &saved)?;
        self.rows.ivars().borrow_mut().saved = saved;
        Ok(())
    }

    pub fn remove_selected(&mut self) -> bool {
        let row = self.table.selectedRow() as usize;
        let mut saved = self.rows.ivars().borrow().saved.clone();
        if self.editing.is_some()
            || self.pending.is_some()
            || self.load_error.is_some()
            || row >= saved.len()
        {
            return false;
        }
        let removed = saved.remove(row);
        let was_current = self
            .rows
            .ivars()
            .borrow()
            .active
            .as_ref()
            .is_some_and(|host| removed.matches(host));
        let previous_target = match self.clear_target(&removed) {
            Ok(previous) => previous,
            Err(error) => {
                self.message(&format!("Could not clear the selected machine. {error}"));
                return false;
            }
        };
        if let Err(error) = self.store(saved) {
            if let Some(previous) = previous_target
                && let Err(restore) = config::write_atomic(&self.target_path, &previous)
            {
                self.message(&format!(
                    "{error} Could not restore the selected machine. {restore}"
                ));
            } else {
                self.message(&error);
            }
            return false;
        }

        if let MachineConnection::Vm { port, .. } = removed.connection
            && let Some(runtime) = &mut self.runtime
        {
            runtime.queued.remove(&port);
        }
        self.table.reloadData();
        let count = self.table.numberOfRows() as usize;
        if count > 0 {
            self.select(row.min(count - 1));
        } else {
            self.selection_changed();
        }
        self.message(&format!("Removed {}.", removed.name));
        was_current
    }

    fn clear_target(&self, removed: &Machine) -> io::Result<Option<Vec<u8>>> {
        let previous = match fs::read(&self.target_path) {
            Ok(previous) => previous,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if let Ok(value) = std::str::from_utf8(&previous)
            && Host::parse(value).is_ok_and(|host| removed.matches(&host))
        {
            // Keep the file so checkout-local configuration stays in use.
            config::write_atomic(&self.target_path, b"")?;
            return Ok(Some(previous));
        }
        Ok(None)
    }

    pub fn begin_add(&mut self, kind: NewMachine) {
        self.editing = Some(kind);
        match kind {
            NewMachine::Remote => self.set_input("SSH alias or user@host", ""),
            NewMachine::Vm => {
                let data = self.rows.ivars().borrow();
                self.set_input("VM name", &default_vm_name(&data.saved));
            }
        }
        self.message("");
        self.update_controls();
        self.window.makeFirstResponder(Some(&self.input));
        if kind == NewMachine::Vm {
            // The whole suggested name is selected so typing replaces it.
            unsafe { self.input.selectText(None) };
        }
    }

    pub fn cancel_add(&mut self) {
        self.message("");
        self.selection_changed();
    }

    pub fn save_new(&mut self) {
        let Some(kind) = self.editing else {
            return;
        };
        self.input.validateEditing();
        let value = self.input.stringValue().to_string();
        let mut saved = self.rows.ivars().borrow().saved.clone();
        let machine = match kind {
            NewMachine::Vm => {
                machine_store::create_vm(&value, &saved, |port| self.vms.contains_key(&port))
            }
            NewMachine::Remote => match Host::parse(&value) {
                Ok(host @ Host::Ssh(_)) => {
                    if let Some(row) = saved.iter().position(|machine| machine.matches(&host)) {
                        self.select(row);
                        self.message("This SSH machine is already saved.");
                        return;
                    }
                    Ok(Machine::from_host(&host))
                }
                _ => Err("Enter an SSH alias or user@host.".into()),
            },
        };
        match machine.and_then(|machine| {
            saved.push(machine);
            self.store(saved)
        }) {
            Ok(()) => {
                self.table.reloadData();
                self.select((self.table.numberOfRows() - 1) as usize);
                if kind == NewMachine::Vm {
                    self.open_selected_vm();
                } else {
                    self.message("Machine saved. Click Connect when SSH is ready.");
                }
            }
            Err(error) => self.message(&error),
        }
    }

    pub fn open_selected_vm(&mut self) {
        let Some(machine) = self.selected() else {
            return;
        };
        let MachineConnection::Vm { port, .. } = machine.connection else {
            return;
        };
        if let Some(run) = self.vms.get(&port) {
            self.message(run.message());
            return;
        }

        let preparation = self.runtime.get_or_insert_with(|| RuntimePreparation {
            result: vm_run::prepare_runtime(self.vm_data_directory.join("Runtime")),
            queued: HashMap::new(),
        });
        preparation.queued.insert(port, machine);
        self.message("Preparing the Omarchy runtime…");
    }

    fn poll_runtime(&mut self) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let result = match runtime.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Could not prepare the Omarchy runtime.".into()),
        };
        let queued = self.runtime.take().expect("polled runtime").queued;
        let helper = match result {
            Ok(helper) => helper,
            Err(error) => {
                self.message(&error);
                return;
            }
        };
        let selected = self.selected_vm_port();
        for (port, machine) in queued {
            let result = machine
                .vm_storage(&self.vm_data_directory.join("VMs"))
                .and_then(|storage| {
                    let factory_cache = self.vm_data_directory.join("VMs/shell-factory");
                    VmRun::start(
                        &helper,
                        &machine.name,
                        storage.as_deref(),
                        Some(&factory_cache),
                    )
                });
            match result {
                Ok(run) => {
                    if selected == Some(port) {
                        self.message(run.message());
                    }
                    self.vms.insert(port, run);
                }
                Err(error) => self.message(&error),
            }
        }
    }

    fn selected_vm_port(&self) -> Option<u16> {
        match self.selected()?.connection {
            MachineConnection::Vm { port, .. } if self.editing.is_none() => Some(port),
            _ => None,
        }
    }

    fn poll_vms(&mut self) {
        let active = self.selected_vm_port();
        let mut message = None;
        self.vms.retain(|port, run| {
            let (changed, exited) = run.poll();
            if changed && active == Some(*port) {
                message = Some(run.message().to_owned());
            }
            !exited
        });
        if let Some(message) = message {
            self.message(&message);
        }
    }

    fn selected_host(&mut self) -> Result<Host, String> {
        let row = self.table.selectedRow() as usize;
        let mut machine = self.selected().ok_or("Select a machine first.")?;
        if let MachineConnection::Vm { user, port, .. } = &mut machine.connection {
            self.input.validateEditing();
            let entered = self.input.stringValue().to_string();
            let host = machine_store::vm_host(&entered, *port)?;
            if let Host::LocalVm { user: entered, .. } = &host
                && user != entered
            {
                user.clone_from(entered);
                let mut saved = self.rows.ivars().borrow().saved.clone();
                saved[row] = machine;
                self.store(saved)?;
                self.reload_rows();
            }
            return Ok(host);
        }
        machine.host()
    }

    pub fn setup_selected_vm(&mut self) {
        let result = self.selected_host().and_then(|host| {
            let Host::LocalVm { user, port, .. } = host else {
                return Err("Select a VM first.".into());
            };
            start_vm_setup(&user, port)
        });
        match result {
            Ok(()) => self.message("Finish setup in Terminal, then return and click Connect."),
            Err(error) => self.message(&error),
        }
    }

    pub fn connect(&mut self, install: bool) {
        if self.pending.is_some() {
            return;
        }
        let result = self.selected_host().and_then(|host| {
            let operation = if install {
                Operation::Installing(Instant::now(), remote::install_agent(&host)?)
            } else {
                Operation::Checking(CatalogueCheck::start(&host)?)
            };
            Ok((host, operation))
        });
        match result {
            Ok((host, operation)) => {
                let action = if install {
                    "Installing the bridge agent on"
                } else {
                    "Reading desktop apps from"
                };
                self.message(&format!("{action} {}…", host.name()));
                self.pending = Some((host, operation));
                self.update_controls();
            }
            Err(error) => self.message(&error),
        }
    }

    pub fn tick(&mut self) -> Option<Host> {
        if self.table.selectedRow() != self.selection {
            self.selection_changed();
        }
        self.poll_runtime();
        if !self.vms.is_empty() {
            self.poll_vms();
        }
        let (host, operation) = self.pending.as_ref()?;
        let result = match operation {
            Operation::Checking(check) => check.poll()?,
            Operation::Installing(started, task) => {
                let result = task.poll().or_else(|| {
                    (started.elapsed() >= Duration::from_secs(600))
                        .then(|| Err("Timed out installing the bridge agent.".into()))
                })?;
                match result.and_then(|()| CatalogueCheck::start(host)) {
                    Ok(check) => {
                        self.message("Bridge agent installed. Checking desktop apps…");
                        self.pending = Some((host.clone(), Operation::Checking(check)));
                        return None;
                    }
                    Err(error) => Err(error),
                }
            }
        };
        let (host, _) = self.pending.take().expect("polled pending machine");
        self.update_controls();
        match result.and_then(|count| {
            host.save_to(&self.target_path)
                .map_err(|error| format!("Could not select this machine. {error}"))?;
            Ok(count)
        }) {
            Ok(count) => {
                self.message(&format!(
                    "Connected to {}. Found {count} desktop apps.",
                    host.name()
                ));
                Some(host)
            }
            Err(error) => {
                let help = match &host {
                    Host::LocalVm { .. } => {
                        " Check that the VM is running and finish Set up access."
                    }
                    Host::Ssh(_) => " Check SSH key login and host trust in Terminal.",
                };
                self.message(&format!("{error}{help}"));
                None
            }
        }
    }

    fn update_controls(&self) {
        let idle = self.pending.is_none();
        let selected = self.selected();
        let editing = self.editing.is_some();
        let vm = selected
            .as_ref()
            .is_some_and(|m| matches!(m.connection, MachineConnection::Vm { .. }));
        let remote = selected.is_some() && !vm;
        self.add_remote
            .setEnabled(idle && self.load_error.is_none());
        self.create_vm.setEnabled(idle && self.load_error.is_none());
        self.input.setHidden(!editing && !vm);
        self.input.setEnabled(idle);
        self.save.setHidden(!editing);
        self.save
            .setKeyEquivalent(&text(if editing { "\r" } else { "" }));
        self.cancel.setHidden(!editing);
        self.open_vm.setHidden(editing || !vm);
        self.setup_vm.setHidden(editing || !vm);
        self.install
            .setHidden(editing || !remote || !remote::can_install_agent());
        self.connect.setHidden(editing || selected.is_none());
        self.connect.setEnabled(idle);
        self.remove
            .setEnabled(idle && !editing && selected.is_some() && self.load_error.is_none());
        self.install.setEnabled(idle);
        self.open_vm.setEnabled(idle && vm_supported());
        self.setup_vm.setEnabled(idle && vm_supported());

        let hint = match self.editing {
            Some(NewMachine::Remote) => {
                "Enter an SSH alias or user@host. Verify the host key and set up key-based login in Terminal."
            }
            Some(NewMachine::Vm) => {
                "Name this VM. Launchpad downloads the Omarchy image if needed and starts the guest."
            }
            None if vm && !vm_supported() => {
                "Try Omarchy requires Apple silicon and macOS 15 or newer."
            }
            None if vm => "Sign in to Omarchy, then choose Set up access on first use.",
            None if remote && remote::can_install_agent() => {
                "Install the bridge agent if needed, then connect to load this machine's apps."
            }
            None if remote => {
                "Connect to load this machine's apps. The bridge agent must already be installed on the host."
            }
            None => "Add an SSH machine or create an Omarchy VM.",
        };
        let mut hint = hint.to_owned();
        if self.editing == Some(NewMachine::Vm)
            || matches!(selected.as_ref().map(|machine| &machine.connection),
                Some(MachineConnection::Vm { user, .. }) if vm_supported() && user.is_empty())
        {
            let username = mac_guest_username();
            if !username.is_empty() {
                hint.push_str(&format!(" Suggested guest username: {username}."));
            }
            if let Some(layout) = mac_keyboard_layout() {
                hint.push_str(&format!(" Mac layout: {layout}."));
            }
        }
        self.hint.setStringValue(&text(&hint));
        self.hint.setToolTip(Some(&text(&hint)));
        self.save
            .setTitle(&text(if self.editing == Some(NewMachine::Vm) {
                "Create & Open"
            } else {
                "Save Machine"
            }));
    }
}

fn default_vm_name(saved: &[Machine]) -> String {
    if saved.iter().all(|machine| machine.name != "Omarchy") {
        return "Omarchy".into();
    }
    (2..)
        .map(|number| format!("Omarchy {number}"))
        .find(|name| saved.iter().all(|machine| &machine.name != name))
        .expect("a finite list cannot use every VM name")
}

fn mac_guest_username() -> String {
    let username = NSUserName().to_string();
    let valid_start = username
        .bytes()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == b'_');
    let valid_chars = username
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c));
    if valid_start && valid_chars && username.len() <= 32 && username != "root" {
        username
    } else {
        String::new()
    }
}

fn mac_keyboard_layout() -> Option<String> {
    use std::ffi::c_void;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const NSString;
        static kTISPropertyInputSourceID: *const c_void;
        static kTISPropertyLocalizedName: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *mut c_void);
    }

    // TIS owns these CFString properties until the copied input source is released.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let id = TISGetInputSourceProperty(source, kTISPropertyInputSourceID)
            .as_ref()
            .map(ToString::to_string);
        let layout = match id.as_deref() {
            Some("com.apple.keylayout.ABC" | "com.apple.keylayout.US") => {
                Some("English (US)".into())
            }
            Some("com.apple.keylayout.British") => Some("English (UK)".into()),
            _ => TISGetInputSourceProperty(source, kTISPropertyLocalizedName)
                .as_ref()
                .map(ToString::to_string),
        };
        CFRelease(source);
        layout
    }
}

fn vm_supported() -> bool {
    if !cfg!(target_arch = "aarch64") {
        return false;
    }
    objc2_foundation::NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 15
}

fn open(args: &[impl AsRef<OsStr>]) -> Result<(), String> {
    let status = Command::new("/usr/bin/open")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if status.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&status.stderr).trim().into())
    }
}

fn shell_word(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

fn start_vm_setup(user: &str, port: u16) -> Result<(), String> {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };

    let app = std::env::current_exe().map_err(|e| e.to_string())?;
    let script = remote::resource("local-vm")
        .or_else(|| {
            app.ancestors()
                .map(|p| p.join("scripts/local-vm"))
                .find(|p| p.is_file())
        })
        .ok_or("Could not find the VM setup script.")?;

    // Terminal opens an executable command file with its own TTY for SSH and sudo.
    let dir = std::env::temp_dir().join(format!(
        "omarchy-launchpad-{}",
        remote::token().map_err(|e| e.to_string())?
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| e.to_string())?;
    let command = dir.join("Set up Omarchy.command");
    let result = (|| -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&command)
            .map_err(|e| e.to_string())?;
        writeln!(file, "#!/bin/bash\nrm -f -- \"$0\"\nrmdir -- \"$(dirname -- \"$0\")\"\n/bin/bash {} provision {} {port} {}\nresult=$?\nif [ \"$result\" -ne 0 ]; then read -r -p 'Setup failed. Press Return to close. '; fi\nexit \"$result\"",
            shell_word(&script), shell_word(Path::new(user)), shell_word(&app))
            .map_err(|e| e.to_string())?;
        drop(file);
        open(&[
            "-a",
            "Terminal",
            command.to_str().ok_or("Invalid Terminal command path.")?,
        ])
    })();
    if result.is_err() {
        let _ = fs::remove_file(&command);
        let _ = fs::remove_dir(&dir);
    }
    result
}
