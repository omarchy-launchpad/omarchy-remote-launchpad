use crate::model::{Action, Profile};
use std::os::unix::process::CommandExt;
use std::{
    collections::VecDeque,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

fn terminate_group(child: &mut Child) {
    // SAFETY: spawn creates a process group with this child's PID.
    // A negative PID addresses only that group, including SSH subprocesses.
    unsafe {
        kill(-(child.id() as i32), 15);
    }
}

pub fn find_xpra(custom: &str) -> Option<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if !custom.is_empty() {
        paths.push(custom.into());
    }
    for app in ["/Applications/Xpra.app", "/Applications/Xpra"] {
        for bin in ["Xpra", "xpra"] {
            paths.push(Path::new(app).join("Contents/MacOS").join(bin));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join("Applications/Xpra.app/Contents/MacOS/Xpra"));
    }
    paths.extend([
        "/opt/homebrew/bin/xpra".into(),
        "/usr/local/bin/xpra".into(),
    ]);
    paths.into_iter().find(|p| {
        use std::os::unix::fs::PermissionsExt;
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    })
}

pub struct Job {
    pub child: Child,
    pub display: u16,
    pub title: String,
    pub check: bool,
}

pub struct Sessions {
    pub jobs: Vec<Job>,
    pub lines: VecDeque<String>,
    sender: SyncSender<String>,
    receiver: Receiver<String>,
}

impl Default for Sessions {
    fn default() -> Self {
        let (sender, receiver) = mpsc::sync_channel(256);
        Self {
            jobs: Vec::new(),
            lines: VecDeque::new(),
            sender,
            receiver,
        }
    }
}

impl Sessions {
    pub fn log(&mut self, text: impl Into<String>) {
        // Bound UI log storage even when an engine emits very long lines.
        let text = text.into();
        let bounded: String = text.chars().take(2000).collect();
        self.lines.push_back(bounded);
        while self.lines.len() > 180 {
            self.lines.pop_front();
        }
    }

    pub fn spawn(
        &mut self,
        mut command: Command,
        profile: &Profile,
        check: bool,
    ) -> Result<(), String> {
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        for mut stream in [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)] {
            let tx = self.sender.clone();
            let name = profile.name.clone();
            thread::spawn(move || {
                let mut buffer = [0u8; 2048];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let _ = tx.try_send(format!(
                                "[{name}] {}",
                                String::from_utf8_lossy(&buffer[..n])
                            ));
                        }
                    }
                }
            });
        }
        self.jobs.push(Job {
            child,
            display: profile.display,
            title: profile.name.clone(),
            check,
        });
        Ok(())
    }

    pub fn start(&mut self, xpra: &Path, profile: &Profile, action: Action) -> Result<(), String> {
        profile.validate()?;
        if self
            .jobs
            .iter()
            .any(|j| j.display == profile.display && !j.check)
        {
            return Err("A client or session command is already running for this display. Disconnect it first.".into());
        }
        let mut command = Command::new(xpra);
        command.args(profile.xpra_args(action));
        self.log(format!("{}: {action:?} {}", profile.name, profile.uri()));
        self.spawn(command, profile, false)
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        // Limit each tick so an active video engine cannot starve the AppKit loop.
        for _ in 0..100 {
            let Ok(line) = self.receiver.try_recv() else {
                break;
            };
            self.log(line);
            changed = true;
        }
        let mut ended = Vec::new();
        for (i, job) in self.jobs.iter_mut().enumerate() {
            match job.child.try_wait() {
                Ok(Some(status)) => ended.push((
                    i,
                    format!(
                        "{}: {} exited ({status}). {}",
                        job.title,
                        if job.check {
                            "SSH check"
                        } else {
                            "Client / session command"
                        },
                        if job.check {
                            if status.success() {
                                "Host prerequisites found."
                            } else {
                                "Check SSH trust, key access and remote dependencies."
                            }
                        } else {
                            "Remote apps may still be running. Use Reconnect to return."
                        }
                    ),
                )),
                Err(e) => ended.push((i, format!("{}: {e}", job.title))),
                _ => {}
            }
        }
        for (i, message) in ended.into_iter().rev() {
            self.jobs.remove(i);
            self.log(message);
            changed = true;
        }
        changed
    }

    pub fn disconnect(&mut self, display: Option<u16>) {
        for job in &mut self.jobs {
            if display.is_none_or(|d| d == job.display) {
                // Kill only our local client/check, never the remote server.
                terminate_group(&mut job.child);
            }
        }
        self.log("Local clients disconnected. Remote applications keep running.");
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for job in &mut self.jobs {
            terminate_group(&mut job.child);
            let _ = job.child.kill();
            let _ = job.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn pump_until(s: &mut Sessions, done: impl Fn(&Sessions) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            s.poll();
            if done(s) {
                return;
            }
            assert!(Instant::now() < deadline, "local process did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn process_diagnostics_and_failure_are_reported() {
        let mut s = Sessions::default();
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "echo stdout-test; echo stderr-test >&2; exit 7"]);
        s.spawn(c, &Profile::default(), true).unwrap();
        pump_until(&mut s, |s| {
            s.jobs.is_empty()
                && s.lines.iter().any(|l| l.contains("stderr-test"))
                && s.lines.iter().any(|l| l.contains("stdout-test"))
        });
        assert!(s.lines.iter().any(|l| l.contains("exit status: 7")));
    }

    #[test]
    fn disconnect_reaps_own_process_group() {
        let mut s = Sessions::default();
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "exec sleep 30"]);
        s.spawn(c, &Profile::default(), false).unwrap();
        s.disconnect(Some(100));
        pump_until(&mut s, |s| s.jobs.is_empty());
    }

    #[test]
    fn log_storage_is_bounded() {
        let mut s = Sessions::default();
        for _ in 0..300 {
            s.log("x".repeat(3000));
        }
        assert_eq!(s.lines.len(), 180);
        assert!(s.lines.iter().all(|l| l.len() == 2000));
    }
}
