#![cfg(unix)]

use agentic_compact::checkpoint::CompactionIntent;
use agentic_compact::lease::ThreadLease;
use agentic_compact::metadata::BoundInvocation;
use agentic_compact::orchestrator::Orchestrator;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};

mod support;

use support::{
    EnvironmentGuard, Integrity, TransitionSample, TransitionServer, write_ready_capability,
};

const SOAK_TRANSITIONS: usize = 100;
const STRESS_THREADS: usize = 8;
const STRESS_TRANSITIONS: usize = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "phase 6 production-cooldown soak, stress and latency gate"]
async fn production_cooldown_soak_and_parallel_stress() {
    let server = TransitionServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());
    let orchestrator = Arc::new(Orchestrator::new().unwrap());
    let mut warm_attach =
        Vec::with_capacity(SOAK_TRANSITIONS + STRESS_THREADS * STRESS_TRANSITIONS);
    let mut samples = Vec::with_capacity(warm_attach.capacity());

    for sequence in 0..SOAK_TRANSITIONS {
        let (warm, sample) = run_one(&server, Arc::clone(&orchestrator), "soak", sequence).await;
        warm_attach.push(warm);
        samples.push(sample);
        server.wait_for_no_connections().await;
    }

    for sequence in 0..STRESS_TRANSITIONS {
        let mut scheduling = JoinSet::new();
        for thread in 0..STRESS_THREADS {
            let thread_id = format!("stress-{thread}");
            let source_id = server.prepare_transition(&thread_id, sequence);
            let orchestrator = Arc::clone(&orchestrator);
            scheduling.spawn(async move {
                let started = Instant::now();
                let result = orchestrator
                    .schedule(invocation(&thread_id, source_id), compaction_intent())
                    .await
                    .unwrap();
                (thread_id, started.elapsed(), result)
            });
        }
        let mut scheduled = Vec::with_capacity(STRESS_THREADS);
        while let Some(result) = scheduling.join_next().await {
            scheduled.push(result.unwrap());
        }
        for (thread_id, warm, result) in &scheduled {
            warm_attach.push(*warm);
            server.complete_source(
                thread_id,
                sequence,
                &result.receipt_id,
                &compaction_intent(),
            );
        }
        for (thread_id, _, _) in &scheduled {
            samples.push(server.wait_for_transition(thread_id, sequence).await);
        }
        server.wait_for_no_connections().await;
    }

    assert_eq!(
        server.integrity(),
        Integrity {
            transitions: SOAK_TRANSITIONS + STRESS_THREADS * STRESS_TRANSITIONS,
            ..Integrity::default()
        }
    );
    assert_slo(
        "warm attach + scheduled",
        &warm_attach,
        Duration::from_millis(1_500),
    );
    assert_slo(
        "source completed -> compact request",
        &samples
            .iter()
            .map(|sample| sample.source_to_compact)
            .collect::<Vec<_>>(),
        Duration::from_millis(150),
    );
    assert_slo(
        "compact completed -> checkpoint injection",
        &samples
            .iter()
            .map(|sample| sample.compact_to_injection)
            .collect::<Vec<_>>(),
        Duration::from_millis(150),
    );
    assert_slo(
        "injection -> continuation request",
        &samples
            .iter()
            .map(|sample| sample.injection_to_continuation)
            .collect::<Vec<_>>(),
        Duration::from_millis(150),
    );
    assert_lease_released("soak").await;
    for thread in 0..STRESS_THREADS {
        assert_lease_released(&format!("stress-{thread}")).await;
    }
    server.shutdown().await;
}

async fn run_one(
    server: &TransitionServer,
    orchestrator: Arc<Orchestrator>,
    thread_id: &str,
    sequence: usize,
) -> (Duration, TransitionSample) {
    let source_id = server.prepare_transition(thread_id, sequence);
    let started = Instant::now();
    let scheduled = orchestrator
        .schedule(invocation(thread_id, source_id), compaction_intent())
        .await
        .unwrap();
    let warm = started.elapsed();
    server.complete_source(
        thread_id,
        sequence,
        &scheduled.receipt_id,
        &compaction_intent(),
    );
    let sample = server.wait_for_transition(thread_id, sequence).await;
    (warm, sample)
}

fn invocation(thread_id: &str, turn_id: String) -> BoundInvocation {
    BoundInvocation {
        thread_id: thread_id.to_owned(),
        turn_id,
        model: None,
        reasoning_effort: None,
    }
}

fn compaction_intent() -> CompactionIntent {
    CompactionIntent {
        preserve: vec!["preserve the transition invariant".to_owned()],
        next_action: "continue the bounded soak".to_owned(),
    }
}

fn assert_slo(label: &str, samples: &[Duration], limit: Duration) {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
    let p95 = samples[index];
    eprintln!("{label}: p95={p95:?}, samples={}", samples.len());
    assert!(p95 < limit, "{label} p95 {p95:?} exceeded {limit:?}");
}

async fn assert_lease_released(thread_id: &str) {
    timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(lease) = ThreadLease::acquire(thread_id) {
                drop(lease);
                return;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{thread_id} lease remained held"));
}
