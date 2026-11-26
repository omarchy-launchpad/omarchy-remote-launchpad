use std::{env, fs, path::PathBuf, process::Command};

const FILE: &str = "seams.local";

pub fn directory() -> PathBuf {
    PathBuf::from(env::var_os("HOME").expect("HOME must exist"))
        .join("Library/Application Support/Seams")
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

    fn path() -> PathBuf {
        env::var_os("SEAMS_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                // Both target/debug/seams and dist/Seams.app live below the checkout.
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

    pub fn save(&self) -> std::io::Result<()> {
        use std::io::Write;
        let path = Self::path();
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent)?;
        let scratch = parent.join(format!(".seams-{}", crate::remote::token()?));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&scratch)?;
        let result = writeln!(file, "{}", self.name()).and_then(|_| fs::rename(&scratch, path));
        if result.is_err() {
            let _ = fs::remove_file(scratch);
        }
        result
    }

    pub fn load() -> Result<Self, String> {
        let path = Self::path();
        let value = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&value)
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Ssh(name) => name,
            Self::LocalVm { target, .. } => target,
        }
    }

    pub fn ssh_command(&self) -> Command {
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
            && let Some(path) = env::var_os("SEAMS_FIXTURE_SSH_CONFIG")
        {
            let path = PathBuf::from(path);
            cmd.arg("-S")
                .arg(path.with_extension("sock"))
                .arg("-F")
                .arg(path);
        }
        cmd.args([
            "-T",
            "-x",
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "-oControlMaster=auto",
            "-oControlPersist=120",
            "-oCompression=yes",
            "-oControlPath=~/.ssh/seams-z-%C",
            "-oConnectTimeout=8",
            "-oServerAliveInterval=10",
            "-oServerAliveCountMax=3",
            "--",
            destination,
        ]);
        cmd
    }
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
