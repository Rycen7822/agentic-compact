mod plugin_cli;

use crate::app_server::{AppServerClient, codex_home as default_codex_home};
use crate::cli::{InstallArgs, UninstallArgs};
use crate::error::{Error, ErrorCode, Result};
use crate::launcher::resolve_supported_codex_binary;
use crate::observability::sha256_hex;
use plugin_cli::{install_plugin_with_cli, remove_plugin_with_cli};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, Value as TomlValue, value};

const STATE_SCHEMA_VERSION: u32 = 1;
const MANAGED_SERVER: &str = "agentic-compact";
const PLUGIN_JSON: &str = include_str!("../plugins/agentic-compact/.codex-plugin/plugin.json");
const PLUGIN_README: &str = include_str!("../plugins/agentic-compact/README.md");
const SKILL: &str = include_str!("../plugins/agentic-compact/skills/agentic-compact/SKILL.md");
const MARKETPLACE: &str = include_str!("../.agents/plugins/marketplace.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallState {
    schema_version: u32,
    version: String,
    binary_path: PathBuf,
    binary_sha256: String,
    config_path: PathBuf,
    config_section_sha256: String,
    plugin_root: PathBuf,
    plugin_sha256: String,
    wrapper_path: Option<PathBuf>,
    wrapper_sha256: Option<String>,
}

pub async fn install(args: InstallArgs) -> Result<()> {
    let codex_home = args.codex_home.unwrap_or(default_codex_home()?);
    let codex_binary = resolve_supported_codex_binary().await?;
    let state_path = install_state_path(&codex_home);
    let previous = load_state(&state_path)?;
    if previous.is_some() {
        ensure_upgrade_quiescent(&codex_home).await?;
    }
    let home = user_home()?;
    let bin_dir = home.join(".local/bin");
    fs::create_dir_all(&bin_dir)?;
    let binary_path = bin_dir.join(binary_name());
    let source_binary = std::env::current_exe()?;
    let source_bytes = fs::read(&source_binary)?;
    let binary_sha256 = sha256_hex(&source_bytes);
    protect_owned_file_before_replace(
        &binary_path,
        previous.as_ref().map(|state| state.binary_sha256.as_str()),
        &binary_sha256,
        "binary",
    )?;
    atomic_write(&binary_path, &source_bytes, true)?;

    let plugin_root = codex_home.join("agentic-compact/plugin-source");
    let plugin_files = plugin_files();
    let plugin_sha256 = aggregate_files_hash(&plugin_files);
    if plugin_root.exists() {
        let current = hash_plugin_tree(&plugin_root)?;
        let owned = previous.as_ref().is_some_and(|state| {
            state.plugin_root == plugin_root && state.plugin_sha256 == current
        });
        if current != plugin_sha256 && !owned {
            return Err(Error::new(
                ErrorCode::ConfigUserModified,
                format!(
                    "plugin source was modified by the user: {}",
                    plugin_root.display()
                ),
            )
            .component("install"));
        }
    }
    write_plugin_tree(&plugin_root, &plugin_files)?;

    let config_path = codex_home.join("config.toml");
    let config_section_sha256 = install_mcp_section(
        &config_path,
        &binary_path,
        previous
            .as_ref()
            .map(|state| state.config_section_sha256.as_str()),
    )?;

    let (wrapper_path, wrapper_sha256) = if args.no_shell_alias {
        (None, None)
    } else {
        let path = bin_dir.join("codex-agentic");
        let script = format!(
            "#!/bin/sh\nexec \"{}\" codex -- \"$@\"\n",
            binary_path.display()
        );
        let hash = sha256_hex(script.as_bytes());
        protect_owned_file_before_replace(
            &path,
            previous
                .as_ref()
                .and_then(|state| state.wrapper_sha256.as_deref()),
            &hash,
            "wrapper",
        )?;
        atomic_write(&path, script.as_bytes(), true)?;
        (Some(path), Some(hash))
    };

    let state = InstallState {
        schema_version: STATE_SCHEMA_VERSION,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        binary_path,
        binary_sha256,
        config_path,
        config_section_sha256,
        plugin_root,
        plugin_sha256,
        wrapper_path,
        wrapper_sha256,
    };
    save_state(&state_path, &state)?;
    install_plugin_with_cli(&codex_binary, &codex_home, &state.plugin_root).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "installed",
            "binary": state.binary_path,
            "wrapper": state.wrapper_path,
            "pluginSource": state.plugin_root,
            "next": [
                "Start the TUI with codex-agentic, then run agentic-compact doctor --probe with both acknowledgement flags."
            ]
        }))?
    );
    Ok(())
}

pub async fn uninstall(args: UninstallArgs) -> Result<()> {
    let codex_home = args.codex_home.unwrap_or(default_codex_home()?);
    let codex_binary = resolve_supported_codex_binary().await?;
    let state_path = install_state_path(&codex_home);
    let state = load_state(&state_path)?.ok_or_else(|| {
        Error::new(
            ErrorCode::ConfigUserModified,
            "agentic-compact install state is missing",
        )
        .component("install")
    })?;

    validate_uninstall_state(&state)?;
    remove_plugin_with_cli(&codex_binary, &codex_home, &state.plugin_root).await?;
    remove_mcp_section(&state.config_path, &state.config_section_sha256)?;
    remove_owned_file(&state.binary_path, &state.binary_sha256, "binary")?;
    if let (Some(path), Some(hash)) = (&state.wrapper_path, &state.wrapper_sha256) {
        remove_owned_file(path, hash, "wrapper")?;
    }
    if state.plugin_root.exists() {
        let current = hash_plugin_tree(&state.plugin_root)?;
        if current != state.plugin_sha256 {
            return Err(Error::new(
                ErrorCode::ConfigUserModified,
                format!(
                    "plugin source was modified by the user: {}",
                    state.plugin_root.display()
                ),
            )
            .component("install"));
        }
        remove_plugin_files(&state.plugin_root)?;
    }
    if state_path.exists() {
        fs::remove_file(&state_path)?;
    }
    println!(r#"{{"status":"uninstalled"}}"#);
    Ok(())
}

fn install_mcp_section(
    config_path: &Path,
    binary_path: &Path,
    previous_hash: Option<&str>,
) -> Result<String> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let original = if config_path.exists() {
        fs::read_to_string(config_path)?
    } else {
        String::new()
    };
    let mut document = if original.trim().is_empty() {
        DocumentMut::new()
    } else {
        original.parse::<DocumentMut>()?
    };
    let root = document.as_table_mut();
    if !root.contains_key("mcp_servers") {
        root.insert("mcp_servers", Item::Table(Table::new()));
    }
    let servers = root
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::ConfigUserModified,
                "mcp_servers is not a TOML table",
            )
        })?;

    let mut desired = Table::new();
    desired.insert("command", value(binary_path.display().to_string()));
    let mut args = Array::new();
    args.push("mcp");
    desired.insert("args", Item::Value(TomlValue::Array(args)));
    desired.insert("default_tools_approval_mode", value("approve"));
    let desired_item = Item::Table(desired);
    let desired_hash = sha256_hex(desired_item.to_string().as_bytes());

    if let Some(current) = servers.get(MANAGED_SERVER) {
        let current_hash = sha256_hex(current.to_string().as_bytes());
        let owned = previous_hash.is_some_and(|hash| hash == current_hash);
        if current_hash != desired_hash && !owned {
            return Err(Error::new(
                ErrorCode::ConfigUserModified,
                "mcp_servers.agentic-compact exists and is not owned by this installer",
            )
            .component("install"));
        }
    }
    servers.insert(MANAGED_SERVER, desired_item);

    if config_path.exists() && !original.is_empty() {
        let backup = config_path.with_extension("toml.agentic-compact.bak");
        if !backup.exists() {
            atomic_write(&backup, original.as_bytes(), false)?;
        }
    }
    atomic_write(config_path, document.to_string().as_bytes(), false)?;
    Ok(desired_hash)
}

async fn ensure_upgrade_quiescent(codex_home: &Path) -> Result<()> {
    let socket = codex_home
        .join("app-server-control")
        .join("app-server-control.sock");
    if !socket.exists() {
        return Ok(());
    }
    let client = AppServerClient::connect(&socket).await?;
    let loaded = client.loaded_threads().await;
    client.close().await;
    if !loaded?.is_empty() {
        return Err(Error::new(
            ErrorCode::TransitionPending,
            "install or upgrade is deferred while Codex has loaded threads",
        )
        .component("install"));
    }
    Ok(())
}

fn remove_mcp_section(config_path: &Path, expected_hash: &str) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let original = fs::read_to_string(config_path)?;
    let mut document = original.parse::<DocumentMut>()?;
    match managed_mcp_hash(&document) {
        Some(current_hash) if current_hash != expected_hash => {
            return Err(Error::new(
                ErrorCode::ConfigUserModified,
                "mcp_servers.agentic-compact was modified after installation",
            )
            .component("install"));
        }
        None => return Ok(()),
        Some(_) => {}
    }
    let root = document.as_table_mut();
    let Some(servers) = root.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
        return Ok(());
    };
    servers.remove(MANAGED_SERVER);
    if servers.is_empty() {
        root.remove("mcp_servers");
    }
    atomic_write(config_path, document.to_string().as_bytes(), false)
}

fn validate_uninstall_state(state: &InstallState) -> Result<()> {
    validate_mcp_section(&state.config_path, &state.config_section_sha256)?;
    validate_owned_file(&state.binary_path, &state.binary_sha256, "binary")?;
    if let (Some(path), Some(hash)) = (&state.wrapper_path, &state.wrapper_sha256) {
        validate_owned_file(path, hash, "wrapper")?;
    }
    if state.plugin_root.exists() && hash_plugin_tree(&state.plugin_root)? != state.plugin_sha256 {
        return Err(Error::new(
            ErrorCode::ConfigUserModified,
            format!(
                "plugin source was modified by the user: {}",
                state.plugin_root.display()
            ),
        )
        .component("install"));
    }
    Ok(())
}

fn validate_mcp_section(config_path: &Path, expected_hash: &str) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let document = fs::read_to_string(config_path)?.parse::<DocumentMut>()?;
    if managed_mcp_hash(&document).is_some_and(|current| current != expected_hash) {
        Err(Error::new(
            ErrorCode::ConfigUserModified,
            "mcp_servers.agentic-compact was modified after installation",
        )
        .component("install"))
    } else {
        Ok(())
    }
}

fn managed_mcp_hash(document: &DocumentMut) -> Option<String> {
    document
        .as_table()
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(MANAGED_SERVER))
        .map(|section| sha256_hex(section.to_string().as_bytes()))
}

fn plugin_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (".agents/plugins/marketplace.json", MARKETPLACE),
        (
            "plugins/agentic-compact/.codex-plugin/plugin.json",
            PLUGIN_JSON,
        ),
        ("plugins/agentic-compact/README.md", PLUGIN_README),
        (
            "plugins/agentic-compact/skills/agentic-compact/SKILL.md",
            SKILL,
        ),
    ]
}

fn write_plugin_tree(root: &Path, files: &[(&str, &str)]) -> Result<()> {
    for (relative, contents) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&path, contents.as_bytes(), false)?;
    }
    Ok(())
}

fn remove_plugin_files(root: &Path) -> Result<()> {
    let mut directories = Vec::new();
    for (relative, _) in plugin_files() {
        let path = root.join(relative);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory == root.parent().unwrap_or(root) {
                break;
            }
            directories.push(directory.to_path_buf());
            if directory == root {
                break;
            }
            parent = directory.parent();
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    directories.dedup();
    for directory in directories {
        let _ = fs::remove_dir(&directory);
    }
    Ok(())
}

fn aggregate_files_hash(files: &[(&str, &str)]) -> String {
    let mut bytes = Vec::new();
    for (path, contents) in files {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(contents.as_bytes());
        bytes.push(0xff);
    }
    sha256_hex(&bytes)
}

fn hash_plugin_tree(root: &Path) -> Result<String> {
    let files = plugin_files();
    let mut bytes = Vec::new();
    for (relative, _) in files {
        let path = root.join(relative);
        if !path.is_file() {
            return Ok(String::new());
        }
        bytes.extend_from_slice(relative.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&fs::read(path)?);
        bytes.push(0xff);
    }
    Ok(sha256_hex(&bytes))
}

fn protect_owned_file_before_replace(
    path: &Path,
    previous_hash: Option<&str>,
    desired_hash: &str,
    label: &str,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let current = sha256_hex(&fs::read(path)?);
    if current == desired_hash || previous_hash.is_some_and(|hash| hash == current) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::ConfigUserModified,
            format!(
                "managed {label} was modified by the user: {}",
                path.display()
            ),
        )
        .component("install"))
    }
}

fn remove_owned_file(path: &Path, expected_hash: &str, label: &str) -> Result<()> {
    validate_owned_file(path, expected_hash, label)?;
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path)?;
    Ok(())
}

fn validate_owned_file(path: &Path, expected_hash: &str, label: &str) -> Result<()> {
    if path.exists() && sha256_hex(&fs::read(path)?) != expected_hash {
        return Err(Error::new(
            ErrorCode::ConfigUserModified,
            format!(
                "managed {label} was modified by the user: {}",
                path.display()
            ),
        )
        .component("install"));
    }
    Ok(())
}

fn install_state_path(codex_home: &Path) -> PathBuf {
    codex_home.join("agentic-compact/install-state.json")
}

fn load_state(path: &Path) -> Result<Option<InstallState>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.len() > 128 * 1024 {
        return Err(Error::new(
            ErrorCode::Protocol,
            "install state exceeds 128 KiB",
        ));
    }
    let state: InstallState = serde_json::from_slice(&bytes)?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(Error::new(
            ErrorCode::UnsupportedCodex,
            "unsupported install-state schema",
        ));
    }
    Ok(Some(state))
}

fn save_state(path: &Path, state: &InstallState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    atomic_write(path, &bytes, false)
}

fn atomic_write(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)?;
    set_file_mode(&temporary, executable)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(ErrorCode::Io, "HOME is unset").component("install"))
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "agentic-compact.exe"
    } else {
        "agentic-compact"
    }
}

#[cfg(unix)]
fn set_file_mode(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _executable: bool) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_hash_is_stable() {
        assert_eq!(
            aggregate_files_hash(&plugin_files()),
            aggregate_files_hash(&plugin_files())
        );
    }

    #[test]
    fn emitted_plugin_source_is_a_complete_marketplace_root() {
        let files = plugin_files();
        assert!(
            files
                .iter()
                .any(|(path, _)| *path == ".agents/plugins/marketplace.json")
        );
        assert!(
            files
                .iter()
                .any(|(path, _)| *path == "plugins/agentic-compact/.codex-plugin/plugin.json")
        );
        assert!(!files.iter().any(|(path, _)| *path == "marketplace.json"));
    }
}
