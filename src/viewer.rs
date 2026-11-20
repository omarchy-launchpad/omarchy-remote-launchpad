//! Native remote window. NSImageView and pointer mapping use the same centered aspect fit.
use crate::remote::{Control, Stream};
use objc2::{
    AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject,
};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use omarchy_seams::{messages::Input, protocol::map_pointer};
use std::{
    cell::Cell,
    sync::mpsc::{SyncSender, TrySendError},
    time::{Duration, Instant},
};

struct ViewState {
    input: SyncSender<Control>,
    pending_motion: Cell<Option<(u32, u32)>>,
    size: Cell<(u32, u32)>,
    flags: Cell<NSEventModifierFlags>,
    closed: Cell<bool>,
    overloaded: Cell<bool>,
}
define_class!(
    // SAFETY: All image and window operations run on the AppKit thread.
    #[unsafe(super = NSImageView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewState]
    struct RemoteView;
    unsafe impl NSObjectProtocol for RemoteView {}
    unsafe impl NSWindowDelegate for RemoteView {
        #[unsafe(method(windowWillClose:))]
        fn will_close(&self,_:&NSNotification) {self.ivars().closed.set(true);self.release_inputs();}
        #[unsafe(method(windowDidResignKey:))]
        fn lost_focus(&self,_:&NSNotification) {self.release_inputs();}
    }
    impl RemoteView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn first_mouse(&self, _: Option<&NSEvent>) -> bool {true}
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts(&self)->bool {true}
        #[unsafe(method(keyDown:))]
        fn key_down(&self,e:&NSEvent) {self.key(e,true)}
        #[unsafe(method(keyUp:))]
        fn key_up(&self,e:&NSEvent) {self.key(e,false)}
        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self,e:&NSEvent) {self.modifiers(e.modifierFlags())}
        #[unsafe(method(mouseMoved:))]
        fn moved(&self,e:&NSEvent) {self.pointer(e,None)}
        #[unsafe(method(mouseDragged:))]
        fn dragged(&self,e:&NSEvent) {self.pointer(e,None)}
        #[unsafe(method(rightMouseDragged:))]
        fn right_drag(&self,e:&NSEvent) {self.pointer(e,None)}
        #[unsafe(method(otherMouseDragged:))]
        fn other_drag(&self,e:&NSEvent) {self.pointer(e,None)}
        #[unsafe(method(mouseDown:))]
        fn down(&self,e:&NSEvent) {self.pointer(e,Some((1,true)))}
        #[unsafe(method(mouseUp:))]
        fn up(&self,e:&NSEvent) {self.pointer(e,Some((1,false)))}
        #[unsafe(method(rightMouseDown:))]
        fn right_down(&self,e:&NSEvent) {self.pointer(e,Some((3,true)))}
        #[unsafe(method(rightMouseUp:))]
        fn right_up(&self,e:&NSEvent) {self.pointer(e,Some((3,false)))}
        #[unsafe(method(otherMouseDown:))]
        fn middle_down(&self,e:&NSEvent) {if e.buttonNumber()==2 {self.pointer(e,Some((2,true)))}}
        #[unsafe(method(otherMouseUp:))]
        fn middle_up(&self,e:&NSEvent) {if e.buttonNumber()==2 {self.pointer(e,Some((2,false)))}}
        #[unsafe(method(scrollWheel:))]
        fn scroll(&self,e:&NSEvent) {
            if let Some((x,y))=self.motion(e) && e.scrollingDeltaY()!=0.0 {
                let button=if e.scrollingDeltaY()>0.0 {4} else {5};
                self.send(vec![Input::Motion{x,y}, Input::Button{button,pressed:true},Input::Button{button,pressed:false}]);
            }
        }
    }
);
impl RemoteView {
    fn send(&self, mut inputs: Vec<Input>) {
        if let Some((x, y)) = self.ivars().pending_motion.take()
            && !matches!(
                inputs.first(),
                Some(Input::Motion { .. } | Input::ReleaseAll)
            )
        {
            inputs.insert(0, Input::Motion { x, y });
        }
        if let Err(TrySendError::Full(_)) = self.ivars().input.try_send(Control::Input(inputs)) {
            // Detach if an ordered batch can't fit, so held input gets released.
            self.ivars().overloaded.set(true);
        }
    }
    fn flush_motion(&self) {
        if let Some((x, y)) = self.ivars().pending_motion.take()
            && let Err(TrySendError::Full(_)) = self
                .ivars()
                .input
                .try_send(Control::Input(vec![Input::Motion { x, y }]))
        {
            self.ivars().pending_motion.set(Some((x, y)));
        }
    }
    fn release_inputs(&self) {
        self.ivars().flags.set(NSEventModifierFlags::empty());
        self.send(vec![Input::ReleaseAll]);
    }
    fn motion(&self, e: &NSEvent) -> Option<(u32, u32)> {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let size = self.bounds().size;
        let y = if self.isFlipped() {
            p.y
        } else {
            size.height - p.y
        };
        map_pointer((size.width, size.height), self.ivars().size.get(), (p.x, y))
    }
    fn pointer(&self, e: &NSEvent, button: Option<(u8, bool)>) {
        // AppKit can deliver the activating click before the window becomes key.
        if button.is_none() && !self.window().is_some_and(|w| w.isKeyWindow()) {
            return;
        }
        if let Some((x, y)) = self.motion(e) {
            if let Some((button, pressed)) = button {
                self.send(vec![
                    Input::Motion { x, y },
                    Input::Button { button, pressed },
                ]);
            } else {
                self.ivars().pending_motion.set(Some((x, y)));
            }
        } else if matches!(button, Some((_, false))) {
            self.release_inputs()
        }
    }
    fn modifiers(&self, now: NSEventModifierFlags) {
        let old = self.ivars().flags.replace(now);
        for (flag, keysym) in [
            (NSEventModifierFlags::Shift, 0xffe1),
            (NSEventModifierFlags::Control, 0xffe3),
            (NSEventModifierFlags::Option, 0xffe9),
        ] {
            if old.contains(flag) != now.contains(flag) {
                self.send(vec![Input::Key {
                    keysym,
                    pressed: now.contains(flag),
                }]);
            }
        }
    }
    fn key(&self, e: &NSEvent, pressed: bool) {
        self.modifiers(e.modifierFlags());
        if let Some(s) = e.charactersByApplyingModifiers(NSEventModifierFlags::empty())
            && let Some(keysym) = keysym(&s.to_string())
        {
            self.send(vec![Input::Key { keysym, pressed }]);
        }
    }
}
fn keysym(s: &str) -> Option<u32> {
    let mut chars = s.chars();
    let c = chars.next()? as u32;
    if chars.next().is_some() {
        return None;
    }
    Some(match c {
        8 | 127 => 0xff08,
        9 => 0xff09,
        3 | 13 => 0xff0d,
        27 => 0xff1b,
        0xf700 => 0xff52,
        0xf701 => 0xff54,
        0xf702 => 0xff51,
        0xf703 => 0xff53,
        0xf704..=0xf70f => 0xffbe + (c - 0xf704),
        0xf728 => 0xffff,
        0xf729 => 0xff50,
        0xf72b => 0xff57,
        0xf72c => 0xff55,
        0xf72d => 0xff56,
        32..=126 | 160..=255 => c,
        _ => return None,
    })
}

fn bitmap(w: u32, h: u32, rgba: &[u8]) -> Result<Retained<NSImage>, String> {
    if w == 0
        || h == 0
        || u64::from(w) * u64::from(h) != rgba.len() as u64 / 4
        || !rgba.len().is_multiple_of(4)
    {
        return Err("Invalid frame size.".into());
    }
    // SAFETY: Null planes request AppKit-owned writable storage, four packed bytes per pixel.
    unsafe {
        let rep=NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),std::ptr::null_mut(),w as isize,h as isize,8,4,true,false,NSDeviceRGBColorSpace,w as isize*4,32
        ).ok_or("AppKit could not allocate frame storage.")?;
        let dst = rep.bitmapData();
        if dst.is_null() {
            return Err("Missing AppKit bitmap storage.".into());
        }
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), dst, rgba.len());
        let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(w.into(), h.into()));
        image.addRepresentation(&rep);
        Ok(image)
    }
}

pub struct Viewer {
    window: Retained<NSWindow>,
    view: Retained<RemoteView>,
    stream: Stream,
    hidden: bool,
    viewport: NSSize,
    resize_since: Option<Instant>,
}
impl Viewer {
    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn new(mtm: MainThreadMarker, stream: Stream) -> Self {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(640., 480.));
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        let view: Retained<RemoteView> = unsafe {
            msg_send![super(RemoteView::alloc(mtm).set_ivars(ViewState{
            input:stream.input.clone(),pending_motion:Cell::default(),size:Cell::default(),flags:Cell::new(NSEventModifierFlags::empty()),closed:Cell::default(),overloaded:Cell::default()
        })),initWithFrame:frame]
        };
        view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        window.setContentView(Some(&view));
        window.setAcceptsMouseMovedEvents(true);
        window.setDelegate(Some(ProtocolObject::from_ref(&*view)));
        window.makeFirstResponder(Some(&view));
        Self {
            window,
            view,
            stream,
            hidden: true,
            viewport: NSSize::new(0., 0.),
            resize_since: None,
        }
    }

    // Fit immediately on the Mac. Request one Linux layout after the drag settles.
    fn sync_viewport(&mut self) {
        if self.view.ivars().size.get() == (0, 0) {
            return;
        }
        let size = self.view.bounds().size;
        if self.viewport != size {
            self.viewport = size;
            self.resize_since = Some(Instant::now());
        }
        if self.view.inLiveResize()
            || !self
                .resize_since
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(100))
        {
            return;
        }
        let (w, h) = (size.width.round() as u32, size.height.round() as u32);
        self.resize_since = None;
        if w == 0
            || h == 0
            || w > 16384
            || h > 16384
            || u64::from(w) * u64::from(h) * 4 + 8 > omarchy_seams::protocol::MAX_PAYLOAD as u64
            || self.view.ivars().size.get() == (w, h)
        {
            return;
        }
        match self.stream.input.try_send(Control::Resize(w, h)) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(_)) => self.resize_since = Some(Instant::now()),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
        }
    }

    // Ok(false) detaches this local window.
    pub fn tick(&mut self) -> Result<bool, String> {
        if self.view.ivars().closed.get() {
            return Ok(false);
        }
        if let Some(done) = self.stream.task.poll() {
            return done.map(|()| false);
        }
        if self.view.ivars().overloaded.get() {
            return Err("Input queue filled. Open the app again from Launchpad.".into());
        }
        let update = std::mem::take(&mut *self.stream.update.lock().unwrap());
        if let Some(title) = update.title {
            self.window.setTitle(&NSString::from_str(&title))
        }
        if update.hidden {
            self.view.ivars().pending_motion.take();
            self.hidden = true;
            self.window.orderOut(None);
            self.view.setImage(None);
        }
        if let Some((w, h, pixels)) = update.frame {
            if self.view.ivars().size.get() != (w, h) {
                self.view.ivars().pending_motion.take();
            }
            if self.view.ivars().size.get() == (0, 0) {
                let fit = (960. / f64::from(w)).min(720. / f64::from(h)).min(1.);
                self.window
                    .setContentSize(NSSize::new(f64::from(w) * fit, f64::from(h) * fit));
                self.window.center();
                self.viewport = self.view.bounds().size;
            }
            self.view.ivars().size.set((w, h));
            self.view.setImage(Some(&*bitmap(w, h, &pixels)?));
            if self.hidden {
                self.window.makeKeyAndOrderFront(None);
                self.hidden = false;
            }
        }
        self.view.flush_motion();
        self.sync_viewport();
        Ok(true)
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.window.setDelegate(None);
        self.window.orderOut(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_characters_and_navigation_to_x_keysyms() {
        assert_eq!(keysym("a"), Some(97));
        assert_eq!(keysym("\u{f702}"), Some(0xff51));
        assert_eq!(keysym("ab"), None);
    }
}
