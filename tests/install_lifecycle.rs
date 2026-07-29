#![cfg(unix)]

use serde_json::json;
use std::path::Path;
use std::process::{Output, Stdio};
use tokio::process::Command;

mod support;

use support::FakeServer;

#[tokio::test]
async fn official_plugin_lifecycle_and_loaded_thread_upgrade_guard() {
    let mut server = FakeServer::start().await;
    let home = server.codex_home().parent().unwrap().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config_path = server.codex_home().join("config.toml");
    let initial_config = "[unrelated]\nvalue = 1\n";
    std::fs::write(&config_path, initial_config).unwrap();

    let installed = run(&home, server.codex_home(), "install").await;
    assert!(installed.status.success(), "{}", stderr(&installed));
    let installed_json: serde_json::Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(installed_json["status"], "installed");
    assert!(home.join(".local/bin/agentic-compact").is_file());
    assert!(
        server
            .codex_home()
            .join("agentic-compact/plugin-source/.agents/plugins/marketplace.json")
            .is_file()
    );
    assert_eq!(
        std::fs::read_to_string(server.codex_home().join("config.toml.agentic-compact.bak"))
            .unwrap(),
        initial_config
    );

    let upgrade_home = home.clone();
    let upgrade_codex_home = server.codex_home().to_owned();
    let upgrading =
        tokio::spawn(async move { run(&upgrade_home, &upgrade_codex_home, "install").await });
    server.initialize_connection().await;
    let loaded = server.next_request().await;
    assert_eq!(loaded["method"], "thread/loaded/list");
    server
        .send(json!({
            "id": loaded["id"],
            "result": {"data": ["active-thread"], "nextCursor": null}
        }))
        .await;
    let rejected = upgrading.await.unwrap();
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("transition_pending"));
    assert!(home.join(".local/bin/agentic-compact").is_file());

    let original_config = std::fs::read_to_string(&config_path).unwrap();
    let modified_config = original_config.replace(
        &home
            .join(".local/bin/agentic-compact")
            .display()
            .to_string(),
        "/user/modified/agentic-compact",
    );
    assert_ne!(modified_config, original_config);
    std::fs::write(&config_path, modified_config).unwrap();
    let protected = run(&home, server.codex_home(), "uninstall").await;
    assert!(!protected.status.success());
    assert!(stderr(&protected).contains("config_user_modified"));
    assert!(home.join(".local/bin/agentic-compact").is_file());
    assert!(
        server
            .codex_home()
            .join("agentic-compact/plugin-source")
            .is_dir()
    );
    std::fs::write(&config_path, original_config).unwrap();

    let uninstalled = run(&home, server.codex_home(), "uninstall").await;
    assert!(uninstalled.status.success(), "{}", stderr(&uninstalled));
    assert!(!home.join(".local/bin/agentic-compact").exists());
    assert!(!home.join(".local/bin/codex-agentic").exists());
    assert!(
        !server
            .codex_home()
            .join("agentic-compact/plugin-source")
            .exists()
    );
    let config = std::fs::read_to_string(config_path).unwrap();
    assert!(!config.contains("agentic-compact"));
    assert!(config.contains("[unrelated]"));
}

async fn run(home: &Path, codex_home: &Path, action: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentic-compact"))
        .arg(action)
        .arg("--codex-home")
        .arg(codex_home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
