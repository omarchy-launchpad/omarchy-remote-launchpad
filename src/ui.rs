// AppKit launchpad, menu bar and launch lifecycle.
use crate::{config::Host, launcher::Launcher, remote, viewer::Viewer};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSDistributedNotificationCenter, NSNotificationSuspensionBehavior, NSObject,
    NSObjectProtocol, NSString, NSTimer,
};
use std::cell::RefCell;

fn text(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}
const SHOW_MANAGER: &str = "dev.app-seams.Seams.show-manager";

pub fn activate_existing() {
    // SAFETY: Fixed notification name, no payload and no credentials.
    unsafe {
        NSDistributedNotificationCenter::defaultCenter()
            .postNotificationName_object(&text(SHOW_MANAGER), None);
    }
}

struct Launch {
    host: Host,
    app: remote::App,
    token: String,
    watch: Option<remote::Watch>,
    views: Vec<Viewer>,
}

struct Controller {
    launcher: Launcher,
    launches: Vec<Launch>,
    _status_item: Retained<NSStatusItem>,
}

impl Controller {
    fn launch(&mut self) -> Result<(), String> {
        let Some(app) = self.launcher.selection() else {
            return Ok(());
        };
        let Some(host) = self.launcher.host.clone() else {
            return Ok(());
        };
        let token = remote::token().map_err(|e| e.to_string())?;
        let watch = remote::watch(&host, &token, &app)?;
        self.launcher.set_status(&format!("Opening {}…", app.name));
        self.launches.push(Launch {
            host,
            app,
            token,
            watch: Some(watch),
            views: Vec::new(),
        });
        Ok(())
    }

    fn tick(&mut self, mtm: MainThreadMarker) {
        self.launcher.tick();
        let mut errors = Vec::new();
        self.launches.retain_mut(|launch| {
            if let Some(watch) = &launch.watch {
                let completed = watch.task.poll();
                for xid in watch.windows.try_iter() {
                    if launch.views.len() >= 32 {
                        errors.push("Window limit reached for this launch.".into());
                        break;
                    }
                    match remote::stream(&launch.host, &launch.token, xid, &launch.app) {
                        Ok(stream) => launch.views.push(Viewer::new(mtm, stream)),
                        Err(e) => errors.push(e),
                    }
                }
                if let Some(done) = completed {
                    if let Err(e) = done {
                        errors.push(format!("{}: {e}", launch.app.name));
                    }
                    launch.watch = None;
                }
            }
            launch.views.retain_mut(|view| {
                let showing = view.is_visible();
                match view.tick() {
                    Ok(keep) => {
                        if !showing && view.is_visible() {
                            self.launcher
                                .set_status(&format!("Opened {}", launch.app.name));
                        }
                        keep
                    }
                    Err(e) => {
                        errors.push(format!("{}: {e}", launch.app.name));
                        false
                    }
                }
            });
            launch.watch.is_some() || !launch.views.is_empty()
        });
        if !errors.is_empty() {
            for error in errors {
                self.launcher.error(&error);
            }
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
            self.with(|c| c.launcher.present());
            true
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
        fn show(&self, _: Option<&AnyObject>) { self.with(|c| c.launcher.present()); }
        #[unsafe(method(chooseApp:))]
        fn choose(&self, _: Option<&AnyObject>) {
            self.with(|c| if let Err(e) = c.launch() { c.launcher.error(&e); });
        }
        #[unsafe(method(startLocalVM:))]
        fn start_local_vm(&self, _: Option<&AnyObject>) {
            let result = std::process::Command::new("/usr/bin/open")
                .args(["-b", "dev.tryomarchy.native"]).status();
            self.with(|c| {
                c.launcher.present();
                if result.is_ok_and(|status| status.success()) {
                    c.launcher.set_status("Launch Omarchy, log in, then press Refresh.");
                } else {
                    c.launcher.error("Install Try Omarchy on an Apple silicon Mac with macOS 15 or newer.");
                }
            });
        }
        #[unsafe(method(refresh:))]
        fn refresh(&self, _: Option<&AnyObject>) {
            self.with(|c| c.launcher.refresh(Host::load()));
        }
        #[unsafe(method(tick:))]
        fn tick(&self, _: Option<&AnyObject>) {
            #[cfg(debug_assertions)]
            if std::env::var_os("SEAMS_DEV_RELOAD_FILE")
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
    let delegate = setup(mtm, Host::load());
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
    app.run();
    unsafe {
        notifications.removeObserver_name_object(&delegate, Some(&text(SHOW_MANAGER)), None);
    }
    timer.invalidate();
    delegate.ivars().borrow_mut().take();
}

fn setup(mtm: MainThreadMarker, host: Result<Host, String>) -> Retained<Delegate> {
    let app = NSApplication::sharedApplication(mtm);
    let delegate: Retained<Delegate> = unsafe {
        msg_send![
            super(Delegate::alloc(mtm).set_ivars(RefCell::new(None))),
            init
        ]
    };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    build_menu(&app, mtm);

    let mut launcher = Launcher::new(mtm, &delegate);
    unsafe {
        launcher
            .search
            .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    launcher.refresh(host);
    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status.button(mtm) {
        button.setTitle(&text("Seams"));
    }
    let menu = NSMenu::new(mtm);
    unsafe {
        let open = menu.addItemWithTitle_action_keyEquivalent(
            &text("Show Launchpad"),
            Some(sel!(showExistingManager:)),
            &text(""),
        );
        open.setTarget(Some(&delegate));
        let vm = menu.addItemWithTitle_action_keyEquivalent(
            &text("Open Local VM…"),
            Some(sel!(startLocalVM:)),
            &text(""),
        );
        vm.setTarget(Some(&delegate));
        menu.addItemWithTitle_action_keyEquivalent(
            &text("Quit Seams"),
            Some(sel!(terminate:)),
            &text("q"),
        );
    }
    status.setMenu(Some(&menu));
    *delegate.ivars().borrow_mut() = Some(Controller {
        launcher,
        launches: Vec::new(),
        _status_item: status,
    });
    delegate
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
