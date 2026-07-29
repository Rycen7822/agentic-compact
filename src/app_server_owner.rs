use crate::app_server::codex_home;
use crate::error::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(any(windows, all(unix, not(target_os = "linux"))))]
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const OWNER_FILE: &str = "agentic-compact/app-server-owner.json";
const MAX_OWNER_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AppServerOwner {
    pub(crate) pid: u32,
    pub(crate) codex_binary: PathBuf,
    pub(crate) codex_version: String,
    pub(crate) started_at: u128,
    pub(crate) socket: PathBuf,
}

impl AppServerOwner {
    pub(crate) fn new(
        pid: u32,
        codex_binary: &Path,
        codex_version: String,
        socket: &Path,
    ) -> Result<Self> {
        Ok(Self {
            pid,
            codex_binary: fs::canonicalize(codex_binary)?,
            codex_version,
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            socket: socket.to_path_buf(),
        })
    }

    pub(crate) fn matches(&self, codex_binary: &Path, codex_version: &str, socket: &Path) -> bool {
        self.pid > 0
            && fs::canonicalize(codex_binary).is_ok_and(|path| path == self.codex_binary)
            && self.codex_version == codex_version
            && self.socket == socket
    }
}

pub(crate) fn owner_path() -> Result<PathBuf> {
    Ok(codex_home()?.join(OWNER_FILE))
}

pub(crate) fn load(path: &Path) -> Result<Option<AppServerOwner>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_OWNER_BYTES {
        return Err(Error::new(
            ErrorCode::SharedAppServerUnavailable,
            "app-server owner record exceeds 64 KiB",
        )
        .component("launcher"));
    }
    let owner = serde_json::from_slice(&fs::read(path)?).map_err(|_| {
        Error::new(
            ErrorCode::SharedAppServerUnavailable,
            "app-server owner record is invalid",
        )
        .component("launcher")
    })?;
    Ok(Some(owner))
}

pub(crate) fn save(path: &Path, owner: &AppServerOwner) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(owner)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::Io, "owner path has no parent"))?;
    fs::create_dir_all(parent)?;
    secure_dir(parent)?;
    let temporary = parent.join(format!(
        ".app-server-owner.{}.{}.tmp",
        std::process::id(),
        owner.started_at
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        secure_file(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn remove_if_pid(path: &Path, pid: u32) -> Result<()> {
    if load(path)?.is_some_and(|owner| owner.pid == pid) {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn process_is_alive(pid: u32) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        Ok(Path::new("/proc").join(pid.to_string()).exists())
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        Ok(Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()?
            .success())
    }
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()?;
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
    }
}

#[cfg(unix)]
fn secure_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
