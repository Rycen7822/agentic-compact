#![allow(dead_code)]

use serde_json::json;
use std::ffi::OsString;
use std::path::Path;

pub(crate) struct EnvironmentGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvironmentGuard {
    pub(crate) fn set(key: &'static str, value: &Path) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(original) = &self.original {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

pub(crate) fn write_ready_capability(codex_home: &Path) {
    let directory = codex_home.join("agentic-compact");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("capabilities.json"),
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "pluginVersion": env!("CARGO_PKG_VERSION"),
            "codexUserAgent": "codex-test/0.1",
            "platformFamily": "unix",
            "platformOs": "linux",
            "emptyContinuation": true,
            "reentrantAttachAcknowledged": true,
            "hiddenCheckpointAcknowledged": true,
            "checkedAtMs": 1
        }))
        .unwrap(),
    )
    .unwrap();
}
