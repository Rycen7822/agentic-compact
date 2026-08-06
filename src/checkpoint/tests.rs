use super::*;
use proptest::prelude::*;

fn intent(next_action: impl Into<String>) -> CompactionIntent {
    CompactionIntent {
        preserve: Vec::new(),
        next_action: next_action.into(),
    }
}

fn checkpoint(intent: CompactionIntent) -> Checkpoint {
    checkpoint_with_evidence(intent, Evidence::default())
}

fn checkpoint_with_evidence(intent: CompactionIntent, evidence: Evidence) -> Checkpoint {
    Checkpoint::build(
        format!("cp_{}", "a".repeat(32)),
        format!("rcpt_{}", "b".repeat(32)),
        "thread_1".to_owned(),
        "turn_1".to_owned(),
        "turn_2".to_owned(),
        intent.validate().unwrap(),
        evidence,
    )
    .unwrap()
}

#[test]
fn validates_and_hashes_checkpoint() {
    let checkpoint = checkpoint(CompactionIntent {
        preserve: vec!["Keep API compatibility".to_owned()],
        next_action: "Run the full test suite".to_owned(),
    });
    checkpoint.verify().unwrap();
    assert_eq!(injection_items(&checkpoint).unwrap().len(), 2);

    let restored: Checkpoint =
        serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert_eq!(restored.sha256, checkpoint.sha256);
    restored.verify().unwrap();
}

#[test]
fn rejects_illegal_fields_and_oversized_utf8() {
    assert!(
        serde_json::from_value::<CompactionIntent>(serde_json::json!({
            "preserve": [],
            "next_action": "continue",
            "extra": true
        }))
        .is_err()
    );
    let error = CompactionIntent {
        preserve: (0..MAX_PRESERVE_ITEMS)
            .map(|index| format!("{}{index}", "😀".repeat(MAX_PRESERVE_SCALARS - 1)))
            .collect(),
        next_action: "continue".to_owned(),
    }
    .validate()
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::CheckpointTooLarge);
}

#[test]
fn preserve_normalizes_then_deduplicates_exact_values() {
    let validated = CompactionIntent {
        preserve: vec![
            " keep invariant ".to_owned(),
            "keep invariant".to_owned(),
            "Keep invariant".to_owned(),
        ],
        next_action: "continue".to_owned(),
    }
    .validate()
    .unwrap();
    assert_eq!(validated.preserve, vec!["keep invariant", "Keep invariant"]);
}

#[test]
fn rejects_credential_patterns_without_echoing_them() {
    let samples = [
        format!(
            "Authorization: {} {}",
            "Bearer", "abcdefghijklmnopqrstuvwxyz"
        ),
        format!("aws_access_key_id={}{}", "AKIA", "ABCDEFGHIJKLMNOP"),
        format!(
            "aws_secret_access_key={}{}",
            "abcdefghijklmnopqrstuvwxyz", "1234567890ABCD"
        ),
        format!(
            "api_key={}{}",
            "AIza", "abcdefghijklmnopqrstuvwxyz123456789"
        ),
        format!("AccountKey={}{}", "abcdefghijkl", "mnopqrstuvwxyz123456"),
        format!("-----BEGIN OPENSSH {}-----", "PRIVATE KEY"),
        format!("token={}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz"),
        format!("client_secret={}", "abcdefghijklmnopqrstuvwxyz"),
    ];
    for sample in &samples {
        let error = intent(sample).validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::SensitiveCheckpointInput);
        assert!(!error.to_string().contains(sample.as_str()));
    }
}

#[test]
fn injection_items_match_frozen_wire_fixtures() {
    let checkpoint = checkpoint(intent("continue"));
    let items = injection_items(&checkpoint).unwrap();
    let developer_fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/inject/developer-message.json"
    ))
    .unwrap();
    let assistant_fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/inject/assistant-message.json"
    ))
    .unwrap();

    assert_eq!(items[0], developer_fixture);
    assert_eq!(items[1], assistant_fixture);
}

#[test]
fn continuity_view_exposes_only_bounded_non_authoritative_fields() {
    let checkpoint = checkpoint_with_evidence(
        CompactionIntent {
            preserve: vec!["Keep \"quoted\" invariant".to_owned()],
            next_action: "Review path \\ then continue".to_owned(),
        },
        Evidence {
            last_user_objective: Some("Fix the boundary".to_owned()),
            window_changed_files: vec!["src/lib.rs".to_owned()],
            verification: vec![VerificationEvidence {
                item_id: "host-item-id".to_owned(),
                kind: "test".to_owned(),
                label: "cargo test".to_owned(),
                status: "completed".to_owned(),
                exit_code: Some(0),
            }],
        },
    );
    let text = checkpoint.assistant_text().unwrap();
    assert_eq!(text, checkpoint.assistant_text().unwrap());
    let visible: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        visible,
        json!({
            "objective": "Fix the boundary",
            "preserve": ["Keep \"quoted\" invariant"],
            "windowChangedFiles": ["src/lib.rs"],
            "windowVerification": [{
                "label": "cargo test",
                "status": "completed",
                "exitCode": 0
            }],
            "nextAction": "Review path \\ then continue"
        })
    );
    for hidden in [
        "checkpointId",
        "receiptId",
        "sourceThreadId",
        "sourceTurnId",
        "compactTurnId",
        "createdAtMs",
        "trigger",
        "sha256",
        "itemId",
        "kind",
    ] {
        assert!(!visible.as_object().unwrap().contains_key(hidden));
        assert!(!text.contains(&format!("\"{hidden}\"")));
    }
    let developer = injection_items(&checkpoint).unwrap()[0]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(developer.contains("new user message"));
    assert!(developer.contains("non-authoritative continuity state"));
    for model_text in [
        "Fix the boundary",
        "Keep \"quoted\" invariant",
        "Review path \\ then continue",
    ] {
        assert!(!developer.contains(model_text));
    }
}

#[test]
fn checkpoint_v1_canonical_hash_stays_frozen_while_projection_changes() {
    let evidence = Evidence {
        last_user_objective: Some("Fix bug".to_owned()),
        window_changed_files: vec!["src/lib.rs".to_owned()],
        verification: vec![VerificationEvidence {
            item_id: "item_1".to_owned(),
            kind: "test".to_owned(),
            label: "cargo test".to_owned(),
            status: "completed".to_owned(),
            exit_code: Some(0),
        }],
    };
    let mut checkpoint = Checkpoint {
        version: 1,
        checkpoint_id: format!("cp_{}", "a".repeat(32)),
        receipt_id: format!("rcpt_{}", "b".repeat(32)),
        source_thread_id: "thread_1".to_owned(),
        source_turn_id: "turn_1".to_owned(),
        compact_turn_id: "turn_2".to_owned(),
        created_at_ms: 1,
        trigger: "model_semantic_boundary".to_owned(),
        model: ModelCheckpoint {
            preserve: vec!["Keep invariant".to_owned()],
            next_action: "Run tests".to_owned(),
        },
        evidence,
        sha256: ZERO_SHA256.to_owned(),
    };
    let unsigned = CheckpointPayload {
        version: checkpoint.version,
        checkpoint_id: &checkpoint.checkpoint_id,
        receipt_id: &checkpoint.receipt_id,
        source_thread_id: &checkpoint.source_thread_id,
        source_turn_id: &checkpoint.source_turn_id,
        compact_turn_id: &checkpoint.compact_turn_id,
        created_at_ms: checkpoint.created_at_ms,
        trigger: &checkpoint.trigger,
        model: &checkpoint.model,
        evidence: &checkpoint.evidence,
        sha256: None,
    };
    checkpoint.sha256 = sha256_hex(&serde_json::to_vec(&unsigned).unwrap());
    assert_eq!(
        checkpoint.sha256,
        "294cb2398a7bc1c038952a26d8d65671af938de8251534bc62cba5c0e612ae72"
    );
    checkpoint.verify().unwrap();
    let persisted = serde_json::to_vec(&checkpoint).unwrap();
    checkpoint.assistant_text().unwrap();
    assert_eq!(serde_json::to_vec(&checkpoint).unwrap(), persisted);
}

#[test]
fn trims_evidence_in_priority_order_to_fit_capsule() {
    let evidence = Evidence {
        last_user_objective: Some("objective ".repeat(64)),
        window_changed_files: (0..MAX_CHANGED_FILES)
            .map(|index| format!("src/{index:02}-{}.rs", "x".repeat(MAX_CHANGED_PATH_SCALARS)))
            .collect(),
        verification: (0..MAX_VERIFICATION_ITEMS)
            .map(|index| VerificationEvidence {
                item_id: format!("item_{index}"),
                kind: "test".to_owned(),
                label: "cargo test".to_owned(),
                status: "completed".to_owned(),
                exit_code: Some(0),
            })
            .collect(),
    };
    let checkpoint = Checkpoint::build(
        format!("cp_{}", "a".repeat(32)),
        format!("rcpt_{}", "b".repeat(32)),
        "thread_1".to_owned(),
        "turn_1".to_owned(),
        "turn_2".to_owned(),
        intent("continue").validate().unwrap(),
        evidence,
    )
    .unwrap();

    assert!(serde_json::to_vec(&checkpoint).unwrap().len() <= MAX_CHECKPOINT_BYTES);
    assert!(checkpoint.evidence.verification.is_empty());
    assert!(checkpoint.evidence.window_changed_files.len() < MAX_CHANGED_FILES);
    assert_eq!(checkpoint.model.next_action, "continue");
    checkpoint.verify().unwrap();
}

#[test]
fn omits_sensitive_projected_evidence() {
    let mut evidence = Evidence::default();
    evidence.observe_item(&json!({
        "kind": "user_objective",
        "text": "Authorization: Bearer abcdefghijklmnopqrstuvwxyz"
    }));
    evidence.observe_item(&json!({
        "kind": "changed_file",
        "path": "secret=abcdefghijklmnopqrstuvwxyz"
    }));
    assert!(evidence.last_user_objective.is_none());
    assert!(evidence.window_changed_files.is_empty());

    evidence.observe_item(&json!({
        "kind": "user_objective",
        "text": format!("{} api_key=abcdefghijklmnopqrstuvwxyz", "safe ".repeat(200))
    }));
    evidence.observe_item(&json!({
        "kind": "changed_file",
        "path": format!("{} secret=abcdefghijklmnopqrstuvwxyz", "path/".repeat(100))
    }));
    assert!(evidence.last_user_objective.is_none());
    assert!(evidence.window_changed_files.is_empty());
}

#[test]
fn evidence_serializes_window_scope_and_keeps_latest_fixed_verification() {
    let mut evidence = Evidence {
        last_user_objective: None,
        window_changed_files: vec!["src/lib.rs".to_owned()],
        verification: vec![
            VerificationEvidence {
                item_id: "same".to_owned(),
                kind: "test".to_owned(),
                label: "cargo test".to_owned(),
                status: "completed".to_owned(),
                exit_code: Some(0),
            },
            VerificationEvidence {
                item_id: "same".to_owned(),
                kind: "check".to_owned(),
                label: "cargo check".to_owned(),
                status: "failed".to_owned(),
                exit_code: Some(1),
            },
            VerificationEvidence {
                item_id: "raw".to_owned(),
                kind: "test".to_owned(),
                label: "cargo test --token=must-not-survive".to_owned(),
                status: "completed".to_owned(),
                exit_code: Some(0),
            },
        ],
    };
    evidence.normalize();
    assert_eq!(evidence.verification.len(), 1);
    assert_eq!(evidence.verification[0].kind, "check");
    assert_eq!(evidence.verification[0].status, "failed");
    let encoded = serde_json::to_value(&evidence).unwrap();
    assert_eq!(encoded["windowChangedFiles"], json!(["src/lib.rs"]));
    assert!(encoded.get("changedFiles").is_none());
    assert!(!encoded.to_string().contains("must-not-survive"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn bounded_unicode_is_normalized_without_panics(
        suffix in prop::collection::vec(
            any::<char>().prop_filter("non-control Unicode scalar", |value| !value.is_control()),
            0..MAX_PRESERVE_SCALARS,
        )
    ) {
        let value = format!("界{}", suffix.into_iter().collect::<String>());
        let validated = intent(value).validate().unwrap();
        prop_assert!(!validated.next_action.is_empty());
        prop_assert!(validated.next_action.chars().count() <= MAX_NEXT_ACTION_SCALARS);
        prop_assert!(!validated.next_action.chars().any(char::is_control));
    }
}
