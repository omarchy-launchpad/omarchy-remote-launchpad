//! Saved machine profiles. The existing target file still selects the active host.
use crate::{
    config::{self, Host},
    remote,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
pub struct Machine {
    pub name: String,
    #[serde(flatten)]
    pub connection: MachineConnection,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineConnection {
    Remote {
        destination: String,
    },
    Vm {
        workspace: Option<String>,
        user: String,
        port: u16,
    },
}

impl Machine {
    pub fn from_host(host: &Host) -> Self {
        match host {
            Host::Ssh(destination) => Self {
                name: destination.clone(),
                connection: MachineConnection::Remote {
                    destination: destination.clone(),
                },
            },
            Host::LocalVm { user, port, .. } => Self {
                name: format!("Omarchy VM ({port})"),
                connection: MachineConnection::Vm {
                    workspace: None,
                    user: user.clone(),
                    port: *port,
                },
            },
        }
    }

    pub fn host(&self) -> Result<Host, String> {
        match &self.connection {
            MachineConnection::Remote { destination } => Host::parse(destination),
            MachineConnection::Vm { user, port, .. } => vm_host(user, *port),
        }
    }

    pub fn matches(&self, host: &Host) -> bool {
        match (&self.connection, host) {
            (MachineConnection::Remote { destination }, Host::Ssh(current)) => {
                destination == current
            }
            (MachineConnection::Vm { port, .. }, Host::LocalVm { port: active, .. }) => {
                port == active
            }
            _ => false,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self.connection {
            MachineConnection::Remote { .. } => "Remote",
            MachineConnection::Vm { .. } => "VM",
        }
    }

    pub fn address(&self) -> String {
        match &self.connection {
            MachineConnection::Remote { destination } => destination.clone(),
            MachineConnection::Vm { port, user, .. } if !user.is_empty() => {
                format!("{user}@127.0.0.1:{port}")
            }
            MachineConnection::Vm { port, .. } => format!("127.0.0.1:{port}"),
        }
    }

    pub fn vm_storage(&self, root: &Path) -> Result<Option<PathBuf>, String> {
        let MachineConnection::Vm {
            workspace, port, ..
        } = &self.connection
        else {
            return Err("Select a VM first.".into());
        };
        let Some(workspace) = workspace else {
            return Ok(None);
        };
        if workspace.len() != 32 || !workspace.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Invalid VM workspace ID.".into());
        }
        let path = root.join(workspace);
        initialize_workspace(&path, *port)
            .map_err(|error| format!("Could not prepare the VM workspace. {error}"))?;
        Ok(Some(path))
    }
}

fn initialize_workspace(path: &Path, port: u16) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    match fs::symlink_metadata(path) {
        Ok(existing) if existing.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "VM workspace is not a directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let parent = path.parent().expect("VM storage has a parent");
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".initialize-{}", remote::token()?));
    fs::DirBuilder::new().mode(0o700).create(&staging)?;

    // Try Omarchy 1.4.1 loads forwarding from its private workspace file.
    // Its full launcher discards port-forwarding environment overrides.
    let settings = serde_json::json!({
        "schemaVersion": 1,
        "settings": {
            "networkPreferences": {
                "version": 1,
                "preferences": {
                    "mode": "nat",
                    "interface": "",
                    "wifiCompatibility": false,
                    "bridgedSSH": false,
                },
            },
            "portForwardingPreferences": {
                "schemaVersion": 1,
                "mappings": [{ "hostPort": port, "guestPort": 22, "protocol": "tcp" }],
            },
        },
    });
    let result = config::write_atomic(
        &staging.join("settings.json"),
        settings.to_string().as_bytes(),
    )
    .and_then(|()| {
        config::write_atomic(
            &staging.join(".omarchy-qemu-storage"),
            b"omarchy-qemu-storage-root-v1\n",
        )
    })
    .and_then(|()| fs::rename(&staging, path));
    if result.is_err() {
        let _ = fs::remove_dir_all(staging);
    }
    result
}

pub fn vm_host(user: &str, port: u16) -> Result<Host, String> {
    let user = user.trim();
    let valid_start = user
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !valid_start {
        return Err("Enter your Omarchy guest username.".into());
    }
    Host::parse(&format!("vm://{user}:{port}"))
        .map_err(|_| "Enter a valid Omarchy guest username.".into())
}

pub fn create_vm(
    name: &str,
    saved: &[Machine],
    port_in_use: impl Fn(u16) -> bool,
) -> Result<Machine, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("Enter a VM name of 1 to 80 characters.".into());
    }
    let port = (2222..=65535)
        .find(|port| {
            !port_in_use(*port)
                && !saved.iter().any(|machine| matches!(machine.connection, MachineConnection::Vm { port: saved, .. } if saved == *port))
                && TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_ok()
        })
        .ok_or("No free SSH port for a new VM.")?;
    let workspace = remote::token().map_err(|error| error.to_string())?;
    Ok(Machine {
        name: name.into(),
        connection: MachineConnection::Vm {
            workspace: Some(workspace),
            user: String::new(),
            port,
        },
    })
}

pub fn load(path: &Path) -> Result<Vec<Machine>, String> {
    match fs::read(path) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|error| format!("Could not read saved machines. {error}")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("Could not read saved machines. {error}")),
    }
}

pub fn save(path: &Path, machines: &[Machine]) -> Result<(), String> {
    let contents = serde_json::to_vec_pretty(machines).map_err(|error| error.to_string())?;
    config::write_atomic(path, &contents)
        .map_err(|error| format!("Could not save machines. {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn multiple_machines_keep_their_addresses_and_independent_vm_workspaces() {
        let root =
            std::env::temp_dir().join(format!("launchpad-machines-{}", remote::token().unwrap()));
        let first = Host::parse("studio").unwrap();
        let second = Host::parse("user@backup").unwrap();
        let mut saved = vec![Machine::from_host(&first), Machine::from_host(&second)];
        saved.push(create_vm("Development", &saved, |_| false).unwrap());
        saved.push(create_vm("Testing", &saved, |_| false).unwrap());

        let store = root.join("machines.json");
        save(&store, &saved).unwrap();
        let saved = load(&store).unwrap();
        assert_eq!(saved.len(), 4);
        assert!(saved[0].matches(&first));
        assert!(saved[1].matches(&second));
        assert_eq!(saved[2].name, "Development");
        assert_eq!(saved[3].name, "Testing");
        assert_ne!(saved[2].address(), saved[3].address());

        let vm_root = root.join("VMs");
        let first_storage = saved[2].vm_storage(&vm_root).unwrap().unwrap();
        let second_storage = saved[3].vm_storage(&vm_root).unwrap().unwrap();
        assert_ne!(first_storage, second_storage);
        for machine in &saved[2..] {
            let MachineConnection::Vm {
                workspace: Some(id),
                port,
                ..
            } = &machine.connection
            else {
                panic!("expected an independent VM");
            };
            let workspace = vm_root.join(id);
            let settings = workspace.join("settings.json");
            let marker = workspace.join(".omarchy-qemu-storage");
            assert_eq!(
                settings.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                marker.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
            let initial = fs::read_to_string(&settings).unwrap();
            let mut settings_json: serde_json::Value = serde_json::from_str(&initial).unwrap();
            assert_eq!(
                settings_json["settings"]["portForwardingPreferences"]["mappings"][0],
                serde_json::json!({ "hostPort": port, "guestPort": 22, "protocol": "tcp" })
            );
            settings_json["settings"]["userPreference"] = serde_json::json!(true);
            fs::write(&settings, settings_json.to_string()).unwrap();
            machine.vm_storage(&vm_root).unwrap();
            assert_eq!(
                fs::read_to_string(&settings).unwrap(),
                settings_json.to_string()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
