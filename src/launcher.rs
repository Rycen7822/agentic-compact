use crate::app_server::{AppServerClient, default_socket_path};
use crate::app_server_owner::{self, AppServerOwner};
use crate::error::{Error, ErrorCode, Result};
use serde::Deserialize;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

const CURRENT_POLICY: &str =
    include_str!("../tests/fixtures/launcher/codex-cli-0.146.0/launcher-args-v1.json");
const PREVIOUS_POLICY: &str =
    include_str!("../tests/fixtures/launcher/codex-cli-0.145.0/launcher-args-v1.json");
const CURRENT_CODEX_VERSION: &str = "codex-cli 0.146.0";
const PREVIOUS_CODEX_VERSION: &str = "codex-cli 0.145.0";
const SOCKET_WAIT_ATTEMPTS: usize = 100;

#[derive(Debug, Deserialize)]
struct LauncherPolicy {
    codex_version: String,
    #[serde(default)]
    allow_single_prompt: bool,
    #[serde(default)]
    pass_through_exact: Vec<String>,
    #[serde(default)]
    rejected_exact: Vec<String>,
    #[serde(default)]
    rejected_prefixes: Vec<String>,
    #[serde(default)]
    subcommands: Vec<String>,
}

struct OwnedServer {
    child: Child,
    owner: AppServerOwner,
}

pub async fn run(args: Vec<String>) -> Result<()> {
    if env::var_os("AGENTIC_COMPACT_LAUNCH_ACTIVE").is_some() {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            "recursive agentic-compact launcher invocation detected",
        )
        .component("launcher"));
    }
    let codex = resolve_codex_binary()?;
    let codex_version = read_codex_version(&codex).await?;
    let policy = launcher_policy_for_version(&codex_version)?;
    validate_tui_args_with(&args, &policy)?;
    let socket = default_socket_path()?;
    let mut owned_server = ensure_shared_server(&codex, &codex_version, &socket).await?;

    let mut command = Command::new(&codex);
    command
        .args(&args)
        .env("AGENTIC_COMPACT_LAUNCH_ACTIVE", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command.status().await;
    let cleanup_result = if let Some(server) = owned_server.as_mut() {
        cleanup_owned_server(server).await
    } else {
        Ok(())
    };
    let status = status.map_err(|error| {
        Error::new(
            ErrorCode::Io,
            format!("failed to launch {}: {error}", codex.display()),
        )
        .component("launcher")
    })?;
    cleanup_result?;

    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(Error::new(
            ErrorCode::Internal,
            format!("Codex TUI exited with status {code}"),
        )
        .component("launcher")),
        None => Err(
            Error::new(ErrorCode::Internal, "Codex TUI terminated by signal").component("launcher"),
        ),
    }
}

#[cfg(test)]
fn validate_tui_args(args: &[String]) -> Result<()> {
    let policy = launcher_policy_for_version(CURRENT_CODEX_VERSION)?;
    validate_tui_args_with(args, &policy)
}

fn launcher_policy_for_version(version: &str) -> Result<LauncherPolicy> {
    let source = policy_source(version).ok_or_else(|| {
        Error::new(
            ErrorCode::UnsupportedCodex,
            format!("no frozen launcher contract exists for {version}"),
        )
        .component("launcher")
    })?;
    let policy: LauncherPolicy = serde_json::from_str(source)?;
    if policy.codex_version != version {
        return Err(Error::new(
            ErrorCode::Internal,
            "selected launcher fixture declares a different Codex version",
        )
        .component("launcher"));
    }
    Ok(policy)
}

fn policy_source(version: &str) -> Option<&'static str> {
    match version {
        CURRENT_CODEX_VERSION => Some(CURRENT_POLICY),
        PREVIOUS_CODEX_VERSION => Some(PREVIOUS_POLICY),
        _ => None,
    }
}

fn validate_tui_args_with(args: &[String], policy: &LauncherPolicy) -> Result<()> {
    let mut positional_count = 0;
    for argument in args {
        if !argument.starts_with('-') {
            if positional_count == 0 && policy.subcommands.iter().any(|item| item == argument) {
                return Err(Error::new(
                    ErrorCode::InvalidRequest,
                    "agentic-compact codex launches the interactive TUI only; Codex subcommands must bypass the wrapper",
                )
                .component("launcher"));
            }
            positional_count += 1;
            if !policy.allow_single_prompt || positional_count > 1 {
                return Err(Error::new(
                    ErrorCode::InvalidRequest,
                    "the frozen launcher contract permits at most one initial TUI prompt",
                )
                .component("launcher"));
            }
            continue;
        }

        let exact = policy.rejected_exact.iter().any(|value| value == argument);
        let prefixed = policy
            .rejected_prefixes
            .iter()
            .any(|prefix| argument.starts_with(prefix));
        if exact || prefixed {
            return Err(Error::new(
                ErrorCode::InvalidRequest,
                format!(
                    "Codex argument {argument:?} can force an embedded or divergent app-server configuration; move the setting into config.toml"
                ),
            )
            .component("launcher"));
        }
        if !policy
            .pass_through_exact
            .iter()
            .any(|value| value == argument)
        {
            return Err(Error::new(
                ErrorCode::InvalidRequest,
                format!(
                    "Codex argument {argument:?} is not classified by the frozen launcher contract"
                ),
            )
            .component("launcher"));
        }
    }
    Ok(())
}

async fn ensure_shared_server(
    codex: &Path,
    codex_version: &str,
    socket: &Path,
) -> Result<Option<OwnedServer>> {
    let owner_path = app_server_owner::owner_path()?;
    let existing_owner = app_server_owner::load(&owner_path)?;
    if socket.exists() {
        let client = AppServerClient::connect(socket).await.map_err(|error| {
            Error::new(
                ErrorCode::SharedAppServerUnavailable,
                format!("the default app-server socket exists but is unusable: {error}"),
            )
            .component("launcher")
        })?;
        let loaded = client.loaded_threads().await?;
        client.close().await;
        let Some(owner) = existing_owner else {
            return Err(unconfirmed_owner_error(!loaded.is_empty()));
        };
        if !owner.matches(codex, codex_version, socket)
            || !app_server_owner::process_is_alive(owner.pid)?
        {
            return Err(unconfirmed_owner_error(!loaded.is_empty()));
        }
        return Ok(None);
    }
    if let Some(owner) = existing_owner {
        if app_server_owner::process_is_alive(owner.pid)? {
            return Err(Error::new(
                ErrorCode::SharedAppServerUnavailable,
                "the recorded app-server process is alive but its socket is absent; refusing to start a competing server",
            )
            .component("launcher"));
        }
        app_server_owner::remove_if_pid(&owner_path, owner.pid)?;
    }
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut child = Command::new(codex)
        .args(["app-server", "--listen", "unix://"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| {
            Error::new(
                ErrorCode::Io,
                format!("failed to start Codex app-server: {error}"),
            )
            .component("launcher")
        })?;

    for _ in 0..SOCKET_WAIT_ATTEMPTS {
        if child.try_wait()?.is_some() {
            return Err(Error::new(
                ErrorCode::SharedAppServerUnavailable,
                "Codex app-server exited before its default socket became ready",
            )
            .component("launcher"));
        }
        if socket.exists() && AppServerClient::connect(socket).await.is_ok() {
            let pid = child.id().ok_or_else(|| {
                Error::new(
                    ErrorCode::SharedAppServerUnavailable,
                    "spawned app-server has no process id",
                )
                .component("launcher")
            })?;
            let owner = AppServerOwner::new(pid, codex, codex_version.to_owned(), socket)?;
            if let Err(error) = app_server_owner::save(&owner_path, &owner) {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(error);
            }
            return Ok(Some(OwnedServer { child, owner }));
        }
        sleep(Duration::from_millis(100)).await;
    }
    let _ = child.start_kill();
    Err(Error::new(
        ErrorCode::SharedAppServerUnavailable,
        "Codex app-server did not become ready within 10 seconds",
    )
    .component("launcher"))
}

async fn cleanup_owned_server(server: &mut OwnedServer) -> Result<()> {
    let stopped = if server.child.try_wait()?.is_some() {
        true
    } else if server_can_stop().await {
        let _ = server.child.start_kill();
        let _ = server.child.wait().await;
        true
    } else {
        false
    };
    if stopped {
        app_server_owner::remove_if_pid(&app_server_owner::owner_path()?, server.owner.pid)?;
    }
    Ok(())
}

async fn server_can_stop() -> bool {
    let Ok(client) = AppServerClient::connect_default().await else {
        return false;
    };
    let empty = client
        .loaded_threads()
        .await
        .is_ok_and(|threads| threads.is_empty());
    client.close().await;
    empty
}

async fn read_codex_version(codex: &Path) -> Result<String> {
    let mut command = Command::new(codex);
    command
        .arg("--version")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| Error::timeout("launcher", "Codex version probe timed out"))??;
    if !output.status.success() || output.stdout.len() > 256 {
        return Err(Error::new(
            ErrorCode::UnsupportedCodex,
            "resolved Codex binary did not return a bounded successful version",
        )
        .component("launcher"));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| {
            Error::new(
                ErrorCode::UnsupportedCodex,
                "resolved Codex version is not UTF-8",
            )
            .component("launcher")
        })
}

fn unconfirmed_owner_error(has_loaded_threads: bool) -> Error {
    let detail = if has_loaded_threads {
        " and it has loaded threads"
    } else {
        ""
    };
    Error::new(
        ErrorCode::SharedAppServerUnavailable,
        format!(
            "the existing app-server ownership cannot be confirmed{detail}; preserving it and refusing a new seamless session"
        ),
    )
    .component("launcher")
}

fn resolve_codex_binary() -> Result<PathBuf> {
    if let Some(explicit) = env::var_os("AGENTIC_COMPACT_CODEX_BIN") {
        return validate_codex_path(PathBuf::from(explicit));
    }
    let path = env::var_os("PATH").ok_or_else(|| {
        Error::new(
            ErrorCode::Io,
            "PATH is unset; set AGENTIC_COMPACT_CODEX_BIN",
        )
        .component("launcher")
    })?;
    let current = env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    for directory in env::split_paths(&path) {
        for name in codex_names() {
            let candidate = directory.join(name);
            if !candidate.is_file() {
                continue;
            }
            if current.as_ref().is_some_and(|current| {
                candidate
                    .canonicalize()
                    .is_ok_and(|candidate| &candidate == current)
            }) {
                continue;
            }
            return validate_codex_path(candidate);
        }
    }
    Err(Error::new(
        ErrorCode::Io,
        "stock Codex binary was not found; set AGENTIC_COMPACT_CODEX_BIN",
    )
    .component("launcher"))
}

pub(crate) async fn resolve_supported_codex_binary() -> Result<PathBuf> {
    let codex = resolve_codex_binary()?;
    let version = read_codex_version(&codex).await?;
    if policy_source(&version).is_none() {
        return Err(Error::new(
            ErrorCode::UnsupportedCodex,
            format!(
                "plugin CLI contract supports {CURRENT_CODEX_VERSION} and {PREVIOUS_CODEX_VERSION}, but the resolved binary reports {version}"
            ),
        )
        .component("install"));
    }
    Ok(codex)
}

fn validate_codex_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_file() {
        Ok(path)
    } else {
        Err(Error::new(
            ErrorCode::Io,
            format!("Codex binary does not exist: {}", path.display()),
        )
        .component("launcher"))
    }
}

fn codex_names() -> Vec<OsString> {
    if cfg!(windows) {
        vec![OsString::from("codex.exe"), OsString::from("codex")]
    } else {
        vec![OsString::from("codex")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_remote_and_config_overrides() {
        assert!(validate_tui_args(&["--remote".into(), "unix:///tmp/x".into()]).is_err());
        assert!(validate_tui_args(&["-c".into(), "model=x".into()]).is_err());
    }

    #[test]
    fn rejects_unverified_and_unknown_options() {
        assert!(validate_tui_args(&["--model".into(), "gpt-test".into()]).is_err());
        assert!(validate_tui_args(&["--future-flag".into()]).is_err());
    }

    #[test]
    fn allows_at_most_one_initial_prompt() {
        assert!(validate_tui_args(&[]).is_ok());
        assert!(validate_tui_args(&["continue the task".into()]).is_ok());
        assert!(validate_tui_args(&["first".into(), "second".into()]).is_err());
        assert!(validate_tui_args(&["resume".into()]).is_err());
    }

    #[test]
    fn supports_current_and_previous_stable_contracts() {
        for version in [CURRENT_CODEX_VERSION, PREVIOUS_CODEX_VERSION] {
            let policy = launcher_policy_for_version(version).unwrap();
            assert_eq!(policy.codex_version, version);
        }
        assert!(launcher_policy_for_version("codex-cli 0.144.0").is_err());
    }
}
