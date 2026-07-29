#![cfg(target_os = "linux")]

use agentic_compact::checkpoint::CompactionIntent;
use agentic_compact::journal::{JournalStore, TransitionJournal};
use serde_json::json;
use std::time::{Duration, Instant};

mod support;

use support::{EnvironmentGuard, FakeServer};

const KIB: u64 = 1024;

#[tokio::test]
#[ignore = "phase 6 Linux journal and memory SLO gate"]
async fn journal_and_snapshot_memory_stay_within_slo() {
    let idle_rss = proc_status_kib("VmRSS");
    eprintln!("idle RSS: {idle_rss} KiB");
    assert!(idle_rss < 64 * KIB, "idle RSS exceeded 64 MiB");

    let state = tempfile::tempdir().unwrap();
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", state.path());
    let store = JournalStore::open().unwrap();
    let journal = TransitionJournal::new(
        "phase6-journal".to_owned(),
        "source".to_owned(),
        "receipt".to_owned(),
        "checkpoint".to_owned(),
        CompactionIntent {
            preserve: vec!["measure atomic persistence".to_owned()],
            next_action: "finish the resource gate".to_owned(),
        },
    )
    .unwrap();
    let mut writes = Vec::with_capacity(200);
    for _ in 0..200 {
        let started = Instant::now();
        store.save(&journal).unwrap();
        writes.push(started.elapsed());
    }
    let journal_p99 = percentile(&writes, 99);
    eprintln!(
        "journal write: p99={journal_p99:?}, samples={}",
        writes.len()
    );
    assert!(
        journal_p99 < Duration::from_millis(25),
        "journal write p99 exceeded 25 ms"
    );

    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let reading_client = client.clone();
    let reading =
        tokio::spawn(async move { reading_client.thread_read("large-snapshot", true).await });
    let request = server.next_request().await;
    assert_eq!(request["method"], "thread/read");
    server
        .send(json!({
            "id": request["id"],
            "result": {
                "id": "large-snapshot",
                "status": {"type": "idle"},
                "turns": [],
                "ignoredPadding": "x".repeat(31 * 1024 * 1024)
            }
        }))
        .await;
    assert_eq!(reading.await.unwrap().unwrap().id, "large-snapshot");
    client.close().await;

    let peak_rss = proc_status_kib("VmHWM");
    eprintln!("32 MiB snapshot-path peak RSS: {peak_rss} KiB");
    assert!(peak_rss < 256 * KIB, "snapshot peak RSS exceeded 256 MiB");
}

fn percentile(samples: &[Duration], percentile: usize) -> Duration {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let index = (samples.len() * percentile).div_ceil(100).saturating_sub(1);
    samples[index]
}

fn proc_status_kib(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix(field)?
                .trim_start_matches(':')
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or_else(|| panic!("{field} is absent from /proc/self/status"))
}
