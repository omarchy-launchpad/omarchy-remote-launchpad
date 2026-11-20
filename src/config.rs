use std::{env, fs, path::PathBuf, process::Command};

const FILE: &str = "seams.local";

pub fn directory() -> PathBuf {
    PathBuf::from(env::var_os("HOME").expect("HOME must exist"))
        .join("Library/Application Support/Seams")
}

#[derive(Clone, Debug)]
pub struct Host(String);

impl Host {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let target = raw.trim();
        let valid = |s: &str| {
            !s.is_empty()
                && !s.starts_with('-')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
        };
        let (user, host) = target.split_once('@').unwrap_or(("local", target));
        if !valid(user) || !valid(host) {
            return Err(format!("Enter one SSH alias or user@host in {FILE}."));
        }
        Ok(Self(target.into()))
    }

    pub fn load() -> Result<Self, String> {
        let path = env::var_os("SEAMS_CONFIG")
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
            });
        let value = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&value)
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn ssh_command(&self) -> Command {
        let mut cmd = Command::new("/usr/bin/ssh");
        #[cfg(test)]
        if let Some(path) = env::var_os("SEAMS_FIXTURE_SSH_CONFIG") {
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
            self.name(),
        ]);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
