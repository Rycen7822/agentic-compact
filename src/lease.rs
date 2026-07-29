use crate::error::{Error, ErrorCode, Result};
use crate::observability::hash_identifier;
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ThreadLease {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl ThreadLease {
    pub fn acquire(thread_id: &str) -> Result<Self> {
        Self::acquire_in(&state_root()?.join("locks"), thread_id)
    }

    fn acquire_in(directory: &Path, thread_id: &str) -> Result<Self> {
        secure_dir(directory)?;
        let path = directory.join(format!("{}.lock", hash_identifier(thread_id)));
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        secure_file(&path)?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::WouldBlock {
                ErrorCode::TransitionPending
            } else {
                ErrorCode::Io
            };
            Error::new(code, format!("failed to acquire per-thread lease: {error}"))
                .component("lease")
        })?;
        Ok(Self { file, path })
    }

    #[cfg(test)]
    fn for_test(directory: &Path, thread_id: &str) -> Result<Self> {
        Self::acquire_in(directory, thread_id)
    }
}

impl Drop for ThreadLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn state_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AGENTIC_COMPACT_STATE_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("agentic-compact"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(ErrorCode::Io, "HOME is unset"))?;
    Ok(home.join(".local/state/agentic-compact"))
}

pub fn secure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn secure_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lease_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let locks = temp.path().join("locks");
        let first = ThreadLease::for_test(&locks, "thread-a").unwrap();
        let second = ThreadLease::for_test(&locks, "thread-a").unwrap_err();
        assert_eq!(second.code, ErrorCode::TransitionPending);
        drop(first);
        ThreadLease::for_test(&locks, "thread-a").unwrap();
    }
}
