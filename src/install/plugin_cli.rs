use crate::error::{Error, ErrorCode, Result};
use serde_json::Value;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

const MARKETPLACE_NAME: &str = "agentic-compact";
const PLUGIN_SELECTOR: &str = "agentic-compact@agentic-compact";
const MANAGED_SERVER: &str = "agentic-compact";
const CODEX_CLI_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CODEX_CLI_OUTPUT: usize = 1024 * 1024;

pub(super) async fn validate_mcp_config_before_write(
    codex: &Path,
    binary_path: &Path,
) -> Result<()> {
    let validation_home =
        std::env::temp_dir().join(format!("agentic-compact-config-{}", Uuid::new_v4()));
    std::fs::create_dir(&validation_home)?;
    let result = run_json(
        codex,
        &validation_home,
        mcp_get_args(Some(binary_path))?,
        "MCP config validation",
    )
    .await;
    std::fs::remove_dir_all(&validation_home)?;
    validate_mcp_server(&result?, binary_path)
}

pub(super) async fn validate_mcp_config_after_write(
    codex: &Path,
    codex_home: &Path,
    binary_path: &Path,
) -> Result<()> {
    let result = run_json(
        codex,
        codex_home,
        mcp_get_args(None)?,
        "MCP effective config validation",
    )
    .await?;
    validate_mcp_server(&result, binary_path)
}

pub(super) async fn install_plugin_with_cli(
    codex: &Path,
    codex_home: &Path,
    marketplace_root: &Path,
) -> Result<()> {
    let added = run_json(
        codex,
        codex_home,
        [
            "plugin".into(),
            "marketplace".into(),
            "add".into(),
            marketplace_root.as_os_str().to_owned(),
            "--json".into(),
        ],
        "marketplace add",
    )
    .await?;
    require_string(&added, "marketplaceName", MARKETPLACE_NAME)?;
    let installed_root = require_path(&added, "installedRoot")?;
    require_same_path(&installed_root, marketplace_root, "marketplace root")?;

    let listed = run_json(
        codex,
        codex_home,
        ["plugin".into(), "list".into(), "--json".into()],
        "plugin list",
    )
    .await?;
    let installed = listed
        .get("installed")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_output("plugin list omitted installed"))?
        .iter()
        .find(|plugin| plugin.get("pluginId").and_then(Value::as_str) == Some(PLUGIN_SELECTOR));
    if let Some(plugin) = installed {
        let source = plugin
            .pointer("/source/path")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| invalid_output("installed plugin omitted source.path"))?;
        require_same_path(
            &source,
            &marketplace_root.join("plugins/agentic-compact"),
            "installed plugin source",
        )?;
        if plugin.get("version").and_then(Value::as_str) == Some(env!("CARGO_PKG_VERSION")) {
            return Ok(());
        }
        remove_plugin(codex, codex_home).await?;
    }

    let installed = run_json(
        codex,
        codex_home,
        [
            "plugin".into(),
            "add".into(),
            PLUGIN_SELECTOR.into(),
            "--json".into(),
        ],
        "plugin add",
    )
    .await?;
    require_string(&installed, "pluginId", PLUGIN_SELECTOR)?;
    require_string(&installed, "version", env!("CARGO_PKG_VERSION"))
}

pub(super) async fn remove_plugin_with_cli(
    codex: &Path,
    codex_home: &Path,
    marketplace_root: &Path,
) -> Result<()> {
    let listed = run_json(
        codex,
        codex_home,
        ["plugin".into(), "list".into(), "--json".into()],
        "plugin list",
    )
    .await?;
    let installed = listed
        .get("installed")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_output("plugin list omitted installed"))?
        .iter()
        .find(|plugin| plugin.get("pluginId").and_then(Value::as_str) == Some(PLUGIN_SELECTOR));
    if let Some(plugin) = installed {
        let source = plugin
            .pointer("/source/path")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| invalid_output("installed plugin omitted source.path"))?;
        require_same_path(
            &source,
            &marketplace_root.join("plugins/agentic-compact"),
            "installed plugin source",
        )?;
        remove_plugin(codex, codex_home).await?;
    }

    let marketplaces = run_json(
        codex,
        codex_home,
        [
            "plugin".into(),
            "marketplace".into(),
            "list".into(),
            "--json".into(),
        ],
        "marketplace list",
    )
    .await?;
    let marketplace = marketplaces
        .get("marketplaces")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_output("marketplace list omitted marketplaces"))?
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(MARKETPLACE_NAME));
    if let Some(marketplace) = marketplace {
        let root = require_path(marketplace, "root")?;
        require_same_path(&root, marketplace_root, "marketplace root")?;
        let removed = run_json(
            codex,
            codex_home,
            [
                "plugin".into(),
                "marketplace".into(),
                "remove".into(),
                MARKETPLACE_NAME.into(),
                "--json".into(),
            ],
            "marketplace remove",
        )
        .await?;
        require_string(&removed, "marketplaceName", MARKETPLACE_NAME)?;
    }
    Ok(())
}

async fn remove_plugin(codex: &Path, codex_home: &Path) -> Result<()> {
    let removed = run_json(
        codex,
        codex_home,
        [
            "plugin".into(),
            "remove".into(),
            PLUGIN_SELECTOR.into(),
            "--json".into(),
        ],
        "plugin remove",
    )
    .await?;
    require_string(&removed, "pluginId", PLUGIN_SELECTOR)
}

fn mcp_get_args(binary_path: Option<&Path>) -> Result<Vec<OsString>> {
    let mut args = Vec::new();
    if let Some(binary_path) = binary_path {
        let command = serde_json::to_string(&binary_path.display().to_string())?;
        for value in [
            format!("mcp_servers.{MANAGED_SERVER}.command={command}"),
            format!("mcp_servers.{MANAGED_SERVER}.args=[\"mcp\"]"),
            format!("mcp_servers.{MANAGED_SERVER}.env_vars=[\"CODEX_HOME\"]"),
            format!("mcp_servers.{MANAGED_SERVER}.default_tools_approval_mode=\"approve\""),
        ] {
            args.extend([OsString::from("-c"), OsString::from(value)]);
        }
    }
    args.extend([
        OsString::from("mcp"),
        OsString::from("get"),
        OsString::from(MANAGED_SERVER),
        OsString::from("--json"),
    ]);
    Ok(args)
}

fn validate_mcp_server(value: &Value, binary_path: &Path) -> Result<()> {
    require_string(value, "name", MANAGED_SERVER)?;
    if value.get("enabled").and_then(Value::as_bool) != Some(true)
        || !value
            .get("disabled_reason")
            .is_some_and(serde_json::Value::is_null)
    {
        return Err(invalid_output("managed MCP server is not enabled"));
    }
    let transport = value
        .get("transport")
        .ok_or_else(|| invalid_output("Codex MCP JSON omitted transport"))?;
    require_string(transport, "type", "stdio")?;
    let command = require_path(transport, "command")?;
    require_same_path(&command, binary_path, "MCP command")?;
    if transport.get("args") != Some(&serde_json::json!(["mcp"]))
        || transport.get("env_vars") != Some(&serde_json::json!(["CODEX_HOME"]))
        || !transport.get("env").is_some_and(serde_json::Value::is_null)
        || !transport.get("cwd").is_some_and(serde_json::Value::is_null)
    {
        return Err(invalid_output(
            "managed MCP stdio arguments or environment did not match",
        ));
    }
    Ok(())
}

async fn run_json(
    codex: &Path,
    codex_home: &Path,
    args: impl IntoIterator<Item = OsString>,
    operation: &'static str,
) -> Result<Value> {
    let mut command = Command::new(codex);
    command
        .args(args)
        .env("CODEX_HOME", codex_home)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(CODEX_CLI_TIMEOUT, command.output())
        .await
        .map_err(|_| Error::timeout("install", format!("Codex {operation} timed out")))??;
    if !output.status.success() {
        return Err(
            Error::new(ErrorCode::Protocol, format!("Codex {operation} failed"))
                .component("install"),
        );
    }
    if output.stdout.len() > MAX_CODEX_CLI_OUTPUT {
        return Err(invalid_output("Codex JSON exceeded 1 MiB"));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| invalid_output("Codex command returned invalid JSON"))
}

fn require_string(value: &Value, field: &str, expected: &str) -> Result<()> {
    if value.get(field).and_then(Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(invalid_output(format!(
            "Codex plugin JSON field {field} did not match"
        )))
    }
}

fn require_path(value: &Value, field: &str) -> Result<std::path::PathBuf> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| invalid_output(format!("Codex plugin JSON omitted {field}")))
}

fn require_same_path(actual: &Path, expected: &Path, label: &str) -> Result<()> {
    let actual = std::fs::canonicalize(actual)?;
    let expected = std::fs::canonicalize(expected)?;
    if actual == expected {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::ConfigUserModified,
            format!("{label} is owned by a different installation"),
        )
        .component("install"))
    }
}

fn invalid_output(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Protocol, message.into()).component("install")
}
