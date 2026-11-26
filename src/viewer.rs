//! Native remote window. NSImageView and pointer mapping use the same centered aspect fit.
use crate::remote::WindowStream;
use objc2::{
    AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject,
};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use omarchy_launchpad::{messages::Input, protocol::map_pointer};
use std::cell::Cell;

const DEFAULT_VIEWER_WIDTH: f64 = 960.;
const DEFAULT_VIEWER_HEIGHT: f64 = 720.;
const VIEWER_FRAME_NAME: &str = "omarchy-launchpad Viewer";

struct ViewState {
    stream: WindowStream,
    flags: Cell<NSEventModifierFlags>,
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
        fn will_close(&self, _: &NSNotification) {
            self.ivars().flags.set(NSEventModifierFlags::empty());
            self.ivars().stream.request_close();
        }
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
                self.ivars().stream.send_input(vec![Input::Motion{x,y}, Input::Button{button,pressed:true},Input::Button{button,pressed:false}]);
            }
        }
    }
);
impl RemoteView {
    fn release_inputs(&self) {
        self.ivars().flags.set(NSEventModifierFlags::empty());
        self.ivars().stream.release_input();
    }
    fn motion(&self, e: &NSEvent) -> Option<(u32, u32)> {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let size = self.bounds().size;
        let y = if self.isFlipped() {
            p.y
        } else {
            size.height - p.y
        };
        map_pointer(
            (size.width, size.height),
            self.ivars().stream.frame_size(),
            (p.x, y),
        )
    }
    fn pointer(&self, e: &NSEvent, button: Option<(u8, bool)>) {
        // AppKit can deliver the activating click before the window becomes key.
        if button.is_none() && !self.window().is_some_and(|w| w.isKeyWindow()) {
            return;
        }
        if let Some((x, y)) = self.motion(e) {
            if let Some((button, pressed)) = button {
                eprintln!(
                    "[omarchy-launchpad-viewer] button={button} pressed={pressed} mapped=({x},{y}) window=({:.1},{:.1}) key={}",
                    e.locationInWindow().x,
                    e.locationInWindow().y,
                    self.window().is_some_and(|window| window.isKeyWindow())
                );
                self.ivars().stream.send_input(vec![
                    Input::Motion { x, y },
                    Input::Button { button, pressed },
                ]);
            } else {
                self.ivars().stream.pointer_motion(x, y);
            }
        } else if let Some((button, pressed)) = button {
            eprintln!(
                "[omarchy-launchpad-viewer] button={button} pressed={pressed} ignored: pointer outside rendered frame"
            );
            if !pressed {
                self.release_inputs()
            }
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
                self.ivars().stream.send_input(vec![Input::Key {
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
            self.ivars()
                .stream
                .send_input(vec![Input::Key { keysym, pressed }]);
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
    hidden: bool,
}
impl Viewer {
    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn is_minimized(&self) -> bool {
        self.window.isMiniaturized()
    }

    pub fn restore(&self) {
        self.window.deminiaturize(None);
        self.present();
    }

    pub fn present(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn new(mtm: MainThreadMarker, stream: WindowStream) -> Self {
        let frame = NSRect::new(
            NSPoint::ZERO,
            NSSize::new(DEFAULT_VIEWER_WIDTH, DEFAULT_VIEWER_HEIGHT),
        );
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
        let frame_name = NSString::from_str(VIEWER_FRAME_NAME);
        if !window.setFrameUsingName(&frame_name) {
            window.center();
        }
        window.setFrameAutosaveName(&frame_name);
        let view: Retained<RemoteView> = unsafe {
            msg_send![super(RemoteView::alloc(mtm).set_ivars(ViewState{
            stream, flags: Cell::new(NSEventModifierFlags::empty())
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
            hidden: true,
        }
    }

    // Ok(false) detaches this local window.
    pub fn tick(&mut self) -> Result<bool, String> {
        let stream = &self.view.ivars().stream;
        let Some(update) = stream.poll()? else {
            return Ok(false);
        };
        if let Some(title) = update.title {
            self.window.setTitle(&NSString::from_str(&title))
        }
        if update.hidden {
            self.hidden = true;
            self.window.orderOut(None);
            self.view.setImage(None);
        }
        if let Some((w, h, pixels)) = update.frame {
            self.view.setImage(Some(&*bitmap(w, h, &pixels)?));
            if self.hidden {
                self.present();
                self.hidden = false;
            }
        }
        let size = self.view.bounds().size;
        stream.sync_controls((size.width, size.height), self.view.inLiveResize());
        Ok(true)
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.window.setDelegate(None);
        self.window.orderOut(None);
        self.view.ivars().stream.detach();
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
