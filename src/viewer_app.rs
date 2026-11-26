//! AppKit lifecycle for one remote app and all of its captured windows.
use crate::{
    config::Host,
    dock_apps::icon_image,
    remote::{self, App, Watch},
    viewer::Viewer,
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
    sel,
};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu,
    NSMenuItem,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes,
    NSString, NSTimer,
};
use std::cell::RefCell;

pub struct Session {
    pub(crate) host: Host,
    pub(crate) app: App,
    token: String,
    watch: Option<Watch>,
    views: Vec<Viewer>,
}

impl Session {
    pub fn start(host: Host, app: App) -> Result<Self, String> {
        let token = remote::token().map_err(|e| e.to_string())?;
        let watch = remote::watch(&host, &token, &app)?;
        Ok(Self {
            host,
            app,
            token,
            watch: Some(watch),
            views: Vec::new(),
        })
    }

    pub fn reopen(&self) {
        if let Some(view) = self.views.iter().rev().find(|view| view.is_visible()) {
            view.present();
        } else if let Some(view) = self.views.iter().rev().find(|view| view.is_minimized()) {
            view.restore();
        }
    }

    pub fn tick(&mut self, mtm: MainThreadMarker, errors: &mut Vec<String>) -> bool {
        let mut had_views = !self.views.is_empty();
        if let Some(watch) = &self.watch {
            let completed = watch.task.poll();
            for xid in watch.windows.try_iter() {
                if self.views.len() >= 32 {
                    errors.push("Window limit reached for this launch.".into());
                    break;
                }
                match remote::stream(&self.host, &self.token, xid, &self.app) {
                    Ok(stream) => {
                        self.views.push(Viewer::new(mtm, stream));
                        had_views = true;
                    }
                    Err(e) => errors.push(e),
                }
            }
            if let Some(done) = completed {
                if let Err(e) = done {
                    errors.push(e);
                }
                self.watch = None;
                if !had_views && errors.is_empty() {
                    errors.push(format!(
                        "No capturable windows opened for {}.",
                        self.app.name
                    ));
                }
            }
        }
        self.views.retain_mut(|view| match view.tick() {
            Ok(keep) => keep,
            Err(e) => {
                errors.push(e);
                false
            }
        });
        !self.views.is_empty() || (!had_views && self.watch.is_some())
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<Option<Session>>]
    struct ViewerDelegate;

    unsafe impl NSObjectProtocol for ViewerDelegate {}
    unsafe impl NSApplicationDelegate for ViewerDelegate {
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _: &NSApplication, _: bool) -> bool {
            if let Some(session) = self.ivars().borrow().as_ref() {
                session.reopen();
            }
            false
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _: &NSNotification) {
            self.ivars().borrow_mut().take();
        }
    }

    impl ViewerDelegate {
        #[unsafe(method(tick:))]
        fn tick(&self, timer: &NSTimer) {
            let mut errors = Vec::new();
            let keep = self.ivars().borrow_mut().as_mut().is_some_and(|s| s.tick(self.mtm(), &mut errors));
            for error in &errors {
                eprintln!("{error}");
            }
            if !keep {
                timer.invalidate();
                if !errors.is_empty() {
                    alert(self.mtm(), &errors.join("\n"));
                }
                NSApplication::sharedApplication(self.mtm()).terminate(None);
            }
        }
    }
);

pub fn run(entry: Result<(Host, App), String>) {
    let mtm = MainThreadMarker::new().expect("AppKit requires the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let session = match entry.and_then(|(host, app)| Session::start(host, app)) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("{error}");
            app.finishLaunching();
            alert(mtm, &error);
            return;
        }
    };
    if let Some(icon) = session.app.icon.as_deref().and_then(icon_image) {
        // Set the current image even if macOS cached an earlier bundle icon.
        unsafe { app.setApplicationIconImage(Some(&icon)) };
    }
    build_menu(&app, mtm, &session.app.name);
    let delegate: Retained<ViewerDelegate> = unsafe {
        msg_send![
            super(ViewerDelegate::alloc(mtm).set_ivars(RefCell::new(Some(session)))),
            init
        ]
    };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The timer's single argument is NSTimer, and the delegate outlives the loop.
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            1.0 / 60.0,
            &delegate,
            sel!(tick:),
            None,
            true,
        )
    };
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    app.run();
    timer.invalidate();
}

fn alert(mtm: MainThreadMarker, error: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Remote application unavailable"));
    alert.setInformativeText(&NSString::from_str(error));
    alert.runModal();
}

fn build_menu(app: &NSApplication, mtm: MainThreadMarker, name: &str) {
    let bar = NSMenu::new(mtm);

    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(name));
    unsafe {
        app_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str(&format!("Quit {name}")),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        );
    }
    app_item.setSubmenu(Some(&app_menu));
    bar.addItem(&app_item);

    let windows_item = NSMenuItem::new(mtm);
    let windows_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Window"));
    unsafe {
        for (title, action, key) in [
            ("Close Window", sel!(performClose:), "w"),
            ("Minimize", sel!(performMiniaturize:), "m"),
        ] {
            windows_menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            );
        }
    }
    windows_item.setSubmenu(Some(&windows_menu));
    bar.addItem(&windows_item);
    app.setMainMenu(Some(&bar));
    app.setWindowsMenu(Some(&windows_menu));
}
