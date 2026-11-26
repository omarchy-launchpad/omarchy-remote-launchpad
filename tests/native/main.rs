//! Opt-in AppKit/SSH integration test. The fixture script supplies isolated SSH trust.
#![allow(dead_code, unused_imports)]
#[path = "../../src/config.rs"]
mod config;
#[cfg(target_os = "macos")]
#[path = "../../src/dock_apps.rs"]
mod dock_apps;
#[cfg(target_os = "macos")]
#[path = "../../src/launcher.rs"]
mod launcher;
#[cfg(target_os = "macos")]
#[path = "../../src/machine_store.rs"]
mod machine_store;
#[cfg(target_os = "macos")]
#[path = "../../src/machines.rs"]
mod machines;
#[path = "../../src/remote.rs"]
mod remote;
#[cfg(target_os = "macos")]
#[path = "../../src/unlock.rs"]
mod unlock;
#[cfg(target_os = "macos")]
#[path = "../../src/viewer.rs"]
mod viewer;
#[cfg(target_os = "macos")]
#[path = "../../src/viewer_app.rs"]
mod viewer_app;
#[cfg(target_os = "macos")]
#[path = "../../src/vm_run.rs"]
mod vm_run;

#[cfg(target_os = "macos")]
mod ui {
    include!("../../src/ui.rs");
    use objc2_foundation::{NSDate, NSPoint, NSRunLoop};
    use std::time::{Duration, Instant};

    thread_local! {
        static SESSIONS: RefCell<Vec<crate::viewer_app::Session>> = const { RefCell::new(Vec::new()) };
    }

    // Inspect AppKit pixels and send native events in-process. The fixture also
    // exercises Launch Services and the separate viewer process in dock_smoke.
    fn open_in_process(host: &Host, app: &remote::App) -> Result<(), String> {
        SESSIONS.with_borrow_mut(|sessions| {
            if let Some(session) = sessions
                .iter()
                .find(|session| session.host.name() == host.name() && session.app.id == app.id)
            {
                session.reopen();
            } else {
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap())
                    .setActivationPolicy(NSApplicationActivationPolicy::Regular);
                sessions.push(crate::viewer_app::Session::start(
                    host.clone(),
                    app.clone(),
                )?);
            }
            Ok(())
        })
    }

    fn pump_until(delegate: &Delegate, mut ready: impl FnMut() -> bool) {
        let start = Instant::now();
        loop {
            delegate.tick(sel!(tick:), None);
            SESSIONS.with_borrow_mut(|sessions| {
                let mut errors = Vec::new();
                sessions.retain_mut(|session| session.tick(delegate.mtm(), &mut errors));
                assert!(errors.is_empty(), "{errors:?}");
            });
            if ready() {
                return;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!("timed out waiting for fixture state");
            }
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
        }
    }
    fn window(mtm: MainThreadMarker, title: &str) -> Option<Retained<NSWindow>> {
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .find(|w| w.isVisible() && w.title().to_string() == title)
    }
    pub fn manage_machines() {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let root =
            std::env::temp_dir().join(format!("launchpad-appkit-{}", remote::token().unwrap()));
        let target_path = root.join("launchpad.local");
        let machine_path = target_path.with_extension("machines.json");
        let vm = crate::machine_store::create_vm("Saved VM", &[], |_| false).unwrap();
        let vm_dir = vm.vm_storage(&root.join("VMs")).unwrap().unwrap();
        let vm_disk = vm_dir.join("disk.img");
        std::fs::write(&vm_disk, b"guest data").unwrap();
        crate::machine_store::save(&machine_path, &[vm]).unwrap();
        let delegate = setup(
            mtm,
            Err("No machine selected.".into()),
            open_in_process,
            target_path.clone(),
        );
        app.finishLaunching();
        let menu = delegate
            .ivars()
            .borrow()
            .as_ref()
            .unwrap()
            ._status_item
            .menu(mtm)
            .unwrap();
        let item = menu.itemWithTitle(&text("Manage Machines")).unwrap();
        assert!(item.isEnabled());
        menu.performActionForItemAtIndex(menu.indexOfItem(&item));
        let panel =
            window(mtm, "Manage Machines").expect("Manage Machines opens from the status menu");
        let subviews = panel.contentView().unwrap().subviews();
        let scroll = subviews
            .iter()
            .find_map(|view| view.downcast::<NSScrollView>().ok())
            .expect("saved-machine table has a scroll view");
        let table = scroll
            .documentView()
            .unwrap()
            .downcast::<NSTableView>()
            .unwrap();
        assert_eq!(table.numberOfColumns(), 4);
        assert!(table.headerView().is_some());
        let button = |title: &str| {
            subviews
                .iter()
                .filter_map(|view| view.downcast::<NSButton>().ok())
                .find(|button| button.title().to_string() == title)
                .unwrap_or_else(|| panic!("Missing {title} button"))
        };
        let add = button("Add Remote");
        let create = button("Create VM");
        let remove = button("Remove");
        assert!(!add.isHidden());
        assert!(!create.isHidden());
        assert!(remove.isEnabled());
        unsafe { remove.performClick(None) };
        assert_eq!(table.numberOfRows(), 0);
        assert_eq!(std::fs::read(&vm_disk).unwrap(), b"guest data");
        let input = subviews
            .iter()
            .filter_map(|view| view.downcast::<NSTextField>().ok())
            .find(|field| field.isEditable())
            .unwrap();
        for destination in ["studio", "user@backup"] {
            // These buttons target the retained delegate and accept an object sender.
            unsafe { add.performClick(None) };
            let save = button("Save Machine");
            assert!(!save.isHidden());
            let editor = input.currentEditor().expect("Add Remote focuses the input");
            editor.setString(&text(destination));
            unsafe { save.performClick(None) };
        }
        assert_eq!(table.numberOfRows(), 2);
        let saved = crate::machine_store::load(&machine_path).unwrap();
        assert_eq!(saved[0].address(), "studio");
        assert_eq!(saved[1].address(), "user@backup");
        Host::parse("studio")
            .unwrap()
            .save_to(&target_path)
            .unwrap();
        delegate.refresh(sel!(refresh:), None);
        assert!(remove.isEnabled());
        unsafe { remove.performClick(None) };
        assert_eq!(table.numberOfRows(), 1);
        let saved = crate::machine_store::load(&machine_path).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].address(), "studio");
        assert_eq!(Host::load_from(&target_path).unwrap().name(), "studio");
        assert_eq!(table.selectedRow(), 0);
        unsafe { remove.performClick(None) };
        assert_eq!(table.numberOfRows(), 0);
        assert!(!remove.isEnabled());
        assert!(
            crate::machine_store::load(&machine_path)
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read_to_string(&target_path).unwrap(), "");
        delegate.with(|c| assert!(c.launcher.host.is_none()));
        delegate.refresh(sel!(refresh:), None);
        assert_eq!(table.numberOfRows(), 0);
        if create.isEnabled() {
            unsafe { create.performClick(None) };
            assert_eq!(input.stringValue().to_string(), "Omarchy");
            let save_vm = button("Create & Open");
            assert!(!save_vm.isHidden());
            if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_VM_RUNTIME").is_some() {
                delegate.with(|controller| {
                    controller
                        .machines
                        .use_vm_data_directory(root.join("vm-data"))
                });
                let editor = input
                    .currentEditor()
                    .expect("Create VM focuses the name field");
                editor.setString(&text("Runtime onboarding fixture"));
                unsafe { save_vm.performClick(None) };
                assert_eq!(table.numberOfRows(), 1);
                let labels = || {
                    subviews
                        .iter()
                        .filter_map(|view| view.downcast::<NSTextField>().ok())
                        .map(|field| field.stringValue().to_string())
                        .collect::<Vec<_>>()
                };
                assert!(
                    labels()
                        .iter()
                        .any(|label| label == "Preparing the Omarchy runtime…")
                );
                pump_until(&delegate, || {
                    labels().iter().any(|label| {
                        label.starts_with("Starting Omarchy")
                            || label.starts_with("Downloading the Omarchy image")
                            || label.contains("not enough free space")
                    })
                });
                let runtime = root.join(
                    "vm-data/Runtime/Try Omarchy v1.4.1.app/Contents/MacOS/omarchy-vm-helper",
                );
                assert!(runtime.is_file());
                println!("Create & Open installed the signed runtime and started it directly.");
            }
        }
        panel.close();
        std::fs::remove_dir_all(root).unwrap();
        println!(
            "Manage Machines removes saved profiles, preserves VM disk data, and clears the current target."
        );
    }

    pub fn offline() {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let delegate = setup(mtm, Host::load(), open_in_process, Host::path());
        app.finishLaunching();
        delegate.with(|c| {
            assert_eq!(
                c.status_menu.connection.stringValue().to_string(),
                "●  Checking…"
            );
            assert!(!c.status_menu.open.isEnabled());
        });
        assert_offline(&delegate, &app);
    }
    fn assert_offline(delegate: &Delegate, app: &NSApplication) {
        let status = delegate
            .ivars()
            .borrow()
            .as_ref()
            .unwrap()
            .status_menu
            .connection
            .clone();
        pump_until(delegate, || {
            status.stringValue().to_string() == "●  Offline"
        });
        delegate.show(sel!(showExistingManager:), None);
        assert!(
            !delegate
                .reopen(
                    sel!(applicationShouldHandleReopen:hasVisibleWindows:),
                    app,
                    false.into(),
                )
                .as_bool()
        );
        delegate.with(|c| {
            assert!(!c.status_menu.open.isEnabled());
            assert!(!c.status_menu.unlock.isEnabled());
            assert!(!c.launcher.window.isVisible());
            assert!(c.launcher.selection().is_none());
            let count = SESSIONS.with_borrow(Vec::len);
            c.launch().unwrap();
            assert_eq!(SESSIONS.with_borrow(Vec::len), count);
        });
    }
    fn pixel(window: &NSWindow) -> Option<[u8; 4]> {
        let content = window.contentView()?;
        let image = content.downcast_ref::<NSImageView>()?.image()?;
        let reps = image.representations();
        let rep = reps.firstObject()?;
        let rep = rep.downcast_ref::<NSBitmapImageRep>()?;

        // The view retains the NSImage and bitmap representation for this sample.
        Some(unsafe { *rep.bitmapData().cast::<[u8; 4]>() })
    }
    pub fn live(id: &str) {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let delegate = setup(mtm, Host::load(), open_in_process, Host::path());
        app.finishLaunching();
        pump_until(&delegate, || {
            let mut found = false;
            delegate.with(|c| {
                c.launcher.search.setStringValue(&text(id));
                found = c.launcher.selection().is_some_and(|a| a.id == id);
            });
            found
        });
        delegate.choose(sel!(chooseApp:), None);
        let expected_title = std::env::var("OMARCHY_LAUNCHPAD_TEST_WINDOW_TITLE").ok();
        let mut rendered_window = None;
        pump_until(&delegate, || {
            app.windows().iter().any(|w| {
                let Some(view) = w.contentView() else {
                    return false;
                };
                let Some(image) = view
                    .downcast_ref::<NSImageView>()
                    .and_then(NSImageView::image)
                else {
                    return false;
                };
                if w.isVisible()
                    && image.size().width > 0.
                    && image.size().height > 0.
                    && expected_title
                        .as_ref()
                        .is_none_or(|title| *title == w.title().to_string())
                {
                    println!(
                        "Rendered {} ({} × {})",
                        w.title(),
                        image.size().width,
                        image.size().height
                    );
                    rendered_window = Some(w);
                    true
                } else {
                    false
                }
            })
        });
        let w = rendered_window.unwrap();
        if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_CLICK_PROBE").is_some() {
            let ready = "omarchy-launchpad click probe ready - Chromium";
            let passed = "omarchy-launchpad click probe passed - Chromium";
            pump_until(&delegate, || window(mtm, ready).is_some());
            let probe = window(mtm, ready).unwrap();
            let view = probe.contentView().unwrap();
            let size = view.bounds().size;
            let y = if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_CLICK_BOTTOM_EDGE").is_some() {
                size.height / 20.
            } else {
                size.height / 2.
            };
            for ty in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
                let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    ty, NSPoint::new(size.width / 2., y), NSEventModifierFlags::empty(),
                    0., probe.windowNumber(), None, 0, 1, 1.).unwrap();
                probe.sendEvent(&event);
            }
            pump_until(&delegate, || window(mtm, passed).is_some());
            println!("Chromium handled the AppKit click.");
        }
        if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_POINTER_BURST").is_some() {
            pointer_burst(&w);
            let start = Instant::now();
            pump_until(&delegate, || {
                assert!(w.isVisible(), "viewer detached during pointer burst");
                start.elapsed() > Duration::from_secs(2)
            });
        }
        if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_TILED_RESIZE").is_some() {
            let view = w.contentView().unwrap().downcast::<NSImageView>().unwrap();
            for (width, height) in [(840, 680), (1024, 768)] {
                w.setContentSize(objc2_foundation::NSSize::new(width as f64, height as f64));
                pump_until(&delegate, || {
                    view.image()
                        .and_then(|image| image.representations().firstObject())
                        .is_some_and(|rep| rep.pixelsWide() == width && rep.pixelsHigh() == height)
                });
                println!("Mac resize delivered a {width} × {height} source bitmap.");
            }
        }
        if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_SINGLE_WINDOW").is_some() {
            delegate.choose(sel!(chooseApp:), None);
            let start = Instant::now();
            pump_until(&delegate, || {
                let viewers: Vec<_> = app
                    .windows()
                    .iter()
                    .filter(|w| {
                        w.isVisible()
                            && w.contentView()
                                .is_some_and(|view| view.downcast_ref::<NSImageView>().is_some())
                    })
                    .map(|w| w.title().to_string())
                    .collect();
                assert_eq!(viewers, [w.title().to_string()]);
                start.elapsed() >= Duration::from_secs(1)
            });
            println!("Exactly one viewer opened.");
        }
    }
    fn pointer_burst(w: &NSWindow) {
        w.makeKeyAndOrderFront(None);
        let view = w.contentView().unwrap();
        let app = NSApplication::sharedApplication(w.mtm());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !w.isKeyWindow() {
            assert!(Instant::now() < deadline, "viewer did not become key");
            if let Some(e) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::dateWithTimeIntervalSinceNow(0.01)),
                unsafe { objc2_foundation::NSDefaultRunLoopMode },
                true,
            ) {
                app.sendEvent(&e);
            }
        }
        for i in 0..4096 {
            let e = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                NSEventType::MouseMoved, NSPoint::new(view.bounds().size.width / 2. + f64::from(i % 8),
                    view.bounds().size.height / 2.), NSEventModifierFlags::empty(), 0., w.windowNumber(), None, i as isize, 0, 0.).unwrap();
            view.mouseMoved(&e);
        }
    }
    pub fn smoke() {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let delegate = setup(
            mtm,
            Host::parse("omarchy-launchpad-fixture"),
            open_in_process,
            Host::path(),
        );
        app.finishLaunching();
        if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_DOCK_ONLY").is_some() {
            dock_smoke(&delegate);
            return;
        }
        let mut found = false;
        pump_until(&delegate, || {
            delegate.with(|c| {
                let l = &mut c.launcher;
                l.search.setStringValue(&text("omarchy-launchpad smoke"));
                found = l
                    .selection()
                    .is_some_and(|a| a.id == "omarchy-launchpad-fixture.desktop");
            });
            found
        });
        if let Some(outage) = std::env::var_os("OMARCHY_LAUNCHPAD_FIXTURE_OUTAGE") {
            // Block new connections while the catalogue's SSH master remains open.
            delegate.with(|c| assert!(c.launcher.window.isVisible()));
            let outage = std::path::PathBuf::from(outage);
            std::fs::write(&outage, []).unwrap();
            assert_offline(&delegate, &app);
            std::fs::remove_file(outage).unwrap();
            pump_until(&delegate, || {
                let mut ready = false;
                delegate.with(|c| {
                    ready = c.status_menu.open.isEnabled() && c.launcher.selection().is_some();
                });
                ready
            });
            delegate.show(sel!(showExistingManager:), None);
            delegate.with(|c| assert!(c.launcher.window.isVisible()));
            println!("Launchpad stayed closed during the SSH outage and reopened after recovery.");
        }
        let start = Instant::now();
        delegate.choose(sel!(chooseApp:), None);
        delegate.choose(sel!(chooseApp:), None);
        assert_eq!(
            SESSIONS.with_borrow(Vec::len),
            1,
            "duplicate launch while opening"
        );
        pump_until(&delegate, || {
            window(mtm, "omarchy-launchpad fixture").is_some()
        });
        let w = window(mtm, "omarchy-launchpad fixture").unwrap();
        delegate.choose(sel!(chooseApp:), None);
        assert_eq!(
            SESSIONS.with_borrow(Vec::len),
            1,
            "duplicate launch after opening"
        );
        assert_eq!(pixel(&w), Some([255, 0, 0, 255]));
        println!(
            "Launch to AppKit frame: {:?}, 24576 RGBA bytes",
            start.elapsed()
        );
        delegate.with(|c| {
            c.launcher.window.close();
        });
        assert!(w.isVisible());
        let event = |ty| {
            NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            ty,NSPoint::ZERO,NSEventModifierFlags::empty(),0.,w.windowNumber(),None,&text("a"),&text("a"),false,0).unwrap()
        };
        w.makeKeyAndOrderFront(None);
        let view = w.contentView().unwrap();
        pointer_burst(&w);
        let start = Instant::now();
        view.keyDown(&event(NSEventType::KeyDown));
        view.keyUp(&event(NSEventType::KeyUp));
        pump_until(&delegate, || pixel(&w) == Some([0, 255, 0, 255]));
        println!("AppKit key to changed bitmap: {:?}", start.elapsed());
        pump_until(&delegate, || {
            window(mtm, "omarchy-launchpad dialog").is_some()
        });
        let dialog = window(mtm, "omarchy-launchpad dialog").unwrap();
        // Pin the dialog size so AppKit's restored frame cannot cover both test points.
        let dialog_size = objc2_foundation::NSSize::new(128., 80.);
        dialog.setContentSize(dialog_size);
        w.setContentSize(objc2_foundation::NSSize::new(320., 180.));
        pump_until(&delegate, || {
            let image_size = |window: &NSWindow| {
                window
                    .contentView()
                    .and_then(|v| v.downcast_ref::<NSImageView>()?.image())
                    .map(|image| image.size())
            };
            image_size(&w) == Some(objc2_foundation::NSSize::new(320., 180.))
                && image_size(&dialog) == Some(dialog_size)
        });
        assert_eq!(pixel(&w), Some([0, 255, 0, 255]));
        w.makeKeyAndOrderFront(None);
        let mouse = |ty, point| {
            NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            ty, point,NSEventModifierFlags::empty(),0., w.windowNumber(),None,0,1,1.).unwrap()
        };
        // The focused dialog covers (150, 30) on Linux. The second point is exposed.
        for point in [NSPoint::new(150., 150.), NSPoint::new(275., 40.)] {
            view.mouseDown(&mouse(NSEventType::LeftMouseDown, point));
            view.mouseUp(&mouse(NSEventType::LeftMouseUp, point));
        }
        pump_until(&delegate, || pixel(&w) == Some([255, 255, 0, 255]));
        println!("Resized Linux frame to 320 × 180 and verified click at (275, 140).");
        dialog.close();
        w.close();
        pump_until(&delegate, || SESSIONS.with_borrow(Vec::is_empty));

        // Keep the old AppKit objects alive and reopen the same Linux window.
        delegate.choose(sel!(chooseApp:), None);
        pump_until(&delegate, || {
            window(mtm, "omarchy-launchpad fixture")
                .is_some_and(|reopened| pixel(&reopened) == Some([255, 255, 0, 255]))
        });
        assert!(!w.isVisible());
        assert!(!dialog.isVisible());
        println!("Closing the viewers left the Linux app running with its pixels intact.");
        SESSIONS.with_borrow_mut(Vec::clear);
        dock_smoke(&delegate);
    }

    pub fn remote_icon(id: &str) {
        use std::{fs, process::Command};

        let mtm = MainThreadMarker::new().unwrap();
        let host = Host::load().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let delegate = setup(mtm, Ok(host.clone()), open_in_process, Host::path());
        app.finishLaunching();
        let mut entry = None;
        pump_until(&delegate, || {
            delegate.with(|c| {
                c.launcher.search.setStringValue(&text(id));
                entry = c.launcher.selection().filter(|app| app.id == id);
            });
            entry.is_some()
        });
        let entry = entry.unwrap();
        let icon = entry
            .icon
            .as_deref()
            .expect("expected the remote desktop icon");
        assert!(dock_apps::icon_image(icon).is_some());
        let root =
            std::env::temp_dir().join(format!("launchpad-icon-{}", remote::token().unwrap()));
        let bundle =
            dock_apps::prepare(&root, &std::env::current_exe().unwrap(), &host, &entry).unwrap();
        let resources = bundle.join("Contents/Resources");
        assert_eq!(fs::read(resources.join("App.png")).unwrap(), icon);
        assert!(
            fs::read(resources.join("App.icns"))
                .unwrap()
                .starts_with(b"icns")
        );
        assert!(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--strict"])
                .arg(bundle)
                .status()
                .unwrap()
                .success()
        );
        fs::remove_dir_all(root).unwrap();
        println!(
            "Fetched and decoded the Linux icon for {id} and installed it in the signed Dock bundle."
        );
    }

    fn dock_smoke(delegate: &Delegate) {
        use objc2_foundation::{NSURL, NSURLLocalizedNameKey};
        use std::{fs, process::Command};

        let ssh_config = std::env::var("OMARCHY_LAUNCHPAD_FIXTURE_SSH_CONFIG").unwrap();
        let root = std::path::Path::new(&ssh_config)
            .parent()
            .unwrap()
            .join("Remote Apps");
        let source = std::env::current_exe().unwrap();
        let host = Host::parse("omarchy-launchpad-fixture").unwrap();
        let app = remote::App {
            id: "omarchy-launchpad-fixture.desktop".into(),
            name: "Fixture & Editor".into(),
            icon: None,
        };
        let bundle = dock_apps::prepare(&root, &source, &host, &app).unwrap();
        let renamed = remote::App {
            name: "Renamed Fixture".into(),
            ..app.clone()
        };
        assert_eq!(
            dock_apps::prepare(&root, &source, &host, &renamed).unwrap(),
            bundle
        );
        let url = NSURL::fileURLWithPath(&text(bundle.to_str().unwrap()));
        let mut display_name = None;
        unsafe { url.getResourceValue_forKey_error(&mut display_name, NSURLLocalizedNameKey) }
            .unwrap();
        assert_eq!(
            display_name
                .unwrap()
                .downcast::<NSString>()
                .unwrap()
                .to_string(),
            renamed.name
        );
        assert_ne!(
            dock_apps::bundle_id(&host, &app),
            dock_apps::bundle_id(&Host::parse("another-host").unwrap(), &app),
        );
        assert_ne!(
            dock_apps::bundle_id(&host, &app),
            dock_apps::bundle_id(
                &host,
                &remote::App {
                    id: "other.desktop".into(),
                    ..app.clone()
                }
            ),
        );
        let open = || {
            assert!(
                Command::new("/usr/bin/open")
                    .args([
                        "--env",
                        &format!("OMARCHY_LAUNCHPAD_FIXTURE_SSH_CONFIG={ssh_config}")
                    ])
                    .args(["--stderr"])
                    .arg(root.join("viewer.log"))
                    .arg(&bundle)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        let running = || {
            NSRunningApplication::runningApplicationsWithBundleIdentifier(&text(
                &dock_apps::bundle_id(&host, &app),
            ))
        };
        open();
        pump_until(delegate, || {
            running().firstObject().is_some_and(|viewer| {
                viewer.isFinishedLaunching()
                    && viewer.activationPolicy() == NSApplicationActivationPolicy::Regular
                    && viewer
                        .localizedName()
                        .is_some_and(|name| name.to_string() == renamed.name)
            })
        });
        let viewer = running().firstObject().unwrap();
        assert_ne!(viewer, NSRunningApplication::currentApplication());
        assert!(
            Command::new("/usr/bin/open")
                .arg(&bundle)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(running().count(), 1);
        assert_eq!(running().firstObject().unwrap(), viewer);
        assert!(viewer.terminate());
        pump_until(delegate, || viewer.isTerminated());
        fs::remove_dir_all(root).unwrap();
        println!("Launch Services registered and reopened one separate, Dock-visible viewer.");
    }
}
fn main() {
    #[cfg(target_os = "macos")]
    if let Some(entry) = dock_apps::entry() {
        viewer_app::run(entry);
    } else if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_MANAGE_MACHINES").is_some() {
        objc2::rc::autoreleasepool(|_| ui::manage_machines());
    } else if std::env::var_os("OMARCHY_LAUNCHPAD_TEST_OFFLINE").is_some() {
        objc2::rc::autoreleasepool(|_| ui::offline());
    } else if std::env::var_os("OMARCHY_LAUNCHPAD_FIXTURE_SSH_CONFIG").is_some() {
        objc2::rc::autoreleasepool(|_| ui::smoke());
    } else if let Ok(id) = std::env::var("OMARCHY_LAUNCHPAD_TEST_ICON_ID") {
        objc2::rc::autoreleasepool(|_| ui::remote_icon(&id));
    } else if let Ok(id) = std::env::var("OMARCHY_LAUNCHPAD_TEST_DESKTOP_ID") {
        objc2::rc::autoreleasepool(|_| ui::live(&id));
    } else {
        println!("Native fixture available through scripts/test-native.");
    }
}
