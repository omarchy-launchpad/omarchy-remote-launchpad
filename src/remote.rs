//! Blocking SSH and bridge I/O. AppKit only polls completed work and the latest frame.
use crate::config::Host;
use omarchy_seams::{
    client::{self, Event},
    messages::{CAP_CAPTURE, CAP_INPUT, CAP_RESIZE, Input},
};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

pub fn token() -> io::Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ssh(p: &Host, args: &[&str]) -> Command {
    let mut cmd = p.ssh_command();
    let mut remote = String::from(
        "agent=$(command -v x11-bridge-agent) || agent=\"$HOME/.local/bin/x11-bridge-agent\"; [ -x \"$agent\" ] || { echo 'Install the current x11-bridge-agent in ~/.local/bin on Linux.' >&2; exit 1; }; exec \"$agent\" --desktop",
    );
    for arg in args {
        remote.push_str(" '");
        remote.push_str(&arg.replace('\'', "'\\''"));
        remote.push('\'');
    }
    cmd.arg(remote);
    cmd
}

pub struct Task<T> {
    child: Arc<Mutex<Child>>,
    done: Receiver<Result<T, String>>,
}

impl<T: Send + 'static> Task<T> {
    fn spawn(
        mut cmd: Command,
        work: impl FnOnce(ChildStdout, ChildStdin) -> io::Result<T> + Send + 'static,
    ) -> Result<Self, String> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let child = Arc::new(Mutex::new(child));
        let process = child.clone();
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            let errors = thread::spawn(move || {
                let mut tail = Vec::new();
                let mut buf = [0; 1024];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > 4096 {
                        tail.drain(..tail.len() - 4096);
                    }
                }
                String::from_utf8_lossy(&tail).trim().to_owned()
            });
            let result = work(stdout, stdin);
            if result.is_err() {
                let _ = process.lock().unwrap().kill();
            }
            let status = loop {
                match process.lock().unwrap().try_wait() {
                    Ok(None) => (),
                    other => break other,
                }
                thread::sleep(Duration::from_millis(20));
            };
            let detail = errors.join().unwrap_or_default();
            let value = match (result, status) {
                (Ok(value), Ok(Some(status))) if status.success() => Ok(value),
                (Err(e), _) => Err(format!("{e} {detail}").trim().to_owned()),
                (_, status) => Err(if detail.is_empty() {
                    format!(
                        "SSH transport ended ({status:?}). Check host access and agent installation."
                    )
                } else {
                    detail
                }),
            };
            let _ = tx.send(value);
        });
        Ok(Self { child, done })
    }

    pub fn poll(&self) -> Option<Result<T, String>> {
        match self.done.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("SSH worker stopped unexpectedly.".into()))
            }
        }
    }
}
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub id: String,
    pub name: String,
}

fn read_apps(input: impl Read) -> io::Result<Vec<App>> {
    const LIMIT: u64 = 1024 * 1024;
    let mut data = String::new();
    input.take(LIMIT + 1).read_to_string(&mut data)?;
    if data.len() as u64 > LIMIT {
        return Err(io::Error::other("Application catalogue exceeds 1 MiB."));
    }
    data.lines()
        .map(|line| {
            let (id, name) = line.split_once('\t').ok_or_else(|| {
                io::Error::other("Invalid desktop catalogue. Update the Linux agent.")
            })?;
            if [id, name]
                .iter()
                .any(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control))
            {
                return Err(io::Error::other("Invalid desktop entry."));
            }
            Ok(App {
                id: id.into(),
                name: name.into(),
            })
        })
        .collect()
}

pub fn catalogue(p: &Host) -> Result<Task<Vec<App>>, String> {
    Task::spawn(ssh(p, &["list"]), |stdout, _stdin| read_apps(stdout))
}

pub struct Watch {
    pub task: Task<()>,
    pub windows: Receiver<u32>,
}
pub fn watch(p: &Host, token: &str, app: &App) -> Result<Watch, String> {
    let command = ssh(p, &["open", token, &app.id]);
    let (tx, windows) = mpsc::sync_channel(64);
    let task = Task::spawn(command, move |stdout, _keep_stdin_open| {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            if reader.by_ref().take(16).read_line(&mut line)? == 0 {
                return Ok(());
            }
            let id = line
                .strip_suffix('\n')
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|id| *id != 0)
                .ok_or_else(|| io::Error::other("Invalid X11 window ID from agent."))?;
            tx.try_send(id)
                .map_err(|_| io::Error::other("Too many pending windows."))?;
        }
    })?;
    Ok(Watch { task, windows })
}

#[derive(Default)]
pub struct Update {
    pub title: Option<String>,
    pub frame: Option<(u32, u32, Vec<u8>)>,
    pub hidden: bool,
}
pub enum Control {
    Input(Vec<Input>),
    Resize(u32, u32),
}
pub struct Stream {
    pub task: Task<()>,
    pub update: Arc<Mutex<Update>>,
    pub input: SyncSender<Control>,
}
pub fn stream(p: &Host, token: &str, xid: u32, app: &App) -> Result<Stream, String> {
    stream_command(ssh(p, &["attach-app", token, &xid.to_string(), &app.id]))
}
pub(crate) fn stream_command(command: Command) -> Result<Stream, String> {
    let update = Arc::new(Mutex::new(Update::default()));
    let state = update.clone();
    let (input, controls) = mpsc::sync_channel::<Control>(64);
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
            let mut pending = state.lock().unwrap();
            match event {
                Event::Window(info) => pending.title = Some(info.title),
                Event::Frame {
                    width,
                    height,
                    rgba,
                } => {
                    pending.hidden = false;
                    pending.frame = Some((width, height, rgba));
                }
                Event::Hidden => {
                    pending.frame = None;
                    pending.hidden = true;
                }
                Event::Destroyed => return Ok(()),
            }
        }
        Err(io::Error::other(
            "Window connection closed. Check SSH access to the host.",
        ))
    })?;
    Ok(Stream {
        task,
        update,
        input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalogue_preserves_names_and_rejects_broken_records() {
        assert_eq!(
            read_apps("org.edit.desktop\tÉditor\n".as_bytes()).unwrap()[0].name,
            "Éditor"
        );
        for data in ["missing-fields", "bad\tname\tmore", "\tname", "id\t"] {
            assert!(read_apps(data.as_bytes()).is_err());
        }
        assert!(read_apps(vec![b'x'; 1024 * 1024 + 1].as_slice()).is_err());
    }
    #[test]
    fn desktop_ids_remain_one_remote_argument() {
        let p = Host::parse("test").unwrap();
        let id = "a' b$(false).desktop";
        let cmd = ssh(&p, &["launch", "token", id]);
        let shell = cmd
            .get_args()
            .last()
            .unwrap()
            .to_str()
            .unwrap()
            .split_once(" --desktop ")
            .unwrap()
            .1;
        let output = Command::new("sh")
            .args(["-c", &format!("set -- {shell}; printf '%s' \"$3\"")])
            .output()
            .unwrap();
        assert_eq!(output.stdout, id.as_bytes());
    }
}
