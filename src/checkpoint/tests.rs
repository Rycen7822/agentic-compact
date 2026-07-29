use super::*;
use proptest::prelude::*;

fn intent(next_action: impl Into<String>) -> CompactionIntent {
    CompactionIntent {
        preserve: Vec::new(),
        next_action: next_action.into(),
    }
}

fn checkpoint(intent: CompactionIntent) -> Checkpoint {
    Checkpoint::build(
        format!("cp_{}", "a".repeat(32)),
        format!("rcpt_{}", "b".repeat(32)),
        "thread_1".to_owned(),
        "turn_1".to_owned(),
        "turn_2".to_owned(),
        intent.validate().unwrap(),
        Evidence::default(),
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
        preserve: vec!["😀".repeat(MAX_PRESERVE_SCALARS); MAX_PRESERVE_ITEMS],
        next_action: "continue".to_owned(),
    }
    .validate()
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::CheckpointTooLarge);
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
    assert_eq!(items[1]["type"], assistant_fixture["type"]);
    assert_eq!(items[1]["role"], assistant_fixture["role"]);
    assert_eq!(
        items[1]["content"][0]["type"],
        assistant_fixture["content"][0]["type"]
    );
    assert!(
        items[1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("<agentic_compact_checkpoint ")
    );
}

#[test]
fn trims_evidence_in_priority_order_to_fit_capsule() {
    let evidence = Evidence {
        last_user_objective: Some("objective ".repeat(64)),
        changed_files: (0..MAX_CHANGED_FILES)
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
    assert!(checkpoint.evidence.changed_files.len() < MAX_CHANGED_FILES);
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
    assert!(evidence.changed_files.is_empty());
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
