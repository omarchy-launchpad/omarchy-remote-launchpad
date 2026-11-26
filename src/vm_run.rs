//! Client for the Try Omarchy run-mode protocol.
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

pub fn prepare_runtime(cache: PathBuf) -> Receiver<Result<PathBuf, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(install_runtime(&cache));
    });
    receiver
}

fn install_runtime(cache: &Path) -> Result<PathBuf, String> {
    let script = crate::remote::resource("install-vm-runtime")
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .ancestors()
                .map(|p| p.join("scripts/install-vm-runtime"))
                .find(|p| p.is_file())
        })
        .ok_or("Could not find the Omarchy runtime installer.")?;
    let output = Command::new("/bin/sh")
        .arg(script)
        .arg(cache)
        .output()
        .map_err(|error| format!("Could not prepare the Omarchy runtime. {error}"))?;
    if !output.status.success() {
        let errors = String::from_utf8_lossy(&output.stderr);
        return Err(errors
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Could not prepare the Omarchy runtime.")
            .into());
    }
    let helper = PathBuf::from(
        String::from_utf8(output.stdout)
            .map_err(|_| "Invalid Omarchy runtime path.")?
            .trim_end_matches('\n'),
    );
    if !helper.is_file() {
        return Err("Could not find the cached Omarchy runtime.".into());
    }
    Ok(helper)
}

pub struct VmRun {
    child: Child,
    // The helper treats EOF as a request to shut down the guest.
    _input: ChildStdin,
    events: Receiver<serde_json::Value>,
    message: String,
    failed: bool,
}

impl VmRun {
    pub fn start(
        helper: &Path,
        name: &str,
        storage: Option<&Path>,
        factory_cache: Option<&Path>,
    ) -> Result<Self, String> {
        let product = if name.chars().count() <= 64 && name.len() <= 255 {
            name
        } else {
            "Omarchy"
        };
        let mut config = serde_json::json!({
            "version": 1,
            "productName": product,
            "bridges": ["clipboard", "battery", "timezone"],
            "network": "shared",
        });
        if let Some(path) = storage {
            config["storageRoot"] = path.to_str().ok_or("Invalid VM storage path.")?.into();
        }
        let mut line = serde_json::to_vec(&config).map_err(|e| e.to_string())?;
        line.push(b'\n');

        let mut command = Command::new(helper);
        // Follow the upstream client. Inherited overrides can select another disk or network.
        for (key, _) in std::env::vars_os() {
            if ["OMARCHY_", "TRYOMARCHY_", "SDL_"]
                .iter()
                .any(|prefix| key.as_encoded_bytes().starts_with(prefix.as_bytes()))
            {
                command.env_remove(key);
            }
        }
        // Try Omarchy stores the decompressed factory beside each VM by default.
        // Point all Launchpad VMs at one shared cache so later VMs clone it.
        if let Some(path) = factory_cache {
            command.env("OMARCHY_QEMU_GPU_FACTORY_CACHE", path);
        }
        let mut child = command
            .arg("--run-vm")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not start the Omarchy runtime. {e}"))?;
        let mut input = child.stdin.take().expect("piped VM input");
        if let Err(error) = input.write_all(&line) {
            drop(input);
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Err(format!("Could not configure the Omarchy runtime. {error}"));
        }
        let mut stdout = BufReader::new(child.stdout.take().expect("piped VM output"));
        let (sender, events) = mpsc::channel();
        thread::spawn(move || {
            let mut line = Vec::new();
            while let Ok(size) = stdout.by_ref().take(16_385).read_until(b'\n', &mut line) {
                if size == 0 {
                    break;
                }
                if size <= 16_384
                    && let Ok(event) = serde_json::from_slice(&line)
                    && sender.send(event).is_err()
                {
                    break;
                }
                line.clear();
            }
        });
        Ok(Self {
            child,
            _input: input,
            events,
            message: "Starting Omarchy…".into(),
            failed: false,
        })
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn shutdown(mut self) -> Result<(), String> {
        drop(self._input);
        self.child
            .wait()
            .map_err(|error| format!("Could not stop the VM before removing its disk. {error}"))?;
        Ok(())
    }

    /// Returns whether the displayed status changed and whether the helper exited.
    pub fn poll(&mut self) -> (bool, bool) {
        let mut changed = false;
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    if event["version"].as_u64() != Some(1) {
                        continue;
                    }
                    let message = match event["event"].as_str() {
                        Some("error") => {
                            self.failed = true;
                            event["message"].as_str().map(str::to_owned)
                        }
                        Some("progress") if !self.failed => Some(progress(&event)),
                        Some("ready") if !self.failed => {
                            Some("VM started. Sign in to Omarchy.".into())
                        }
                        Some("stopping") if !self.failed => Some("Shutting down Omarchy…".into()),
                        _ => None,
                    };
                    if let Some(message) = message {
                        self.message = message;
                        changed = true;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return (changed, false),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return match self.child.try_wait() {
                        Ok(None) => (changed, false),
                        Ok(Some(_)) | Err(_) if self.failed => (changed, true),
                        Ok(Some(status)) if status.success() => {
                            self.message = "VM stopped.".into();
                            (true, true)
                        }
                        _ => {
                            self.message =
                                "Omarchy stopped unexpectedly. Try opening the VM again.".into();
                            (true, true)
                        }
                    };
                }
            }
        }
    }
}

fn progress(event: &serde_json::Value) -> String {
    let phase = match event["phase"].as_str() {
        Some("waiting-for-download") => "Waiting for the Omarchy image",
        Some("downloading") => "Downloading the Omarchy image",
        Some("reconnecting") => "Reconnecting to resume the download",
        Some("paused") => "Download paused until an unmetered connection is available",
        Some("verifying-download" | "verifying") => "Verifying the Omarchy image",
        Some("preparing" | "finishing") => "Preparing the VM disk",
        _ => "Starting Omarchy",
    };
    match event["percent"].as_u64().filter(|percent| *percent <= 100) {
        Some(percent) => format!("{phase} ({percent}%)"),
        None => format!("{phase}…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    #[test]
    fn starts_run_mode_for_the_selected_disk_and_keeps_its_input_open() {
        let root = std::env::temp_dir().join(format!(
            "launchpad-run-mode-{}",
            crate::remote::token().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let helper = root.join("omarchy-vm-helper");
        fs::write(
            &helper,
            r#"#!/bin/sh
set -eu
test "$1" = "--run-vm"
IFS= read -r config
printf '%s' "$config" > "$(dirname "$0")/config.json"
printf '%s\n' '{"version":1,"event":"progress","phase":"downloading","percent":45}'
printf '%s\n' '{"version":1,"event":"ready"}'
if IFS= read -r request; then exit 1; fi
touch "$(dirname "$0")/shutdown"
"#,
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

        let workspace = root.join("Development guest");
        let mut run = VmRun::start(&helper, "Development", Some(&workspace), None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !run.message().starts_with("VM started.") {
            let (_, exited) = run.poll();
            assert!(
                !exited,
                "VM exited before reaching ready: {}",
                run.message()
            );
            assert!(Instant::now() < deadline, "VM readiness timed out");
            thread::sleep(Duration::from_millis(10));
        }
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("config.json")).unwrap()).unwrap();
        assert_eq!(config["version"], 1);
        assert_eq!(config["productName"], "Development");
        assert_eq!(config["storageRoot"], workspace.to_str().unwrap());
        assert_eq!(config["network"], "shared");

        let shutdown = root.join("shutdown");
        assert!(!shutdown.exists());
        run.shutdown().unwrap();
        assert!(shutdown.is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
