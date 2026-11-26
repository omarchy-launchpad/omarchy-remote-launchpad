use super::Task;
use omarchy_launchpad::{
    client::{self, Event},
    messages::{CAP_CAPTURE, CAP_INPUT, CAP_RESIZE, Input},
    protocol::MAX_PAYLOAD,
};
use std::{
    cell::{Cell, RefCell},
    io::{self, BufReader},
    process::Command,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

const RESIZE_SETTLE: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct Update {
    pub title: Option<String>,
    pub frame: Option<(u32, u32, Vec<u8>)>,
    pub hidden: bool,
}

impl Update {
    fn record(&mut self, event: Event) -> bool {
        match event {
            Event::Window(info) => self.title = Some(info.title),
            Event::Frame {
                width,
                height,
                rgba,
            } => {
                self.hidden = false;
                self.frame = Some((width, height, rgba));
            }
            Event::Hidden => {
                self.frame = None;
                self.hidden = true;
            }
            Event::Destroyed => return false,
        }
        true
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Control {
    Input(Vec<Input>),
    Resize(u32, u32),
}

struct Delivery {
    sender: SyncSender<Control>,
    pending_motion: Option<(u32, u32)>,
    frame_size: (u32, u32),
    viewport: (f64, f64),
    resize_since: Option<Instant>,
    overloaded: bool,
}

impl Delivery {
    fn new(sender: SyncSender<Control>) -> Self {
        Self {
            sender,
            pending_motion: None,
            frame_size: (0, 0),
            viewport: (0., 0.),
            resize_since: None,
            overloaded: false,
        }
    }

    fn send_input(&mut self, mut inputs: Vec<Input>) {
        if let Some((x, y)) = self.pending_motion.take()
            && !matches!(
                inputs.first(),
                Some(Input::Motion { .. } | Input::ReleaseAll)
            )
        {
            inputs.insert(0, Input::Motion { x, y });
        }
        if let Err(TrySendError::Full(_)) = self.sender.try_send(Control::Input(inputs)) {
            // A dropped ordered batch can leave input held until detach.
            self.overloaded = true;
        }
    }

    fn flush_motion(&mut self) {
        if let Some((x, y)) = self.pending_motion.take()
            && let Err(TrySendError::Full(_)) = self
                .sender
                .try_send(Control::Input(vec![Input::Motion { x, y }]))
        {
            self.pending_motion = Some((x, y));
        }
    }

    fn apply_update(&mut self, update: &Update) {
        if update.hidden {
            self.pending_motion = None;
        }
        if let Some((width, height, _)) = &update.frame {
            let size = (*width, *height);
            if self.frame_size != size {
                self.pending_motion = None;
                self.frame_size = size;
            }
        }
    }

    fn sync_viewport(&mut self, size: (f64, f64), live_resize: bool, now: Instant) {
        if self.frame_size == (0, 0) {
            return;
        }
        if self.viewport != size {
            self.viewport = size;
            self.resize_since = Some(now);
        }
        if live_resize
            || !self
                .resize_since
                .is_some_and(|since| now - since >= RESIZE_SETTLE)
        {
            return;
        }
        let (w, h) = (size.0.round() as u32, size.1.round() as u32);
        self.resize_since = None;
        if w == 0
            || h == 0
            || w > 16384
            || h > 16384
            || u64::from(w) * u64::from(h) * 4 + 8 > MAX_PAYLOAD as u64
            || self.frame_size == (w, h)
        {
            return;
        }
        if let Err(TrySendError::Full(_)) = self.sender.try_send(Control::Resize(w, h)) {
            self.resize_since = Some(now);
        }
    }
}

struct Connection {
    task: Task<()>,
    updates: Arc<Mutex<Update>>,
    delivery: Delivery,
}

/// Presents window updates and owns control delivery for one SSH connection.
pub struct WindowStream {
    connection: RefCell<Option<Connection>>,
    closing: Cell<bool>,
}

impl WindowStream {
    pub(super) fn start(command: Command) -> Result<Self, String> {
        let updates = Arc::new(Mutex::new(Update::default()));
        let state = updates.clone();
        let (sender, controls) = mpsc::sync_channel::<Control>(64);
        let task = Task::spawn(command, move |stdout, stdin| {
            let (mut reader, mut writer) = client::connect_optional(
                BufReader::new(stdout),
                stdin,
                CAP_CAPTURE | CAP_INPUT,
                CAP_RESIZE,
            )?;
            thread::spawn(move || {
                while let Ok(control) = controls.recv() {
                    let result = match control {
                        Control::Input(batch) => {
                            batch.into_iter().try_for_each(|event| writer.input(event))
                        }
                        Control::Resize(w, h) if writer.info().capabilities & CAP_RESIZE != 0 => {
                            writer.resize(w, h)
                        }
                        Control::Resize(..) => Ok(()),
                    };
                    if let Err(e) = result
                        && e.kind() != io::ErrorKind::WouldBlock
                    {
                        return;
                    }
                }
            });
            while let Some(event) = reader.next_event()? {
                if !state.lock().unwrap().record(event) {
                    return Ok(());
                }
            }
            Err(io::Error::other(
                "Window connection closed. Check SSH access to the host.",
            ))
        })?;
        Ok(Self {
            connection: RefCell::new(Some(Connection {
                task,
                updates,
                delivery: Delivery::new(sender),
            })),
            closing: Cell::new(false),
        })
    }

    pub fn frame_size(&self) -> (u32, u32) {
        self.connection
            .borrow()
            .as_ref()
            .map_or((0, 0), |connection| connection.delivery.frame_size)
    }

    pub fn poll(&self) -> Result<Option<Update>, String> {
        if self.closing.get() {
            return Ok(None);
        }
        let mut connection = self.connection.borrow_mut();
        let Some(connection) = connection.as_mut() else {
            return Ok(None);
        };
        if let Some(done) = connection.task.poll() {
            return done.map(|()| None);
        }
        if connection.delivery.overloaded {
            return Err("Input queue filled. Open the app again from Launchpad.".into());
        }
        let update = std::mem::take(&mut *connection.updates.lock().unwrap());
        connection.delivery.apply_update(&update);
        Ok(Some(update))
    }

    fn with_delivery(&self, apply: impl FnOnce(&mut Delivery)) {
        if let Some(connection) = self.connection.borrow_mut().as_mut() {
            apply(&mut connection.delivery);
        }
    }

    pub fn pointer_motion(&self, x: u32, y: u32) {
        self.with_delivery(|delivery| delivery.pending_motion = Some((x, y)));
    }

    pub fn send_input(&self, inputs: Vec<Input>) {
        self.with_delivery(|delivery| delivery.send_input(inputs));
    }

    pub fn release_input(&self) {
        self.send_input(vec![Input::ReleaseAll]);
    }

    pub fn request_close(&self) {
        self.closing.set(true);
        self.release_input();
    }

    pub fn detach(&self) {
        // AppKit can retain the view after the viewer has closed.
        drop(self.connection.borrow_mut().take());
    }

    pub fn sync_controls(&self, size: (f64, f64), live_resize: bool) {
        self.with_delivery(|delivery| {
            delivery.flush_motion();
            delivery.sync_viewport(size, live_resize, Instant::now());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::TryRecvError;

    #[test]
    fn latest_visibility_update_wins_and_discards_stale_motion() {
        let frame = |value| Event::Frame {
            width: 1,
            height: 1,
            rgba: vec![value; 4],
        };
        let mut update = Update::default();
        assert!(update.record(frame(1)));
        assert!(update.record(Event::Hidden));
        assert!(update.hidden && update.frame.is_none());

        let (sender, _) = mpsc::sync_channel(1);
        let mut delivery = Delivery::new(sender);
        delivery.frame_size = (1, 1);
        delivery.pending_motion = Some((0, 0));
        delivery.apply_update(&update);
        assert_eq!(delivery.pending_motion, None);

        assert!(update.record(frame(2)));
        assert!(update.record(frame(3)));
        assert!(!update.hidden);
        assert_eq!(update.frame, Some((1, 1, vec![3; 4])));

        delivery.frame_size = (2, 2);
        delivery.pending_motion = Some((1, 1));
        delivery.apply_update(&update);
        assert_eq!(delivery.frame_size, (1, 1));
        assert_eq!(delivery.pending_motion, None);
        assert!(!update.record(Event::Destroyed));
    }

    #[test]
    fn saturated_motion_retries_latest_position_without_detaching() {
        let (sender, controls) = mpsc::sync_channel(1);
        let mut delivery = Delivery::new(sender);
        delivery.send_input(vec![Input::ReleaseAll]);
        delivery.pending_motion = Some((1, 2));
        delivery.flush_motion();
        assert_eq!(delivery.pending_motion, Some((1, 2)));
        assert!(!delivery.overloaded);

        delivery.pending_motion = Some((3, 4));
        assert_eq!(
            controls.try_recv(),
            Ok(Control::Input(vec![Input::ReleaseAll]))
        );
        delivery.flush_motion();
        assert_eq!(
            controls.try_recv(),
            Ok(Control::Input(vec![Input::Motion { x: 3, y: 4 }]))
        );

        let key = Input::Key {
            keysym: 97,
            pressed: true,
        };
        delivery.pending_motion = Some((5, 6));
        delivery.send_input(vec![key.clone()]);
        assert_eq!(
            controls.try_recv(),
            Ok(Control::Input(vec![Input::Motion { x: 5, y: 6 }, key]))
        );
        assert_eq!(delivery.pending_motion, None);
    }

    #[test]
    fn saturated_resize_retries_after_the_viewport_settles_again() {
        let (sender, controls) = mpsc::sync_channel(1);
        let mut delivery = Delivery::new(sender);
        delivery.frame_size = (96, 64);
        delivery.send_input(vec![Input::ReleaseAll]);
        let start = Instant::now();
        let size = (320., 180.);
        delivery.sync_viewport(size, true, start);
        delivery.sync_viewport(size, false, start + RESIZE_SETTLE);

        assert_eq!(
            controls.try_recv(),
            Ok(Control::Input(vec![Input::ReleaseAll]))
        );
        delivery.sync_viewport(
            size,
            false,
            start + RESIZE_SETTLE + Duration::from_millis(99),
        );
        assert_eq!(controls.try_recv(), Err(TryRecvError::Empty));
        assert!(!delivery.overloaded);
        delivery.sync_viewport(size, false, start + RESIZE_SETTLE * 2);
        assert_eq!(controls.try_recv(), Ok(Control::Resize(320, 180)));
    }

    #[test]
    fn focus_release_clears_motion_and_saturation_ends_the_stream() {
        let (sender, controls) = mpsc::sync_channel(1);
        let task = Task::spawn(Command::new("/bin/cat"), |mut stdout, _stdin| {
            io::copy(&mut stdout, &mut io::sink()).map(|_| ())
        })
        .unwrap();
        let stream = WindowStream {
            connection: RefCell::new(Some(Connection {
                task,
                updates: Arc::default(),
                delivery: Delivery::new(sender),
            })),
            closing: Cell::new(false),
        };
        let key = Input::Key {
            keysym: 97,
            pressed: true,
        };

        stream.pointer_motion(1, 2);
        stream.release_input();
        assert_eq!(
            controls.try_recv(),
            Ok(Control::Input(vec![Input::ReleaseAll]))
        );
        stream.sync_controls((0., 0.), false);
        assert_eq!(controls.try_recv(), Err(TryRecvError::Empty));

        stream.send_input(vec![key.clone()]);
        stream.release_input();
        assert!(matches!(stream.poll(), Err(error) if error.contains("Input queue filled")));
        stream.request_close();
        assert!(matches!(stream.poll(), Ok(None)));
        stream.detach();
        assert_eq!(controls.try_recv(), Ok(Control::Input(vec![key])));
        assert_eq!(controls.try_recv(), Err(TryRecvError::Disconnected));
    }
}
