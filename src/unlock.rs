//! Prompt for Omarchy's own lock screen when the configured host locks.
#[path = "unlock/gliff.rs"]
mod gliff;

use crate::{
    config::Host,
    launcher::{GREEN, MUTED, SELECTION, TEXT, button, color, mono, panel, rect},
    remote::Task,
};
use gliff::{Geometry, Mirror};
use gliff_proto::msg::ClientMsg;
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::AnyObject, sel,
};
use objc2_app_kit::*;
use objc2_foundation::{MainThreadMarker, NSString};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const AUTH_POLL_INTERVAL: Duration = Duration::from_millis(200);
const STALE_AFTER: Duration = Duration::from_secs(5);
pub const PASSWORD_TITLE: &str = "Unlock Omarchy";

pub struct LockPrompt {
    host: Option<Host>,
    probe: Option<Task<Option<bool>>>,
    locking: Option<Task<()>>,
    next_probe: Instant,
    last_check: Option<Instant>,
    locked: bool,
    supported: bool,
    prompted: bool,
    requested: bool,
    window: Option<LockWindow>,
}

impl LockPrompt {
    pub fn new() -> Self {
        Self {
            host: None,
            probe: None,
            locking: None,
            next_probe: Instant::now(),
            last_check: None,
            locked: false,
            supported: true,
            prompted: false,
            requested: false,
            window: None,
        }
    }

    pub fn request(&mut self) {
        if self.resume() {
            return;
        }
        self.requested = true;
    }

    /// Recheck lock state after the app is reopened without treating an
    /// unlocked host as an error.
    pub fn resume(&mut self) -> bool {
        self.requested = false;
        if let Some(window) = &self.window
            && window.window.isVisible()
        {
            window.present();
            return true;
        }
        self.next_probe = Instant::now();
        self.supported = true;
        self.prompted = false;
        false
    }

    pub fn is_visible(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|window| window.window.isVisible())
    }

    pub fn dismiss(&mut self) {
        let requested = std::mem::take(&mut self.requested);
        if self.window.take().is_some() || requested {
            self.prompted = true;
        }
    }

    fn set_host(&mut self, host: Option<&Host>) {
        if self.host.as_ref().map(Host::name) != host.map(Host::name) {
            *self = Self {
                host: host.cloned(),
                requested: self.requested,
                ..Self::new()
            };
        }
    }

    pub fn lock(&mut self, host: &Host) -> Result<(), String> {
        self.set_host(Some(host));
        if self.locking.is_none() {
            self.locking = Some(gliff::lock(host)?);
            self.prompted = self.is_visible();
        }
        Ok(())
    }

    pub fn tick(&mut self, mtm: MainThreadMarker, host: Option<&Host>) -> Result<(), String> {
        self.set_host(host);
        let Some(host) = &self.host else {
            return if std::mem::take(&mut self.requested) {
                Err("Configure an Omarchy host first.".into())
            } else {
                Ok(())
            };
        };
        let now = Instant::now();
        if let Some(result) = self.locking.as_ref().and_then(Task::poll) {
            self.locking = None;
            result?;
            self.supported = true;
            self.next_probe = now;
        }
        if let Some(result) = self.probe.as_ref().and_then(Task::poll) {
            self.probe = None;
            match result {
                Ok(Some(locked)) => {
                    self.last_check = Some(now);
                    self.locked = locked;
                    if !locked {
                        self.window = None;
                        self.prompted = false;
                        if std::mem::take(&mut self.requested) {
                            return Err("Omarchy is already unlocked.".into());
                        }
                    }
                }
                Ok(None) => {
                    self.supported = false;
                    self.window = None;
                    if std::mem::take(&mut self.requested) {
                        return Err("This host does not report Hyprland lock state.".into());
                    }
                }
                Err(error) if std::mem::take(&mut self.requested) => return Err(error),
                Err(_) => {}
            }
        }
        if self.probe.is_none() && self.supported && now >= self.next_probe {
            self.next_probe = now + POLL_INTERVAL;
            match gliff::probe(host, false) {
                Ok(probe) => self.probe = Some(probe),
                Err(error) if std::mem::take(&mut self.requested) => return Err(error),
                Err(_) => {}
            }
        }
        let fresh = self.last_check.is_some_and(|at| now - at < STALE_AFTER);
        if !fresh {
            self.window = None;
        }
        if let Some(window) = &mut self.window {
            match window.tick(host) {
                Ok(true) => {
                    if matches!(window.attempt, Some(Attempt::Authenticating(_))) {
                        self.next_probe = self.next_probe.min(now + AUTH_POLL_INTERVAL);
                    }
                    return Ok(());
                }
                Ok(false) => self.window = None,
                Err(error) => {
                    self.window = None;
                    return Err(error);
                }
            }
        }
        if fresh && self.locked && !self.prompted && (!self.requested || self.probe.is_none()) {
            self.prompted = true;
            self.requested = false;
            self.window = Some(LockWindow::new(mtm, host, Mirror::start(host)?));
        }
        Ok(())
    }
}

define_class!(
    #[unsafe(super = objc2_foundation::NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Cell<bool>]
    struct LoginActions;
    impl LoginActions {
        #[unsafe(method(submitPassword:))]
        fn submit(&self, _: Option<&AnyObject>) {
            self.ivars().set(true);
        }
    }
);

enum Attempt {
    WaitingForMirror(Vec<ClientMsg>),
    VerifyLock {
        probe: Task<Option<bool>>,
        keys: Vec<ClientMsg>,
        started: Instant,
    },
    Authenticating(Instant),
}

struct LockWindow {
    window: Retained<NSWindow>,
    password: Retained<NSSecureTextField>,
    unlock: Retained<NSButton>,
    status: Retained<NSTextField>,
    actions: Retained<LoginActions>,
    mirror: Mirror,
    geometry: Option<Geometry>,
    attempt: Option<Attempt>,
    started: Instant,
}

impl LockWindow {
    fn new(mtm: MainThreadMarker, host: &Host, mirror: Mirror) -> Self {
        let window = panel(mtm, rect(0., 0., 340., 190.), PASSWORD_TITLE);
        let content = window.contentView().unwrap();
        let actions: Retained<LoginActions> = unsafe {
            msg_send![
                super(LoginActions::alloc(mtm).set_ivars(Cell::default())),
                init
            ]
        };
        let title = NSTextField::labelWithString(&NSString::from_str(PASSWORD_TITLE), mtm);
        title.setFrame(rect(20., 144., 300., 24.));
        title.setFont(Some(&mono(18.)));
        title.setTextColor(Some(&color(TEXT)));

        let host_label = NSTextField::labelWithString(&NSString::from_str(host.name()), mtm);
        host_label.setFrame(rect(20., 122., 300., 18.));
        host_label.setFont(Some(&mono(12.)));
        host_label.setTextColor(Some(&color(MUTED)));
        host_label.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);

        let password = NSSecureTextField::initWithFrame(
            NSSecureTextField::alloc(mtm),
            rect(20., 76., 300., 34.),
        );
        password.setFont(Some(&mono(14.)));
        password.setTextColor(Some(&color(TEXT)));
        password.setBackgroundColor(Some(&color(SELECTION)));
        password.setDrawsBackground(true);
        password.setPlaceholderString(Some(&NSString::from_str("Linux password")));
        unsafe {
            password.setTarget(Some(&actions));
            password.setAction(Some(sel!(submitPassword:)));
        }

        let status = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Connecting to Omarchy…"),
            mtm,
        );
        status.setFrame(rect(20., 40., 300., 32.));
        status.setFont(Some(&mono(12.)));
        status.setTextColor(Some(&color(MUTED)));

        let unlock = button(
            mtm,
            "Unlock",
            &actions,
            sel!(submitPassword:),
            rect(253., 10., 70., 28.),
        );
        unlock.setContentTintColor(Some(&color(GREEN)));
        unlock.setKeyEquivalent(&NSString::from_str("\r"));
        let cancel = button(
            mtm,
            "Cancel",
            &window,
            sel!(orderOut:),
            rect(179., 10., 70., 28.),
        );
        cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));

        content.addSubview(&title);
        content.addSubview(&host_label);
        content.addSubview(&password);
        content.addSubview(&status);
        content.addSubview(&cancel);
        content.addSubview(&unlock);

        let this = Self {
            window,
            password,
            unlock,
            status,
            actions,
            mirror,
            geometry: None,
            attempt: None,
            started: Instant::now(),
        };
        this.present();
        this.window.makeFirstResponder(Some(&this.password));
        this
    }

    fn present(&self) {
        self.window.makeKeyAndOrderFront(None);
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.window.mtm()).activateIgnoringOtherApps(true);
    }

    fn ready(&self, message: &str) {
        self.status.setStringValue(&NSString::from_str(message));
        self.password.setEnabled(true);
        self.unlock.setEnabled(true);
        self.window.makeFirstResponder(Some(&self.password));
    }

    fn tick(&mut self, host: &Host) -> Result<bool, String> {
        if !self.window.isVisible() {
            return Ok(false);
        }
        if let Some(geometry) = self.mirror.poll()? {
            if self.geometry.replace(geometry).is_none() && self.attempt.is_none() {
                self.status.setStringValue(&NSString::from_str(""));
            }
        } else if self.geometry.is_none() && self.started.elapsed() > Duration::from_secs(12) {
            return Err("Omarchy’s lock screen did not respond. Reopen the unlock window.".into());
        }

        if self.actions.ivars().replace(false) && self.attempt.is_none() {
            self.submit();
        }

        match self.attempt.take() {
            Some(Attempt::WaitingForMirror(keys)) => {
                if let Some(geometry) = self.geometry {
                    match gliff::probe(host, true) {
                        Ok(probe) => {
                            self.status.setStringValue(&NSString::from_str(
                                "Confirming Omarchy is locked…",
                            ));
                            self.attempt = Some(Attempt::VerifyLock {
                                probe,
                                keys: unlock_keys(geometry, keys),
                                started: Instant::now(),
                            });
                        }
                        Err(_) => self.ready("Could not confirm the lock. Try again."),
                    }
                } else {
                    self.attempt = Some(Attempt::WaitingForMirror(keys));
                }
            }
            Some(Attempt::VerifyLock {
                probe,
                keys,
                started,
            }) => match probe.poll() {
                Some(Ok(Some(true))) => {
                    self.mirror.send(keys)?;
                    self.status
                        .setStringValue(&NSString::from_str("Checking password…"));
                    self.attempt = Some(Attempt::Authenticating(Instant::now()));
                }
                Some(Ok(Some(false))) => return Ok(false),
                Some(_) => self.ready("Could not confirm the lock. Try again."),
                None if started.elapsed() >= STALE_AFTER => {
                    self.ready("Could not confirm the lock. Try again.");
                }
                None => {
                    self.attempt = Some(Attempt::VerifyLock {
                        probe,
                        keys,
                        started,
                    })
                }
            },
            Some(Attempt::Authenticating(started))
                if started.elapsed() >= Duration::from_secs(5) =>
            {
                self.ready("Omarchy is still locked. Try again.");
            }
            other => self.attempt = other,
        }
        Ok(true)
    }

    fn submit(&mut self) {
        self.password.validateEditing();
        let password = self.password.stringValue();
        self.password.abortEditing();
        self.password.setStringValue(&NSString::from_str(""));
        if password.is_empty() {
            self.ready("Enter your Linux password.");
            return;
        }
        let Some(keys) = password_keys(&password.to_string()) else {
            self.ready("Enter a password using printable ASCII.");
            return;
        };
        self.password.setEnabled(false);
        self.unlock.setEnabled(false);
        self.attempt = Some(Attempt::WaitingForMirror(keys));
    }
}

impl Drop for LockWindow {
    fn drop(&mut self) {
        self.password.abortEditing();
        self.password.setStringValue(&NSString::from_str(""));
        self.window.orderOut(None);
    }
}

// Omarchy's field is centered on the lock surface. Escape clears any
// text already there before the password is entered on Gliff's US keymap.
fn unlock_keys(geometry: Geometry, password: Vec<ClientMsg>) -> Vec<ClientMsg> {
    let mut keys = vec![
        ClientMsg::PointerMotion {
            x: f64::from(geometry.width) / (2. * geometry.scale),
            y: f64::from(geometry.height) / (2. * geometry.scale),
        },
        ClientMsg::PointerButton {
            button: 0x110,
            pressed: true,
        },
        ClientMsg::PointerButton {
            button: 0x110,
            pressed: false,
        },
        ClientMsg::Key {
            keycode: 1,
            pressed: true,
        },
        ClientMsg::Key {
            keycode: 1,
            pressed: false,
        },
    ];
    keys.extend(password);
    keys
}

fn password_keys(password: &str) -> Option<Vec<ClientMsg>> {
    if password.is_empty() {
        return None;
    }
    let mut keys = Vec::new();
    for ch in password.chars() {
        let (keycode, shifted) = if ch == ' ' { (57, false) } else { us_key(ch)? };
        if shifted {
            keys.push(ClientMsg::Key {
                keycode: 42,
                pressed: true,
            });
        }
        keys.push(ClientMsg::Key {
            keycode,
            pressed: true,
        });
        keys.push(ClientMsg::Key {
            keycode,
            pressed: false,
        });
        if shifted {
            keys.push(ClientMsg::Key {
                keycode: 42,
                pressed: false,
            });
        }
    }
    keys.push(ClientMsg::Key {
        keycode: 28,
        pressed: true,
    });
    keys.push(ClientMsg::Key {
        keycode: 28,
        pressed: false,
    });
    Some(keys)
}

fn us_key(ch: char) -> Option<(u32, bool)> {
    const ROWS: [(&str, &str, u32); 5] = [
        ("1234567890-=", "!@#$%^&*()_+", 2),
        ("qwertyuiop[]", "QWERTYUIOP{}", 16),
        ("asdfghjkl;'`", "ASDFGHJKL:\"~", 30),
        ("\\", "|", 43),
        ("zxcvbnm,./", "ZXCVBNM<>?", 44),
    ];
    ROWS.iter().find_map(|&(plain, shifted, start)| {
        plain
            .find(ch)
            .map(|column| (start + column as u32, false))
            .or_else(|| shifted.find(ch).map(|column| (start + column as u32, true)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: Geometry = Geometry {
        width: 1280,
        height: 720,
        scale: 2.,
    };

    #[test]
    fn focuses_and_clears_remote_field_before_entering_password() {
        assert!((b' '..=b'~').all(|ch| password_keys(&char::from(ch).to_string()).is_some()));
        assert_eq!(us_key('A'), Some((30, true)));
        assert_eq!(us_key('~'), Some((41, true)));
        assert_eq!(us_key('|'), Some((43, true)));
        assert_eq!(
            unlock_keys(OUTPUT, password_keys("!").unwrap()),
            vec![
                ClientMsg::PointerMotion { x: 320., y: 180. },
                ClientMsg::PointerButton {
                    button: 0x110,
                    pressed: true
                },
                ClientMsg::PointerButton {
                    button: 0x110,
                    pressed: false
                },
                ClientMsg::Key {
                    keycode: 1,
                    pressed: true
                },
                ClientMsg::Key {
                    keycode: 1,
                    pressed: false
                },
                ClientMsg::Key {
                    keycode: 42,
                    pressed: true
                },
                ClientMsg::Key {
                    keycode: 2,
                    pressed: true
                },
                ClientMsg::Key {
                    keycode: 2,
                    pressed: false
                },
                ClientMsg::Key {
                    keycode: 42,
                    pressed: false
                },
                ClientMsg::Key {
                    keycode: 28,
                    pressed: true
                },
                ClientMsg::Key {
                    keycode: 28,
                    pressed: false
                },
            ]
        );
        for invalid in ["", "aé", "a\r", "a\n", "a\u{1b}"] {
            assert!(password_keys(invalid).is_none());
        }
    }
}
