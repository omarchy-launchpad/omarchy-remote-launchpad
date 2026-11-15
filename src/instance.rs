//! A per-user lock shared by packaged and development builds.
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub fn acquire(path: &Path) -> io::Result<Option<File>> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing lock directory"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
    // Never unlink the lock file. That would allow two processes to lock
    // different inodes at the same path. The OS releases locks on exit.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn subprocess_lock_probe() {
        let Some(path) = std::env::var_os("SEAMS_TEST_INSTANCE_PATH") else {
            return;
        };
        let acquired = acquire(Path::new(&path)).unwrap().is_some();
        assert_eq!(
            acquired,
            std::env::var("SEAMS_TEST_EXPECT_LOCK").unwrap() == "yes"
        );
    }

    #[test]
    fn only_one_process_owns_lock_and_exit_releases_it() {
        let dir = std::env::temp_dir().join(format!("seams-instance-test-{}", std::process::id()));
        let path = dir.join("instance.lock");
        let first = acquire(&path).unwrap().unwrap();
        let probe = |expected| {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "instance::tests::subprocess_lock_probe"])
                .env("SEAMS_TEST_INSTANCE_PATH", &path)
                .env("SEAMS_TEST_EXPECT_LOCK", expected)
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(
            probe("no"),
            "a second process acquired the active instance lock"
        );
        drop(first);
        assert!(
            probe("yes"),
            "a new process could not acquire the released lock"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
