use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const FILE: &str = "omarchy-launchpad.local";

pub fn directory() -> PathBuf {
    PathBuf::from(env::var_os("HOME").expect("HOME must exist"))
        .join("Library/Application Support/omarchy-launchpad")
}

#[derive(Clone, Debug)]
pub enum Host {
    Ssh(String),
    LocalVm {
        target: String,
        user: String,
        port: u16,
    },
}

impl Host {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let target = raw.trim();
        if let Some(address) = target.strip_prefix("vm://") {
            let (user, port) = address.rsplit_once(':').ok_or("Expected vm://user:port")?;
            let port: u16 = port.parse().map_err(|_| "Invalid VM SSH port")?;
            if port == 0
                || user.is_empty()
                || user.starts_with('-')
                || !user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
            {
                return Err("Invalid VM account or port".into());
            }
            return Ok(Self::LocalVm {
                target: target.into(),
                user: user.into(),
                port,
            });
        }
        let valid = |s: &str| {
            !s.is_empty()
                && !s.starts_with('-')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
        };
        let (user, host) = target.split_once('@').unwrap_or(("local", target));
        if !valid(user) || !valid(host) {
            return Err(format!(
                "Enter one SSH alias, user@host or vm://user:port in {FILE}."
            ));
        }
        Ok(Self::Ssh(target.into()))
    }

    pub fn path() -> PathBuf {
        env::var_os("OMARCHY_LAUNCHPAD_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                // Both target/debug/omarchy-launchpad and dist/omarchy-launchpad.app live below the checkout.
                let exe = env::current_exe().ok();
                exe.as_deref()
                    .and_then(|p| {
                        p.ancestors().find(|dir| {
                            dir.join("Cargo.toml").is_file() && dir.join(FILE).is_file()
                        })
                    })
                    .map(|dir| dir.join(FILE))
                    .unwrap_or_else(|| directory().join(FILE))
            })
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        write_atomic(path, format!("{}\n", self.name()).as_bytes())
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn load() -> Result<Self, String> {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        let value = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if value.trim().is_empty() {
            return Err("No machine selected.".into());
        }
        Self::parse(&value)
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Ssh(name) => name,
            Self::LocalVm { target, .. } => target,
        }
    }

    pub fn ssh_command(&self) -> Command {
        self.ssh(false)
    }

    pub fn reachability_command(&self) -> Command {
        self.ssh(true)
    }

    fn ssh(&self, probe: bool) -> Command {
        let mut cmd = Command::new("/usr/bin/ssh");
        let destination = match self {
            Self::LocalVm { user, port, .. } => {
                cmd.args([
                    "-F",
                    "/dev/null",
                    "-p",
                    &port.to_string(),
                    "-l",
                    user,
                    "-i",
                    "~/.ssh/seams-vm/client",
                    "-oIdentitiesOnly=yes",
                    "-oIdentityAgent=none",
                    "-oUserKnownHostsFile=~/.ssh/seams-vm/known_hosts",
                    "-oGlobalKnownHostsFile=/dev/null",
                    "-oControlPath=none",
                ]);
                "127.0.0.1"
            }
            Self::Ssh(host) => host,
        };
        #[cfg(test)]
        if matches!(self, Self::Ssh(_))
            && let Some(path) = env::var_os("OMARCHY_LAUNCHPAD_FIXTURE_SSH_CONFIG")
        {
            let path = PathBuf::from(path);
            if !probe {
                cmd.arg("-S").arg(path.with_extension("sock"));
            }
            cmd.arg("-F").arg(path);
        }
        if probe {
            // A fresh connection cannot get stuck waiting for a stale control master.
            cmd.args([
                "-q",
                "-S",
                "none",
                "-oControlMaster=no",
                "-oConnectTimeout=3",
            ]);
        } else {
            if matches!(self, Self::Ssh(_)) {
                cmd.args([
                    "-oControlMaster=auto",
                    "-oControlPersist=120",
                    "-oControlPath=~/.ssh/omarchy-launchpad-z-%C",
                ]);
            }
            cmd.args([
                "-oCompression=yes",
                "-oConnectTimeout=8",
                "-oServerAliveInterval=10",
                "-oServerAliveCountMax=3",
            ]);
        }
        cmd.args([
            "-T",
            "-x",
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "--",
            destination,
        ]);
        if probe {
            cmd.arg("true");
        }
        cmd
    }
}

pub fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    fs::create_dir_all(parent)?;
    let scratch = parent.join(format!(".omarchy-launchpad-{}", crate::remote::token()?));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&scratch)?;
    let result = file
        .write_all(contents)
        .and_then(|_| fs::rename(&scratch, path));
    if result.is_err() {
        let _ = fs::remove_file(scratch);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vm_destinations_use_only_the_local_port_and_dedicated_identity() {
        let host = Host::parse("vm://guest:2222\n").unwrap();
        let cmd = host.ssh_command();
        let args: Vec<_> = cmd.get_args().map(|arg| arg.to_str().unwrap()).collect();
        assert!(args.ends_with(&["--", "127.0.0.1"]));
        assert!(
            args.windows(4)
                .any(|pair| pair == ["-p", "2222", "-l", "guest"])
        );
        assert!(args.contains(&"-oStrictHostKeyChecking=yes"));
        assert!(args.contains(&"-oUserKnownHostsFile=~/.ssh/seams-vm/known_hosts"));
        for invalid in [
            "vm://",
            "vm://:22",
            "vm://-bad:22",
            "vm://a@elsewhere:22",
            "vm://guest:0",
            "vm://guest:65536",
            "vm://guest:22\n-oProxyCommand",
        ] {
            assert!(Host::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn destination_is_one_ssh_operand() {
        for good in ["studio\n", "user@box", "user@fd00::1"] {
            assert!(Host::parse(good).is_ok());
        }
        for bad in ["", "-F", "user@", "@host", "two\nlines", "a@b@c", "ssh box"] {
            assert!(Host::parse(bad).is_err());
        }
    }
}
