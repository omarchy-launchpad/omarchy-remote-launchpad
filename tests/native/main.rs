//! Opt-in AppKit/SSH integration test. The fixture script supplies isolated SSH trust.
#![allow(dead_code, unused_imports)]
#[path = "../../src/config.rs"]
mod config;
#[cfg(target_os = "macos")]
#[path = "../../src/launcher.rs"]
mod launcher;
#[path = "../../src/remote.rs"]
mod remote;
#[cfg(target_os = "macos")]
#[path = "../../src/viewer.rs"]
mod viewer;

#[cfg(target_os = "macos")]
mod ui {
    include!("../../src/ui.rs");
    use objc2_foundation::{NSDate, NSPoint, NSRunLoop};
    use std::time::{Duration, Instant};

    fn pump_until(delegate: &Delegate, mut ready: impl FnMut() -> bool) {
        let start = Instant::now();
        loop {
            delegate.tick(sel!(tick:), None);
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
        let delegate = setup(mtm, Host::load());
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
        let expected_title = std::env::var("SEAMS_TEST_WINDOW_TITLE").ok();
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
        if std::env::var_os("SEAMS_TEST_CLICK_PROBE").is_some() {
            let ready = "Seams click probe ready - Chromium";
            let passed = "Seams click probe passed - Chromium";
            pump_until(&delegate, || window(mtm, ready).is_some());
            let probe = window(mtm, ready).unwrap();
            let view = probe.contentView().unwrap();
            let size = view.bounds().size;
            for ty in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
                let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    ty, NSPoint::new(size.width / 2., size.height / 2.), NSEventModifierFlags::empty(),
                    0., probe.windowNumber(), None, 0, 1, 1.).unwrap();
                probe.sendEvent(&event);
            }
            pump_until(&delegate, || window(mtm, passed).is_some());
            println!("Chromium handled the AppKit click.");
        }
        if std::env::var_os("SEAMS_TEST_POINTER_BURST").is_some() {
            pointer_burst(&w);
            let start = Instant::now();
            pump_until(&delegate, || {
                assert!(w.isVisible(), "viewer detached during pointer burst");
                start.elapsed() > Duration::from_secs(2)
            });
        }
        if std::env::var_os("SEAMS_TEST_TILED_RESIZE").is_some() {
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
        if std::env::var_os("SEAMS_TEST_SINGLE_WINDOW").is_some() {
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
        let delegate = setup(mtm, Host::parse("seams-fixture"));
        app.finishLaunching();
        let mut found = false;
        pump_until(&delegate, || {
            delegate.with(|c| {
                let l = &mut c.launcher;
                l.search.setStringValue(&text("seams smoke"));
                found = l
                    .selection()
                    .is_some_and(|a| a.id == "seams-fixture.desktop");
            });
            found
        });
        let start = Instant::now();
        delegate.choose(sel!(chooseApp:), None);
        pump_until(&delegate, || window(mtm, "Seams fixture").is_some());
        let w = window(mtm, "Seams fixture").unwrap();
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
        pump_until(&delegate, || window(mtm, "Seams dialog").is_some());
        w.setContentSize(objc2_foundation::NSSize::new(320., 180.));
        pump_until(&delegate, || {
            let image = w
                .contentView()
                .and_then(|v| v.downcast_ref::<NSImageView>()?.image());
            image.is_some_and(|i| i.size() == objc2_foundation::NSSize::new(320., 180.))
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
        println!("Search, launch, native frames, keyboard input and dialog passed.");
    }
}
fn main() {
    #[cfg(target_os = "macos")]
    if std::env::var_os("SEAMS_FIXTURE_SSH_CONFIG").is_some() {
        objc2::rc::autoreleasepool(|_| ui::smoke());
    } else if let Ok(id) = std::env::var("SEAMS_TEST_DESKTOP_ID") {
        objc2::rc::autoreleasepool(|_| ui::live(&id));
    } else {
        println!("Native fixture available through scripts/test-native.");
    }
}
