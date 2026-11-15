use std::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: String,
    pub identity: String,
    pub command: String,
    pub display: u16,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "New connection".into(),
            host: String::new(),
            user: String::new(),
            port: String::new(),
            identity: String::new(),
            command: "xterm".into(),
            display: 100,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Launch,
    Reconnect,
    Stop,
}

impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give this connection a name.".into());
        }
        if self.host.is_empty()
            || self.host.starts_with('-')
            || !self
                .host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-:".contains(c))
        {
            return Err(
                "Enter a hostname, IP address, or SSH config alias without user@ or spaces.".into(),
            );
        }
        if !self
            .user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            || self.user.starts_with('-')
        {
            return Err("SSH user contains invalid characters.".into());
        }
        if !self.port.is_empty() && self.port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
            return Err("Port must be between 1 and 65535, or empty to use SSH config.".into());
        }
        if self.display < 100 {
            return Err("Choose a dedicated display number between 100 and 65535.".into());
        }
        if self.command.trim().is_empty() || self.command.chars().any(char::is_control) {
            return Err("Enter a single-line remote application command.".into());
        }
        if self.identity.chars().any(char::is_control) {
            return Err("Invalid identity file path.".into());
        }
        Ok(())
    }

    pub fn destination(&self) -> String {
        if self.user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        }
    }

    pub fn uri(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "ssh://{}{host}{}/{}",
            if self.user.is_empty() {
                String::new()
            } else {
                format!("{}@", self.user)
            },
            if self.port.is_empty() {
                String::new()
            } else {
                format!(":{}", self.port)
            },
            self.display
        )
    }

    pub fn ssh_options(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "-oConnectTimeout=10",
            "-oServerAliveInterval=10",
            "-oServerAliveCountMax=3",
        ]
        .map(String::from)
        .into();
        if !self.port.is_empty() {
            args.extend(["-p".into(), self.port.clone()]);
        }
        if !self.identity.is_empty() {
            args.extend(["-i".into(), self.identity.clone()]);
        }
        args
    }

    pub fn check_command(&self) -> Command {
        let mut c = Command::new("/usr/bin/ssh");
        c.args(self.ssh_options()).arg("--").arg(self.destination())
            .arg("command -v xpra >/dev/null || { echo 'Install xpra on this Linux host first.'; exit 1; }; xpra --version || exit 1; if command -v Xvfb >/dev/null || command -v Xorg >/dev/null; then echo 'X11 server available'; else echo 'Install Xvfb or an xpra-compatible Xorg server.'; exit 1; fi");
        c
    }

    pub fn xpra_args(&self, action: Action) -> Vec<String> {
        let ssh = std::iter::once("/usr/bin/ssh".to_string())
            .chain(self.ssh_options())
            .map(|s| shell_quote(&s))
            .collect::<Vec<_>>()
            .join(" ");
        let mut args = vec![
            match action {
                Action::Launch => "start",
                Action::Reconnect => "attach",
                Action::Stop => "stop",
            }
            .into(),
            self.uri(),
            format!("--ssh={ssh}"),
        ];
        if matches!(action, Action::Launch) {
            args.extend([
                "--attach=yes".into(),
                "--exit-with-children=no".into(),
                "--exit-with-client=no".into(),
                format!("--session-name=Seams {}", self.name),
                format!(
                    "--start-child=env GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb {}",
                    self.command
                ),
            ]);
        }
        if !matches!(action, Action::Stop) {
            args.extend(
                [
                    "--clipboard=yes",
                    "--speaker=off",
                    "--microphone=off",
                    "--notifications=no",
                    "--splash=no",
                ]
                .map(String::from),
            );
        }
        args
    }
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile {
        Profile {
            host: "linux-box".into(),
            ..Profile::default()
        }
    }
    #[test]
    fn aliases_preserve_ssh_config() {
        let p = profile();
        assert_eq!(p.uri(), "ssh://linux-box/100");
        assert!(!p.ssh_options().iter().any(|s| s == "-p"));
    }
    #[test]
    fn user_port_and_ipv6() {
        let p = Profile {
            host: "fd00::1".into(),
            user: "demo".into(),
            port: "2222".into(),
            ..profile()
        };
        assert!(p.validate().is_ok());
        assert_eq!(p.uri(), "ssh://demo@[fd00::1]:2222/100");
    }
    #[test]
    fn reject_option_and_uri_injection() {
        for host in [
            "-F",
            "foo bar",
            "host/100",
            "user@host",
            "x;touch /tmp/pwn",
            "",
        ] {
            assert!(
                Profile {
                    host: host.into(),
                    ..profile()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn paths_and_commands_stay_single_arguments() {
        let p = Profile {
            identity: "/tmp/it's a key".into(),
            command: "firefox --no-remote 'https://example.com'".into(),
            ..profile()
        };
        let args = p.xpra_args(Action::Launch);
        assert!(args.iter().any(|s| s.contains("'/tmp/it'\"'\"'s a key'")));
        assert!(args.iter().any(|s| s == "--start-child=env GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb firefox --no-remote 'https://example.com'"));
        assert!(args.iter().any(|s| s.contains("StrictHostKeyChecking=yes")));
    }
    #[test]
    fn reconnect_does_not_start_duplicate_application() {
        assert!(
            !profile()
                .xpra_args(Action::Reconnect)
                .iter()
                .any(|s| s.starts_with("--start"))
        );
        assert_eq!(profile().xpra_args(Action::Stop)[0], "stop");
    }
}
