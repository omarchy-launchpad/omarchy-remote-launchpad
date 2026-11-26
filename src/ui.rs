// AppKit launchpad, menu bar and launch lifecycle.
use crate::{
    config::Host,
    dock_apps,
    launcher::{Launcher, rect},
    machines::{Machines, NewMachine},
    remote::{self, Connection, Reachability},
    unlock::LockPrompt,
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSDistributedNotificationCenter, NSNotificationSuspensionBehavior, NSObject,
    NSObjectProtocol, NSSize, NSString, NSTimer,
};
use std::cell::RefCell;

fn text(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn omarchy_icon() -> Retained<NSImage> {
    // Source: https://github.com/omacom/omarchy/blob/master/icon.png
    dock_apps::icon_image(dock_apps::FALLBACK_ICON).expect("bundled Omarchy icon is valid PNG")
}

fn menu_label(
    mtm: MainThreadMarker,
    value: &str,
    frame: objc2_foundation::NSRect,
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&text(value), mtm);
    label.setFrame(frame);
    label.setFont(Some(font));
    label.setTextColor(Some(color));
    label.setUsesSingleLineMode(true);
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    label
}

struct StatusMenu {
    connection: Retained<NSTextField>,
    host: Retained<NSTextField>,
    open: Retained<NSMenuItem>,
    unlock: Retained<NSMenuItem>,
}

impl StatusMenu {
    fn update(&self, host: Option<&Host>, connection: Connection) {
        let (label, color) = match (host, connection) {
            (None, _) => ("Not configured", NSColor::secondaryLabelColor()),
            (_, Connection::Checking) => ("Checking…", NSColor::secondaryLabelColor()),
            (_, Connection::Online) => ("Running", NSColor::systemGreenColor()),
            (_, Connection::Offline) => ("Offline", NSColor::secondaryLabelColor()),
        };
        self.connection
            .setStringValue(&text(&format!("●  {label}")));
        self.connection.setTextColor(Some(&color));
        self.host
            .setStringValue(&text(host.map_or("Not configured", Host::name)));
        self.open
            .setEnabled(host.is_none() || connection == Connection::Online);
        self.unlock.setEnabled(connection == Connection::Online);
    }
}

fn status_menu(mtm: MainThreadMarker, delegate: &Delegate) -> (Retained<NSMenu>, StatusMenu) {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);

    let header_item = NSMenuItem::new(mtm);
    let header = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., 260., 76.));
    let icon = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(16., 14., 48., 48.));
    let image = omarchy_icon();
    image.setSize(NSSize::new(48., 48.));
    icon.setImage(Some(&image));
    let title = menu_label(
        mtm,
        "Omarchy",
        rect(80., 40., 164., 24.),
        &NSFont::boldSystemFontOfSize(17.),
        &NSColor::labelColor(),
    );
    let connection = menu_label(
        mtm,
        "",
        rect(80., 17., 164., 20.),
        &NSFont::systemFontOfSize(14.),
        &NSColor::secondaryLabelColor(),
    );
    header.addSubview(&icon);
    header.addSubview(&title);
    header.addSubview(&connection);
    header_item.setView(Some(&header));
    menu.addItem(&header_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let details_item = NSMenuItem::new(mtm);
    let details = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., 260., 50.));
    let primary = NSColor::labelColor();
    let secondary = NSColor::secondaryLabelColor();
    let detail_font = NSFont::systemFontOfSize(13.);
    for (label, y) in [("Omarchy", 27.), ("Host", 7.)] {
        details.addSubview(&menu_label(
            mtm,
            label,
            rect(16., y, 76., 18.),
            &detail_font,
            &secondary,
        ));
    }
    details.addSubview(&menu_label(
        mtm,
        &format!("{} · Preview", env!("CARGO_PKG_VERSION")),
        rect(104., 27., 140., 18.),
        &detail_font,
        &primary,
    ));
    let host = menu_label(mtm, "", rect(104., 7., 140., 18.), &detail_font, &primary);
    details.addSubview(&host);
    details_item.setView(Some(&details));
    menu.addItem(&details_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let (open, unlock) = unsafe {
        let open = menu.addItemWithTitle_action_keyEquivalent(
            &text("Show Launchpad…"),
            Some(sel!(showExistingManager:)),
            &text(""),
        );
        open.setTarget(Some(delegate));
        let manage = menu.addItemWithTitle_action_keyEquivalent(
            &text("Manage Machines"),
            Some(sel!(manageMachines:)),
            &text(""),
        );
        manage.setTarget(Some(delegate));
        let unlock = menu.addItemWithTitle_action_keyEquivalent(
            &text("Unlock Omarchy…"),
            Some(sel!(unlockOmarchy:)),
            &text(""),
        );
        unlock.setTarget(Some(delegate));
        (open, unlock)
    };
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    unsafe {
        menu.addItemWithTitle_action_keyEquivalent(
            &text("Quit Omarchy"),
            Some(sel!(terminate:)),
            &text("q"),
        );
    }
    (
        menu,
        StatusMenu {
            connection,
            host,
            open,
            unlock,
        },
    )
}

const SHOW_MANAGER: &str = "dev.app-omarchy-launchpad.omarchy-launchpad.show-manager";

pub fn activate_existing() {
    // SAFETY: Fixed notification name, no payload and no credentials.
    unsafe {
        NSDistributedNotificationCenter::defaultCenter()
            .postNotificationName_object(&text(SHOW_MANAGER), None);
    }
}

struct Controller {
    launcher: Launcher,
    machines: Machines,
    reachability: Reachability,
    status_menu: StatusMenu,
    open_app: fn(&Host, &remote::App) -> Result<(), String>,
    lock_prompt: LockPrompt,
    show_launchpad_after_prompt: bool,
    _status_item: Retained<NSStatusItem>,
}

impl Controller {
    fn configure(&mut self, host: Result<Host, String>) {
        self.machines.set_current(host.as_ref().ok());
        self.launcher.configure(host);
        self.reachability = Reachability::new();
        self.status_menu
            .update(self.launcher.host.as_ref(), self.reachability.state());
    }

    fn can_show_launchpad(&self) -> bool {
        self.launcher.host.is_none() || self.reachability.state() == Connection::Online
    }

    fn show_launchpad(&mut self) {
        if !self.can_show_launchpad() {
            return;
        }
        self.lock_prompt.dismiss();
        self.show_launchpad_after_prompt = false;
        self.launcher.present();
    }

    fn reopen(&mut self) {
        if self.can_show_launchpad() {
            self.launcher.present();
            self.lock_prompt.resume();
        }
    }

    fn launch(&mut self) -> Result<(), String> {
        if self.reachability.state() != Connection::Online {
            return Ok(());
        }
        let Some(app) = self.launcher.selection() else {
            return Ok(());
        };
        let Some(host) = self.launcher.host.as_ref() else {
            return Ok(());
        };
        (self.open_app)(host, &app)?;
        self.launcher
            .set_status(&format!("Opened {} viewer.", app.name));
        Ok(())
    }

    fn tick(&mut self, mtm: MainThreadMarker) {
        if let Some(host) = self.machines.tick() {
            self.configure(Ok(host));
        }
        if let Some(host) = &self.launcher.host {
            let previous = self.reachability.state();
            let connection = self.reachability.tick(host);
            if connection != previous {
                self.status_menu.update(Some(host), connection);
                match connection {
                    Connection::Online => self.launcher.load_apps(),
                    Connection::Offline => {
                        self.show_launchpad_after_prompt = false;
                        self.lock_prompt.dismiss();
                        self.launcher.clear_apps("Omarchy is offline.");
                        self.launcher.window.orderOut(None);
                    }
                    Connection::Checking => {}
                }
            }
        }
        self.launcher.tick();
        let connected_host = self
            .launcher
            .host
            .as_ref()
            .filter(|_| self.reachability.state() == Connection::Online);
        if let Err(error) = self.lock_prompt.tick(mtm, connected_host) {
            self.launcher.error(&error);
            self.show_launchpad_after_prompt = self.can_show_launchpad();
        }
        if self.lock_prompt.is_visible() {
            if self.launcher.window.isVisible() {
                self.show_launchpad_after_prompt = true;
                self.launcher.window.orderOut(None);
            }
        } else if self.can_show_launchpad() && std::mem::take(&mut self.show_launchpad_after_prompt)
        {
            self.launcher.present();
        }
    }
}

define_class!(
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
            self.with(Controller::reopen);
            false
        }
    }
    unsafe impl NSTextFieldDelegate for Delegate {}
    unsafe impl NSControlTextEditingDelegate for Delegate {
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, _: &NSControl, _: &NSTextView, selector: Sel) -> bool {
            if selector == sel!(insertNewline:) {
                self.with(|c| if let Err(e) = c.launch() { c.launcher.error(&e); });
                true
            } else if selector == sel!(moveDown:) || selector == sel!(moveUp:) {
                self.with(|c| c.launcher.move_selection(if selector == sel!(moveDown:) { 1 } else { -1 }));
                true
            } else { false }
        }
    }
    impl Delegate {
        #[unsafe(method(showExistingManager:))]
        fn show(&self, _: Option<&AnyObject>) { self.with(Controller::show_launchpad); }
        #[unsafe(method(chooseApp:))]
        fn choose(&self, _: Option<&AnyObject>) {
            self.with(|c| if let Err(e) = c.launch() { c.launcher.error(&e); });
        }
        #[unsafe(method(manageMachines:))]
        fn manage_machines(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.present()); }
        #[unsafe(method(addRemoteMachine:))]
        fn add_remote(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.begin_add(NewMachine::Remote)); }
        #[unsafe(method(createLocalVM:))]
        fn create_vm(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.begin_add(NewMachine::Vm)); }
        #[unsafe(method(saveMachine:))]
        fn save_machine(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.save_new()); }
        #[unsafe(method(cancelMachine:))]
        fn cancel_machine(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.cancel_add()); }
        #[unsafe(method(removeMachine:))]
        fn remove_machine(&self, _: Option<&AnyObject>) {
            self.with(|c| {
                if c.machines.remove_selected() {
                    c.lock_prompt.dismiss();
                    c.show_launchpad_after_prompt = false;
                    c.configure(Err("No machine selected.".into()));
                }
            });
        }
        #[unsafe(method(connectMachine:))]
        fn connect_machine(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.connect(false)); }
        #[unsafe(method(installRemoteAgent:))]
        fn install_remote(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.connect(true)); }
        #[unsafe(method(setupLocalVM:))]
        fn setup_vm(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.setup_selected_vm()); }
        #[unsafe(method(startLocalVM:))]
        fn start_vm(&self, _: Option<&AnyObject>) { self.with(|c| c.machines.open_selected_vm()); }
        #[unsafe(method(refresh:))]
        fn refresh(&self, _: Option<&AnyObject>) {
            self.with(|c| c.configure(c.machines.load_current()));
        }
        #[unsafe(method(unlockOmarchy:))]
        fn unlock_omarchy(&self, _: Option<&AnyObject>) {
            self.with(|c| {
                if c.reachability.state() == Connection::Online {
                    c.lock_prompt.request();
                }
            });
        }
        #[unsafe(method(lockOmarchy:))]
        fn lock_omarchy(&self, _: Option<&AnyObject>) {
            self.with(|c| {
                let result = match c.launcher.host.as_ref() {
                    None => Err("Configure an Omarchy host first.".into()),
                    Some(host) if c.reachability.state() == Connection::Online => c.lock_prompt.lock(host),
                    Some(_) => Err("Omarchy is unavailable.".into()),
                };
                if let Err(error) = result {
                    c.launcher.error(&error);
                }
            });
        }
        #[unsafe(method(tick:))]
        fn tick(&self, _: Option<&AnyObject>) {
            #[cfg(debug_assertions)]
            if std::env::var_os("OMARCHY_LAUNCHPAD_DEV_RELOAD_FILE")
                .is_some_and(|p| std::path::Path::new(&p).exists()) {
                NSApplication::sharedApplication(self.mtm()).terminate(None);
                return;
            }
            self.with(|c| c.tick(self.mtm()));
        }
    }
);

impl Delegate {
    fn with(&self, f: impl FnOnce(&mut Controller)) {
        if let Some(c) = self.ivars().borrow_mut().as_mut() {
            f(c);
        }
    }
}
pub fn run() {
    let mtm = MainThreadMarker::new().expect("AppKit requires the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let target_path = Host::path();
    let delegate = setup(
        mtm,
        Host::load_from(&target_path),
        dock_apps::open,
        target_path,
    );
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
    // SAFETY: tick: accepts one object; delegate remains alive for the run loop.
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            1.0 / 60.0,
            &delegate,
            sel!(tick:),
            None,
            true,
        )
    };
    unsafe {
        objc2_foundation::NSRunLoop::mainRunLoop()
            .addTimer_forMode(&timer, objc2_foundation::NSRunLoopCommonModes)
    };
    delegate.with(|c| {
        if c.launcher.host.is_none() {
            c.show_launchpad_after_prompt = false;
            c.machines.present();
        }
    });
    app.run();
    unsafe {
        notifications.removeObserver_name_object(&delegate, Some(&text(SHOW_MANAGER)), None);
    }
    timer.invalidate();
    delegate.ivars().borrow_mut().take();
}

fn setup(
    mtm: MainThreadMarker,
    host: Result<Host, String>,
    open_app: fn(&Host, &remote::App) -> Result<(), String>,
    target_path: std::path::PathBuf,
) -> Retained<Delegate> {
    let app = NSApplication::sharedApplication(mtm);
    let delegate: Retained<Delegate> = unsafe {
        msg_send![
            super(Delegate::alloc(mtm).set_ivars(RefCell::new(None))),
            init
        ]
    };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let app_icon = omarchy_icon();
    // SAFETY: Called on the main thread during application setup.
    unsafe { app.setApplicationIconImage(Some(&app_icon)) };
    build_menu(&app, mtm);

    let launcher = Launcher::new(mtm, &delegate);
    unsafe {
        launcher
            .search
            .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    let (menu, status_menu) = status_menu(mtm, &delegate);
    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status.button(mtm) {
        let status_icon = omarchy_icon();
        status_icon.setSize(NSSize::new(18., 18.));
        button.setImage(Some(&status_icon));
        button.setTitle(&text(""));
        button.setToolTip(Some(&text("Omarchy")));
    }
    status.setMenu(Some(&menu));
    let mut controller = Controller {
        launcher,
        machines: Machines::new(mtm, &delegate, target_path),
        reachability: Reachability::new(),
        status_menu,
        open_app,
        lock_prompt: LockPrompt::new(),
        show_launchpad_after_prompt: true,
        _status_item: status,
    };
    controller.configure(host);
    *delegate.ivars().borrow_mut() = Some(controller);
    delegate
}
fn build_menu(app: &NSApplication, mtm: MainThreadMarker) {
    let bar = NSMenu::new(mtm);
    for (title, entries) in [
        (
            "omarchy-launchpad",
            vec![("Quit omarchy-launchpad", sel!(terminate:), "q")],
        ),
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
