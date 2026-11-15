//! AppKit objects stay on the main thread. Background work sends only text.
use crate::{
    model::{Action, Profile},
    session::{self, Sessions},
    store,
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSDistributedNotificationCenter, NSNotification,
    NSNotificationSuspensionBehavior, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, NSTimer,
};
use std::{cell::RefCell, fs, process::Command};

fn text(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

const SHOW_MANAGER: &str = "dev.app-seams.Seams.show-manager";

pub fn activate_existing() {
    // SAFETY: Fixed notification name, no payload and no credentials.
    unsafe {
        NSDistributedNotificationCenter::defaultCenter()
            .postNotificationName_object(&text(SHOW_MANAGER), None);
    }
}

struct Controller {
    window: Retained<NSWindow>,
    // NSStatusBar does not keep the status item alive for us.
    _status_item: Retained<NSStatusItem>,
    profiles: Vec<Profile>,
    selected: Option<usize>,
    picker: Retained<NSPopUpButton>,
    fields: Vec<Retained<NSTextField>>,
    status: Retained<NSTextField>,
    engine: Retained<NSTextField>,
    logs: Retained<NSTextView>,
    sessions: Sessions,
    engine_path: String,
    store_error: bool,
}

impl Controller {
    fn present(&self, mtm: MainThreadMarker) {
        self.window.makeKeyAndOrderFront(None);
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }

    fn form(&self) -> Result<Profile, String> {
        let v: Vec<String> = self
            .fields
            .iter()
            .map(|f| f.stringValue().to_string())
            .collect();
        let p = Profile {
            name: v[0].trim().into(),
            host: v[1].trim().into(),
            user: v[2].trim().into(),
            port: v[3].trim().into(),
            identity: v[4].trim().into(),
            command: v[5].clone(),
            display: v[6]
                .parse()
                .map_err(|_| "Display must be a number between 100 and 65535.")?,
        };
        p.validate()?;
        Ok(p)
    }

    fn show(&self, p: &Profile) {
        let values = [&p.name, &p.host, &p.user, &p.port, &p.identity, &p.command];
        for (f, value) in self.fields.iter().zip(values) {
            f.setStringValue(&text(value));
        }
        self.fields[6].setStringValue(&text(&p.display.to_string()));
    }

    fn refresh_picker(&self) {
        self.picker.removeAllItems();
        if self.profiles.is_empty() {
            self.picker.addItemWithTitle(&text("No saved connections"));
        }
        for p in &self.profiles {
            self.picker
                .addItemWithTitle(&text(&format!("{}  :{}", p.name, p.display)));
        }
        if let Some(i) = self.selected {
            self.picker.selectItemAtIndex(i as isize);
        }
    }

    fn persist(&mut self, next: Vec<Profile>) -> Result<(), String> {
        if self.store_error {
            return Err("Connections file could not be read. Repair or back it up before saving. See diagnostics.".into());
        }
        store::atomic_write(
            &store::directory().join("connections.tsv"),
            store::serialize(&next).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        self.profiles = next;
        Ok(())
    }

    fn save(&mut self) -> Result<Profile, String> {
        let p = self.form()?;
        if self
            .profiles
            .iter()
            .enumerate()
            .any(|(i, other)| Some(i) != self.selected && other.display == p.display)
        {
            return Err("Display number already belongs to another saved connection. Choose a different one.".into());
        }
        if self.selected.is_some_and(|i| {
            self.sessions
                .jobs
                .iter()
                .any(|j| j.display == self.profiles[i].display)
        }) {
            return Err("Disconnect the local client before editing its saved connection.".into());
        }
        let mut next = self.profiles.clone();
        let index = self.selected.unwrap_or(next.len());
        if index < next.len() {
            next[index] = p.clone();
        } else {
            next.push(p.clone());
        }
        self.persist(next)?;
        self.selected = Some(index);
        self.refresh_picker();
        Ok(p)
    }

    fn update_engine(&self) {
        let s = match session::find_xpra(&self.engine_path) {
            Some(path) => format!("Xpra ready: {}", path.display()),
            None => "Xpra not found. Install the macOS client, then choose its executable.".into(),
        };
        self.engine.setStringValue(&text(&s));
    }

    fn feedback(&mut self, message: &str) {
        self.status.setStringValue(&text(message));
        self.sessions.log(message);
        self.render_logs();
    }

    fn render_logs(&self) {
        self.logs.setString(&text(
            &self
                .sessions
                .lines
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
}

define_class!(
    // SAFETY: NSObject has no subclass invariants; all state is main-thread-only.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<Option<Controller>>]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn quit_on_close(&self, _: &NSApplication) -> bool { false }
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _: &NSApplication, _: bool) -> bool {
            self.with(|c| c.present(self.mtm()));
            true
        }
        #[unsafe(method(applicationWillTerminate:))]
        fn terminating(&self, _: &NSNotification) {
            if let Some(c) = self.ivars().borrow_mut().as_mut() { c.sessions.disconnect(None); }
        }
    }
    unsafe impl NSMenuDelegate for Delegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn update_status_menu(&self, menu: &NSMenu) {
            self.with(|c| {
                c.sessions.poll();
                populate_status_menu(menu, c, self);
            });
        }
    }
    impl Delegate {
        #[unsafe(method(showExistingManager:))]
        fn show_existing_manager(&self, _: &NSNotification) {
            self.with(|c| c.present(self.mtm()));
        }
        #[unsafe(method(manageConnections:))]
        fn manage_connections(&self, _: Option<&AnyObject>) {
            self.with(|c| c.present(self.mtm()));
        }
        #[unsafe(method(menuLaunch:))]
        fn menu_launch(&self, item: &NSMenuItem) { self.execute_saved(item.tag(), Action::Launch); }
        #[unsafe(method(menuReconnect:))]
        fn menu_reconnect(&self, item: &NSMenuItem) { self.execute_saved(item.tag(), Action::Reconnect); }
        #[unsafe(method(menuDisconnect:))]
        fn menu_disconnect(&self, item: &NSMenuItem) {
            self.with(|c| {
                if let Some(p) = c.profiles.get(item.tag() as usize) {
                    c.sessions.disconnect(Some(p.display));
                    c.render_logs();
                }
            });
        }
        #[unsafe(method(menuStop:))]
        fn menu_stop(&self, item: &NSMenuItem) {
            if confirm(self.mtm(), "Stop remote session?", "This closes every remote application on this connection's display. Unsaved work may be lost.", "Stop session") {
                self.execute_saved(item.tag(), Action::Stop);
            }
        }
        #[unsafe(method(disconnectAll:))]
        fn disconnect_all(&self, _: Option<&AnyObject>) {
            self.with(|c| { c.sessions.disconnect(None); c.render_logs(); });
        }
        #[unsafe(method(quitSeams:))]
        fn quit(&self, _: Option<&AnyObject>) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
        #[unsafe(method(save:))]
        fn save(&self, _: Option<&AnyObject>) { self.with(|c| { match c.save() { Ok(_) => c.feedback("Connection saved."), Err(e) => c.feedback(&e) } }); }

        #[unsafe(method(newConnection:))]
        fn new_connection(&self, _: Option<&AnyObject>) { self.with(|c| {
            let display = (100..=u16::MAX).find(|d| !c.profiles.iter().any(|p| p.display == *d)).unwrap_or(100);
            c.selected = None; c.show(&Profile { display, ..Profile::default() });
            c.feedback("New connection. Enter an SSH host and remote application, then save.");
        }); }

        #[unsafe(method(selectConnection:))]
        fn select_connection(&self, _: Option<&AnyObject>) { self.with(|c| {
            let index = c.picker.indexOfSelectedItem() as usize;
            if let Some(p) = c.profiles.get(index) { c.selected = Some(index); c.show(p); c.status.setStringValue(&text("Ready. Launch a new session or reconnect to an existing one.")); }
        }); }

        #[unsafe(method(deleteConnection:))]
        fn delete_connection(&self, _: Option<&AnyObject>) {
            if !confirm(self.mtm(), "Delete saved connection?", "Remote applications will keep running. Only this saved entry is removed.", "Delete") { return; }
            self.with(|c| {
                let Some(i) = c.selected else { return; };
                if c.sessions.jobs.iter().any(|j| j.display == c.profiles[i].display) { c.feedback("Disconnect this client before deleting its connection."); return; }
                let mut next = c.profiles.clone(); next.remove(i);
                if let Err(e) = c.persist(next) { c.feedback(&e); return; }
                c.selected = if c.profiles.is_empty() { None } else { Some(0) };
                c.refresh_picker(); c.show(&c.profiles.first().cloned().unwrap_or_default()); c.feedback("Saved connection deleted.");
            });
        }

        #[unsafe(method(launch:))]
        fn launch(&self, _: Option<&AnyObject>) { self.execute(Action::Launch); }
        #[unsafe(method(reconnect:))]
        fn reconnect(&self, _: Option<&AnyObject>) { self.execute(Action::Reconnect); }
        #[unsafe(method(stopSession:))]
        fn stop_session(&self, _: Option<&AnyObject>) {
            if confirm(self.mtm(), "Stop remote session?", "This closes every remote application on this connection's display. Unsaved work may be lost.", "Stop session") { self.execute(Action::Stop); }
        }
        #[unsafe(method(disconnect:))]
        fn disconnect(&self, _: Option<&AnyObject>) { self.with(|c| {
            match c.form() { Ok(p) => { c.sessions.disconnect(Some(p.display)); c.feedback("Disconnected locally. Remote apps remain available through Reconnect."); }, Err(e) => c.feedback(&e) }
        }); }
        #[unsafe(method(checkHost:))]
        fn check_host(&self, _: Option<&AnyObject>) { self.with(|c| {
            let result = c.form().and_then(|p| {
                if c.sessions.jobs.iter().any(|j| j.check && j.display == p.display) { return Err("SSH check already running.".into()); }
                c.sessions.spawn(p.check_command(), &p, true)
            });
            match result { Ok(()) => c.feedback("Checking SSH access and remote X11 prerequisites…"), Err(e) => c.feedback(&e) }
        }); }
        #[unsafe(method(chooseEngine:))]
        fn choose_engine(&self, _: Option<&AnyObject>) {
            let panel = NSOpenPanel::openPanel(self.mtm()); panel.setCanChooseDirectories(false); panel.setCanChooseFiles(true);
            panel.setTreatsFilePackagesAsDirectories(true); panel.setTitle(Some(&text("Choose the xpra executable inside Xpra.app/Contents/MacOS")));
            if panel.runModal() == NSModalResponseOK
                && let Some(path) = panel.URL().and_then(|u| u.path()) { self.with(|c| {
                    let path = path.to_string();
                    if !std::path::Path::new(&path).is_file() { c.feedback("Select an executable file."); return; }
                    match store::atomic_write(&store::directory().join("engine-path"), path.as_bytes()) {
                        Ok(()) => { c.engine_path = path; c.update_engine(); c.feedback("Rendering engine path saved."); }, Err(e) => c.feedback(&e.to_string())
                    }
                });
            }
        }
        #[unsafe(method(installHelp:))]
        fn install_help(&self, _: Option<&AnyObject>) { let _ = Command::new("/usr/bin/open").arg("https://github.com/Xpra-org/xpra/wiki/Download").spawn(); }
        #[unsafe(method(tick:))]
        fn tick(&self, _: Option<&AnyObject>) {
            // Development watcher requests a normal AppKit quit so local
            // Xpra/SSH process groups are disconnected before restarting.
            #[cfg(debug_assertions)]
            if std::env::var_os("SEAMS_DEV_RELOAD_FILE")
                .is_some_and(|p| std::path::Path::new(&p).exists()) {
                NSApplication::sharedApplication(self.mtm()).terminate(None);
                return;
            }
            self.with(|c| {
            if c.sessions.poll() { c.render_logs();
                if let Some(last) = c.sessions.lines.back() { c.status.setStringValue(&text(last)); }
            }
            c.update_engine();
        }); }
    }
);

impl Delegate {
    fn execute_saved(&self, index: isize, action: Action) {
        self.with(|c| {
            let Some(p) = c.profiles.get(index as usize).cloned() else {
                return;
            };
            let result = session::find_xpra(&c.engine_path)
                .ok_or_else(|| {
                    "Install the macOS Xpra client, or choose its executable first.".to_string()
                })
                .and_then(|xpra| c.sessions.start(&xpra, &p, action));
            match result {
                Ok(()) => c.feedback(&format!(
                    "{}: client started. See diagnostics for connection progress.",
                    p.name
                )),
                Err(e) => {
                    c.feedback(&e);
                    c.present(self.mtm());
                }
            }
        });
    }

    fn with(&self, f: impl FnOnce(&mut Controller)) {
        if let Some(c) = self.ivars().borrow_mut().as_mut() {
            f(c);
        }
    }
    fn execute(&self, action: Action) {
        self.with(|c| {
            let result = (|| {
                let p = c.form()?;
                if c.selected.is_none() || c.selected.is_some_and(|i| c.profiles[i] != p) {
                    c.save()?;
                }
                let xpra = session::find_xpra(&c.engine_path)
                    .ok_or("Install the macOS Xpra client, or choose its executable first.")?;
                c.sessions.start(&xpra, &p, action)
            })();
            match result {
                Ok(()) => {
                    c.feedback("Client started. Connection progress appears in diagnostics below.")
                }
                Err(e) => c.feedback(&e),
            }
        });
    }
}

fn confirm(mtm: MainThreadMarker, title: &str, detail: &str, action: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&text(title));
    alert.setInformativeText(&text(detail));
    alert.addButtonWithTitle(&text("Cancel"));
    alert.addButtonWithTitle(&text(action));
    alert.runModal() == NSAlertSecondButtonReturn
}

fn label(
    view: &NSView,
    mtm: MainThreadMarker,
    s: &str,
    frame: NSRect,
    size: f64,
) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&text(s), mtm);
    l.setFrame(frame);
    l.setFont(Some(&NSFont::systemFontOfSize(size)));
    view.addSubview(&l);
    l
}

fn button(
    view: &NSView,
    mtm: MainThreadMarker,
    target: &Delegate,
    title: &str,
    action: Sel,
    frame: NSRect,
) -> Retained<NSButton> {
    // SAFETY: Each action below is declared on Delegate with one object parameter.
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(&text(title), Some(target), Some(action), mtm)
    };
    b.setFrame(frame);
    view.addSubview(&b);
    b
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("AppKit requires the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let delegate: Retained<Delegate> = unsafe {
        msg_send![
            super(Delegate::alloc(mtm).set_ivars(RefCell::new(None))),
            init
        ]
    };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let notifications = NSDistributedNotificationCenter::defaultCenter();
    // SAFETY: Delegate handles the selector's notification argument and is
    // retained until the observer is removed below. Delivery uses the main loop.
    unsafe {
        notifications.addObserver_selector_name_object_suspensionBehavior(
            &delegate,
            sel!(showExistingManager:),
            Some(&text(SHOW_MANAGER)),
            None,
            NSNotificationSuspensionBehavior::DeliverImmediately,
        );
    }
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    build_menu(&app, mtm);
    // SAFETY: Main thread, valid dimensions. Auto-release-on-close is disabled.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0., 0., 940., 780.),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    window.setTitle(&text("Seams"));
    window.center();
    let v = window.contentView().unwrap();
    label(&v, mtm, "Seams", rect(28., 710., 205., 42.), 34.);
    label(
        &v,
        mtm,
        "Remote apps. Local windows.",
        rect(262., 725., 640., 26.),
        21.,
    );
    label(&v, mtm, "Connections", rect(28., 655., 200., 24.), 15.);
    let picker = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        rect(24., 615., 210., 32.),
        false,
    );
    unsafe {
        picker.setTarget(Some(&delegate));
        picker.setAction(Some(sel!(selectConnection:)));
        v.addSubview(&picker);
    }
    for (title, action, y) in [
        ("New connection", sel!(newConnection:), 566.),
        ("Save connection", sel!(save:), 522.),
        ("Delete connection…", sel!(deleteConnection:), 478.),
    ] {
        button(&v, mtm, &delegate, title, action, rect(24., y, 210., 34.));
    }
    label(
        &v,
        mtm,
        "Save edits before switching.\n\nSSH config aliases and keys\nwork without copying secrets\ninto Seams.",
        rect(28., 310., 206., 140.),
        12.,
    );
    let mut fields = Vec::new();
    for (i, (name, hint)) in [
        ("Name", "Work Linux"),
        ("SSH host or alias", "omarchy or 192.0.2.10"),
        ("User", "From SSH config if empty"),
        ("Port", "From SSH config if empty"),
        ("Identity file", "/Users/you/.ssh/id_ed25519, or empty"),
        ("Application command", "firefox --no-remote"),
        ("Session display", "100"),
    ]
    .iter()
    .enumerate()
    {
        let y = 665. - i as f64 * 46.;
        label(&v, mtm, name, rect(266., y, 158., 22.), 13.);
        let field = NSTextField::textFieldWithString(&text(""), mtm);
        field.setFrame(rect(426., y - 2., 480., 27.));
        field.setPlaceholderString(Some(&text(hint)));
        v.addSubview(&field);
        fields.push(field);
    }
    let launch = button(
        &v,
        mtm,
        &delegate,
        "Launch app",
        sel!(launch:),
        rect(262., 329., 150., 36.),
    );
    launch.setKeyEquivalent(&text("\r"));
    button(
        &v,
        mtm,
        &delegate,
        "Reconnect",
        sel!(reconnect:),
        rect(419., 329., 150., 36.),
    );
    button(
        &v,
        mtm,
        &delegate,
        "Disconnect",
        sel!(disconnect:),
        rect(576., 329., 150., 36.),
    );
    button(
        &v,
        mtm,
        &delegate,
        "Stop session…",
        sel!(stopSession:),
        rect(733., 329., 178., 36.),
    );
    label(
        &v,
        mtm,
        "Launch creates an X11 session. Reconnect returns to its running apps.",
        rect(266., 296., 640., 22.),
        12.,
    );
    button(
        &v,
        mtm,
        &delegate,
        "Check SSH host",
        sel!(checkHost:),
        rect(24., 258., 210., 32.),
    );
    button(
        &v,
        mtm,
        &delegate,
        "Get Xpra…",
        sel!(installHelp:),
        rect(262., 258., 130., 32.),
    );
    button(
        &v,
        mtm,
        &delegate,
        "Choose executable…",
        sel!(chooseEngine:),
        rect(400., 258., 190., 32.),
    );
    let engine = label(&v, mtm, "", rect(28., 223., 878., 23.), 11.);
    let status = label(
        &v,
        mtm,
        "Add a connection to begin. Check SSH access before launching.",
        rect(28., 190., 878., 25.),
        12.,
    );
    label(&v, mtm, "Diagnostics", rect(28., 159., 878., 24.), 13.);
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(28., 24., 878., 132.));
    let logs = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0., 0., 878., 132.));
    logs.setEditable(false);
    logs.setSelectable(true);
    logs.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(11., 0.)));
    logs.setTextContainerInset(NSSize::new(8., 8.));
    scroll.setHasVerticalScroller(true);
    scroll.setDocumentView(Some(&logs));
    v.addSubview(&scroll);
    let loaded = store::load(&store::directory().join("connections.tsv"));
    let store_error = loaded.is_err();
    let mut sessions = Sessions::default();
    let profiles = loaded.unwrap_or_else(|e| {
        sessions.log(format!("Saved connections could not be loaded: {e}"));
        Vec::new()
    });
    let selected = if profiles.is_empty() { None } else { Some(0) };
    let engine_path =
        fs::read_to_string(store::directory().join("engine-path")).unwrap_or_default();
    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status_item.button(mtm) {
        button.setTitle(&text("Seams"));
        button.setToolTip(Some(&text(
            "Launch remote applications and manage SSH connections",
        )));
    }
    let status_menu = NSMenu::new(mtm);
    status_menu.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    status_item.setMenu(Some(&status_menu));
    let c = Controller {
        window,
        _status_item: status_item,
        profiles,
        selected,
        picker,
        fields,
        status,
        engine,
        logs,
        sessions,
        engine_path,
        store_error,
    };
    c.show(&c.profiles.first().cloned().unwrap_or_default());
    c.refresh_picker();
    c.update_engine();
    c.render_logs();
    populate_status_menu(&status_menu, &c, &delegate);
    // First launch needs setup; ordinary launches stay in the menu bar.
    if c.profiles.is_empty() || c.store_error {
        c.present(mtm);
    }
    *delegate.ivars().borrow_mut() = Some(c);
    // SAFETY: tick: accepts one object; delegate remains alive for the run loop.
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.25,
            &delegate,
            sel!(tick:),
            None,
            true,
        )
    };
    app.run();
    unsafe {
        notifications.removeObserver_name_object(&delegate, Some(&text(SHOW_MANAGER)), None);
    }
    timer.invalidate();
    delegate.ivars().borrow_mut().take();
}

fn status_action(
    menu: &NSMenu,
    target: &Delegate,
    title: &str,
    action: Sel,
    index: isize,
    enabled: bool,
) -> Retained<NSMenuItem> {
    // SAFETY: Call sites use Delegate actions with a single object argument.
    let item = unsafe {
        menu.addItemWithTitle_action_keyEquivalent(&text(title), Some(action), &text(""))
    };
    unsafe {
        item.setTarget(Some(target));
    }
    item.setTag(index);
    item.setEnabled(enabled);
    item
}

fn populate_status_menu(menu: &NSMenu, c: &Controller, target: &Delegate) {
    let mtm = target.mtm();
    menu.removeAllItems();
    menu.setAutoenablesItems(false);
    let heading = NSMenuItem::new(mtm);
    heading.setTitle(&text(&format!(
        "Seams · {} local processes",
        c.sessions.jobs.len()
    )));
    heading.setEnabled(false);
    menu.addItem(&heading);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    if c.profiles.is_empty() {
        let empty = NSMenuItem::new(mtm);
        empty.setTitle(&text("No saved connections"));
        empty.setEnabled(false);
        menu.addItem(&empty);
    }
    for (index, p) in c.profiles.iter().enumerate() {
        let parent = NSMenuItem::new(mtm);
        parent.setTitle(&text(&format!("{} ({})", p.name, p.host)));
        let submenu = NSMenu::new(mtm);
        submenu.setAutoenablesItems(false);
        let running = c
            .sessions
            .jobs
            .iter()
            .any(|j| j.display == p.display && !j.check);
        status_action(
            &submenu,
            target,
            "Launch app",
            sel!(menuLaunch:),
            index as isize,
            !running,
        );
        status_action(
            &submenu,
            target,
            "Reconnect",
            sel!(menuReconnect:),
            index as isize,
            !running,
        );
        status_action(
            &submenu,
            target,
            "Disconnect",
            sel!(menuDisconnect:),
            index as isize,
            running,
        );
        submenu.addItem(&NSMenuItem::separatorItem(mtm));
        status_action(
            &submenu,
            target,
            "Stop session…",
            sel!(menuStop:),
            index as isize,
            !running,
        );
        parent.setSubmenu(Some(&submenu));
        menu.addItem(&parent);
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    status_action(
        menu,
        target,
        "Manage connections…",
        sel!(manageConnections:),
        0,
        true,
    )
    .setKeyEquivalent(&text(","));
    if session::find_xpra(&c.engine_path).is_none() {
        status_action(menu, target, "Get Xpra…", sel!(installHelp:), 0, true);
    }
    status_action(
        menu,
        target,
        "Disconnect all",
        sel!(disconnectAll:),
        0,
        !c.sessions.jobs.is_empty(),
    );
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    status_action(menu, target, "Quit Seams", sel!(quitSeams:), 0, true)
        .setKeyEquivalent(&text("q"));
}

fn build_menu(app: &NSApplication, mtm: MainThreadMarker) {
    let bar = NSMenu::new(mtm);
    for (title, entries) in [
        ("Seams", vec![("Quit Seams", sel!(terminate:), "q")]),
        (
            "Edit",
            vec![
                ("Undo", sel!(undo:), "z"),
                ("Cut", sel!(cut:), "x"),
                ("Copy", sel!(copy:), "c"),
                ("Paste", sel!(paste:), "v"),
                ("Select All", sel!(selectAll:), "a"),
            ],
        ),
    ] {
        let parent = NSMenuItem::new(mtm);
        parent.setTitle(&text(title));
        let menu = NSMenu::new(mtm);
        for (label, action, key) in entries {
            unsafe {
                menu.addItemWithTitle_action_keyEquivalent(&text(label), Some(action), &text(key));
            }
        }
        parent.setSubmenu(Some(&menu));
        bar.addItem(&parent);
    }
    app.setMainMenu(Some(&bar));
}
