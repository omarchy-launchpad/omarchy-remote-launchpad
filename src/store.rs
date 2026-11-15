use crate::model::Profile;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub fn directory() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME must exist"))
        .join("Library/Application Support/Seams")
}

fn encode(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
fn decode(s: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('\\') => '\\',
            Some('t') => '\t',
            Some('n') => '\n',
            Some('r') => '\r',
            _ => return Err("Invalid escape in saved connections.".into()),
        });
    }
    Ok(out)
}

pub fn serialize(profiles: &[Profile]) -> String {
    let mut s = "seams-v1\n".to_string();
    for p in profiles {
        let fields = [&p.name, &p.host, &p.user, &p.port, &p.identity, &p.command];
        s.push_str(&fields.map(|f| encode(f)).join("\t"));
        s.push('\t');
        s.push_str(&p.display.to_string());
        s.push('\n');
    }
    s
}

pub fn parse(s: &str) -> Result<Vec<Profile>, String> {
    let mut lines = s.lines();
    if lines.next() != Some("seams-v1") {
        return Err("Unknown connections file version. File left untouched.".into());
    }
    let mut out = Vec::new();
    for line in lines {
        let f = line
            .split('\t')
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        if f.len() != 7 {
            return Err("Invalid saved connection. File left untouched.".into());
        }
        let p = Profile {
            name: f[0].clone(),
            host: f[1].clone(),
            user: f[2].clone(),
            port: f[3].clone(),
            identity: f[4].clone(),
            command: f[5].clone(),
            display: f[6].parse().map_err(|_| "Invalid display number.")?,
        };
        p.validate()?;
        if out.iter().any(|other: &Profile| {
            other.host == p.host
                && other.user == p.user
                && other.port == p.port
                && other.display == p.display
        }) {
            return Err("Saved connections share the same host and display.".into());
        }
        out.push(p);
    }
    Ok(out)
}

pub fn load(path: &Path) -> Result<Vec<Profile>, String> {
    match fs::read_to_string(path) {
        Ok(s) => parse(&s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("No parent directory"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temp = path.with_extension(format!("{}.{stamp}.tmp", std::process::id()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| {
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_unicode_and_escapes() {
        let p = Profile {
            host: "host".into(),
            name: "仕事\t\\test".into(),
            command: "xterm -title 'hello world'".into(),
            ..Profile::default()
        };
        assert_eq!(
            parse(&serialize(std::slice::from_ref(&p))).unwrap(),
            vec![p]
        );
    }
    #[test]
    fn damaged_store_fails_without_partial_data() {
        for s in [
            "unknown\n",
            "seams-v1\nbroken",
            "seams-v1\nbad\\q\tx\tx\tx\tx\tx\t100",
        ] {
            assert!(parse(s).is_err());
        }
    }
    #[test]
    fn duplicate_display_rejected() {
        let p = Profile {
            host: "host".into(),
            ..Profile::default()
        };
        assert!(parse(&serialize(&[p.clone(), p])).is_err());
    }

    #[test]
    fn disk_store_is_private_and_replaced_atomically() {
        let root = std::env::temp_dir().join(format!("seams-store-test-{}", std::process::id()));
        let path = root.join("connections.tsv");
        let p = Profile {
            host: "test-host".into(),
            ..Profile::default()
        };
        atomic_write(&path, serialize(std::slice::from_ref(&p)).as_bytes()).unwrap();
        assert_eq!(load(&path).unwrap(), vec![p]);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        atomic_write(&path, serialize(&[]).as_bytes()).unwrap();
        assert!(load(&path).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
