//! Blocking SSH and bridge I/O. AppKit only polls completed work and the latest frame.
use crate::config::Host;
#[path = "remote/window_stream.rs"]
mod window_stream;
use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};
pub use window_stream::WindowStream;

pub fn token() -> io::Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ssh(p: &Host, args: &[&str]) -> Command {
    let mut cmd = p.ssh_command();
    let mut remote = match p {
        Host::Ssh(_) => String::from(
            r#"agent="$HOME/.local/bin/x11-bridge-agent"; [ -x "$agent" ] || agent=$(command -v x11-bridge-agent)
[ -x "$agent" ] || { echo 'Install the current x11-bridge-agent in ~/.local/bin on Linux.' >&2; exit 1; }
"#,
        ),
        Host::LocalVm { .. } => String::from(
            r#"agent="$HOME/.local/bin/x11-bridge-agent"
[ -x "$agent" ] || { echo 'Use Set up access in Manage Machines to install the bridge agent.' >&2; exit 1; }
"#,
        ),
    };
    if matches!(p, Host::LocalVm { .. }) && args == ["list"] {
        remote.push_str(r#"extensions=$(systemd-run --user --pipe --wait --quiet xdpyinfo -queryExtensions) || {
    echo 'Log in to the Omarchy desktop and try again.' >&2
    exit 1
}
for ext in Composite DAMAGE X-Resource XTEST; do
    printf '%s\n' "$extensions" | grep -qw "$ext" || { echo "Missing Xwayland extension: $ext" >&2; exit 1; }
done
"#);
    }
    remote.push_str(r#"exec "$agent" --desktop"#);
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
    pub(crate) fn spawn(
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
                    let chunk = &buf[..n];
                    eprint!("{}", String::from_utf8_lossy(chunk));
                    tail.extend_from_slice(chunk);
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Checking,
    Online,
    Offline,
}

pub struct Reachability {
    state: Connection,
    probe: Option<(Instant, Task<()>)>,
    next_probe: Instant,
}

impl Reachability {
    pub fn new() -> Self {
        Self {
            state: Connection::Checking,
            probe: None,
            next_probe: Instant::now(),
        }
    }

    pub fn state(&self) -> Connection {
        self.state
    }

    pub fn tick(&mut self, host: &Host) -> Connection {
        const INTERVAL: Duration = Duration::from_secs(5);
        const TIMEOUT: Duration = Duration::from_secs(3);
        let now = Instant::now();
        if let Some((started, task)) = &self.probe {
            if now - *started >= TIMEOUT {
                self.probe = None;
                self.state = Connection::Offline;
            } else if let Some(result) = task.poll() {
                self.probe = None;
                self.state = if result.is_ok() {
                    Connection::Online
                } else {
                    Connection::Offline
                };
            }
        }
        if self.probe.is_none() && now >= self.next_probe {
            self.next_probe = now + INTERVAL;
            match Task::spawn(host.reachability_command(), |mut out, _in| {
                io::copy(&mut out, &mut io::sink())?;
                Ok(())
            }) {
                Ok(task) => self.probe = Some((now, task)),
                Err(_) => self.state = Connection::Offline,
            }
        }
        self.state
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub icon: Option<Arc<[u8]>>,
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
                icon: None,
            })
        })
        .collect()
}

pub fn catalogue(p: &Host) -> Result<Task<Vec<App>>, String> {
    Task::spawn(ssh(p, &["list"]), |stdout, _stdin| read_apps(stdout))
}

pub fn resource(name: &str) -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let path = executable.parent()?.join("../Resources").join(name);
    path.is_file().then_some(path)
}

pub fn agent_archive() -> Option<std::path::PathBuf> {
    resource("linux-agents.tar")
}

fn source_agent_installer() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    executable.ancestors().find_map(|directory| {
        let script = directory.join("scripts/install-dev-agent");
        (script.is_file() && directory.join("../x11-bridge/Cargo.toml").is_file()).then_some(script)
    })
}

pub fn can_install_agent() -> bool {
    agent_archive().is_some() || source_agent_installer().is_some()
}

pub fn install_agent(host: &Host) -> Result<Task<()>, String> {
    let Some(path) = agent_archive() else {
        if let Host::Ssh(destination) = host
            && let Some(installer) = source_agent_installer()
        {
            let mut command = Command::new("/bin/bash");
            command.arg(installer).arg(destination);
            return Task::spawn(command, |mut stdout, _stdin| {
                io::copy(&mut stdout, &mut io::sink()).map(|_| ())
            });
        }
        return Err("Use a release build to install the bridge agent from Launchpad.".into());
    };
    let mut archive = File::open(path).map_err(|error| error.to_string())?;
    let mut command = host.ssh_command();
    command.arg(
        r#"set -eu
        case "$(uname -m)" in
            aarch64|x86_64) agent="x11-bridge-agent-$(uname -m)";;
            *) echo 'The bundled bridge agent supports ARM64 and x86-64 Linux.' >&2; exit 1;;
        esac
        dir="$HOME/.local/bin"
        mkdir -p "$dir"
        tmp=$(mktemp "$dir/.x11-bridge-agent.XXXXXX")
        trap 'rm -f "$tmp"' EXIT
        tar -xOf - "$agent" > "$tmp"
        test -s "$tmp"
        chmod 755 "$tmp"
        mv -f "$tmp" "$dir/x11-bridge-agent""#,
    );
    Task::spawn(command, move |mut stdout, mut stdin| {
        let sent = io::copy(&mut archive, &mut stdin);
        drop(stdin);
        io::copy(&mut stdout, &mut io::sink())?;
        sent.map(|_| ())
    })
}

pub type Icons = HashMap<String, Arc<[u8]>>;

pub fn icons(host: &Host, apps: &[App]) -> Result<Task<Icons>, String> {
    let ids = apps
        .iter()
        .map(|app| app.id.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let script = include_str!("remote/icons.py").replace('\'', "'\\''");
    let mut command = host.ssh_command();
    command.arg(format!(
        "command -v python3 >/dev/null || exit 0; exec python3 -c '{script}'"
    ));
    Task::spawn(command, move |stdout, mut stdin| {
        // The helper can exit without reading when Python is unavailable.
        if let Err(error) = stdin.write_all(ids.as_bytes())
            && error.kind() != io::ErrorKind::BrokenPipe
        {
            return Err(error);
        }
        drop(stdin);
        read_icons(BufReader::new(stdout))
    })
}

fn read_icons(mut reader: impl BufRead) -> io::Result<Icons> {
    let invalid = || io::Error::other("Invalid remote icon catalogue");
    let mut remaining = 16 * 1024 * 1024;
    let mut icons = Icons::new();
    loop {
        let mut header = String::new();
        let size = reader.by_ref().take(4112).read_line(&mut header)?;
        if size == 0 {
            return Ok(icons);
        }
        let (id, length) = header
            .strip_suffix('\n')
            .and_then(|line| line.split_once('\t'))
            .ok_or_else(invalid)?;
        let length = length
            .parse::<usize>()
            .ok()
            .filter(|length| (24..=96 * 1024).contains(length))
            .ok_or_else(invalid)?;
        if id.is_empty()
            || id.len() > 4096
            || id.chars().any(char::is_control)
            || size + length > remaining
        {
            return Err(invalid());
        }
        remaining -= size + length;
        let mut data = vec![0; length];
        reader.read_exact(&mut data)?;
        if data.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR")
            && [16, 20].into_iter().all(|i| {
                (1..=128).contains(&u32::from_be_bytes(data[i..i + 4].try_into().unwrap()))
            })
        {
            icons.insert(id.into(), data.into());
        }
    }
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

pub fn stream(p: &Host, token: &str, xid: u32, app: &App) -> Result<WindowStream, String> {
    WindowStream::start(ssh(p, &["attach-app", token, &xid.to_string(), &app.id]))
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
