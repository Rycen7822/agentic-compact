use serde_json::Value;

const TARGET_SCHEMA: &str =
    include_str!("fixtures/app-server/codex-cli-0.146.0/codex_app_server_protocol.v2.schemas.json");

const CONTRACTS: [(&str, &str, &str); 2] = [
    (
        TARGET_SCHEMA,
        include_str!("fixtures/app-server/codex-cli-0.146.0/baseline.json"),
        include_str!("fixtures/codex-cli/codex-cli-0.146.0/plugin-cli.json"),
    ),
    (
        include_str!(
            "fixtures/app-server/codex-cli-0.145.0/codex_app_server_protocol.v2.schemas.json"
        ),
        include_str!("fixtures/app-server/codex-cli-0.145.0/baseline.json"),
        include_str!("fixtures/codex-cli/codex-cli-0.145.0/plugin-cli.json"),
    ),
];

fn contains_string(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => value == expected,
        Value::Array(values) => values.iter().any(|value| contains_string(value, expected)),
        Value::Object(values) => values
            .values()
            .any(|value| contains_string(value, expected)),
        _ => false,
    }
}

fn required_contains(definition: &Value, field: &str) -> bool {
    definition["required"]
        .as_array()
        .is_some_and(|required| required.iter().any(|value| value == field))
}

fn thread_item<'a>(definitions: &'a Value, item_type: &str) -> &'a Value {
    definitions["ThreadItem"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["properties"]["type"]["enum"][0] == item_type)
        .unwrap_or_else(|| panic!("missing ThreadItem variant {item_type}"))
}

#[test]
fn frozen_schema_contains_every_mandatory_method() {
    for (schema, _, _) in CONTRACTS {
        let schema: Value = serde_json::from_str(schema).unwrap();
        for method in [
            "initialize",
            "thread/loaded/list",
            "thread/read",
            "thread/resume",
            "thread/compact/start",
            "thread/inject_items",
            "thread/unsubscribe",
            "turn/start",
            "turn/started",
            "turn/completed",
            "item/started",
            "item/completed",
            "thread/status/changed",
            "thread/tokenUsage/updated",
        ] {
            assert!(contains_string(&schema, method), "missing method {method}");
        }
    }
}

#[test]
fn frozen_request_and_projection_shapes_match_the_client() {
    for (schema, _, _) in CONTRACTS {
        assert_request_and_projection_shapes(schema);
    }
}

#[test]
fn target_schema_freezes_turn_and_active_work_shapes() {
    let schema: Value = serde_json::from_str(TARGET_SCHEMA).unwrap();
    let definitions = &schema["definitions"];
    assert!(required_contains(&definitions["Thread"], "turns"));
    assert_eq!(
        definitions["Thread"]["properties"]["turns"]["type"],
        "array"
    );

    let turn = &definitions["Turn"];
    for field in ["id", "items", "status"] {
        assert!(required_contains(turn, field));
    }
    assert_eq!(turn["properties"]["items"]["type"], "array");
    assert_eq!(turn["properties"]["itemsView"]["default"], "full");
    for view in ["notLoaded", "summary", "full"] {
        assert!(contains_string(&definitions["TurnItemsView"], view));
    }

    let compact = thread_item(definitions, "contextCompaction");
    assert!(required_contains(compact, "id"));
    assert!(required_contains(compact, "type"));
    assert!(compact["properties"].get("status").is_none());

    for (item_type, status_definition) in [
        ("commandExecution", "CommandExecutionStatus"),
        ("fileChange", "PatchApplyStatus"),
        ("mcpToolCall", "McpToolCallStatus"),
        ("dynamicToolCall", "DynamicToolCallStatus"),
        ("collabAgentToolCall", "CollabAgentToolCallStatus"),
    ] {
        assert!(required_contains(
            thread_item(definitions, item_type),
            "status"
        ));
        assert!(contains_string(
            &definitions[status_definition],
            "inProgress"
        ));
    }
    let image = thread_item(definitions, "imageGeneration");
    assert!(required_contains(image, "status"));
    assert_eq!(image["properties"]["status"]["type"], "string");
}

fn assert_request_and_projection_shapes(schema: &str) {
    let schema: Value = serde_json::from_str(schema).unwrap();
    let definitions = &schema["definitions"];
    assert!(required_contains(
        &definitions["InitializeParams"],
        "clientInfo"
    ));
    for capability in [
        "experimentalApi",
        "optOutNotificationMethods",
        "requestAttestation",
    ] {
        assert!(
            definitions["InitializeCapabilities"]["properties"]
                .get(capability)
                .is_some(),
            "missing initialize capability {capability}"
        );
    }

    assert_eq!(
        definitions["ThreadLoadedListResponse"]["properties"]["data"]["items"]["type"],
        "string"
    );
    for definition in [
        "ThreadReadParams",
        "ThreadResumeParams",
        "ThreadCompactStartParams",
        "ThreadInjectItemsParams",
        "ThreadUnsubscribeParams",
        "TurnStartParams",
    ] {
        assert!(
            required_contains(&definitions[definition], "threadId"),
            "{definition} no longer requires threadId"
        );
    }
    assert!(required_contains(
        &definitions["ThreadInjectItemsParams"],
        "items"
    ));
    assert!(required_contains(&definitions["TurnStartParams"], "input"));
    assert_eq!(
        definitions["TurnStartParams"]["properties"]["input"]["type"],
        "array"
    );

    for notification in [
        "TurnStartedNotification",
        "TurnCompletedNotification",
        "ItemStartedNotification",
        "ItemCompletedNotification",
    ] {
        assert!(required_contains(&definitions[notification], "threadId"));
    }
    assert!(required_contains(
        &definitions["ItemStartedNotification"],
        "startedAtMs"
    ));
    assert!(required_contains(
        &definitions["ItemCompletedNotification"],
        "completedAtMs"
    ));
}

#[test]
fn baselines_are_stable_only_and_bound_to_supported_codex_versions() {
    let expected = [
        (
            "codex-cli 0.146.0",
            "rust-v0.146.0",
            "e363b08c9175ac1cbe5893615dd2cb9ddf95043b",
            275,
            "2e863156ed35ecc5253b1e2f907a9143077b9f7cb51942070c61996471ff6e04",
            "2a56e22ada954380862fe419c39825d45db2d2e7f7f096dd882fa6cc4600cc19",
        ),
        (
            "codex-cli 0.145.0",
            "rust-v0.145.0",
            "25af12f7e61572b0bc18ddb1008be543b91519b0",
            273,
            "a2a05dafaa1acb002a45eaec0a462de5b13694fcfcd7bc43305f14781ce7be14",
            "72a461ea8036d16eb8403ceddfdc7e53905980e2873611e5ab0d6ec13a98e904",
        ),
    ];
    for ((_, baseline, _), (version, tag, commit, count, binary_hash, schema_hash)) in
        CONTRACTS.into_iter().zip(expected)
    {
        let baseline: Value = serde_json::from_str(baseline).unwrap();
        assert_eq!(baseline["codexVersion"], version);
        assert_eq!(baseline["sourceTag"], tag);
        assert_eq!(baseline["sourceCommit"], commit);
        assert_eq!(baseline["experimentalApi"], false);
        assert_eq!(baseline["generatedSchemaFileCount"], count);
        assert_eq!(
            baseline["generatedSchemaHashAlgorithm"],
            "sha256(sorted(relative-path NUL file-sha256 LF))"
        );
        assert_eq!(baseline["nativeBinarySha256"], binary_hash);
        assert_eq!(baseline["generatedSchemaBundleSha256"], schema_hash);
    }
}

#[test]
fn plugin_cli_uses_verified_commands_for_both_supported_versions() {
    for ((_, _, fixture), version) in CONTRACTS
        .into_iter()
        .zip(["codex-cli 0.146.0", "codex-cli 0.145.0"])
    {
        let fixture: Value = serde_json::from_str(fixture).unwrap();
        assert_eq!(fixture["schemaVersion"], 2);
        assert_eq!(fixture["codexVersion"], version);
        assert_eq!(fixture["selector"], "agentic-compact@agentic-compact");
        assert_eq!(
            fixture["commands"]["mcpGet"],
            serde_json::json!(["mcp", "get", "agentic-compact", "--json"])
        );
        assert_eq!(
            fixture["commands"]["mcpGetConfigOverrides"],
            serde_json::json!([
                "mcp_servers.agentic-compact.command={binary}",
                "mcp_servers.agentic-compact.args=[\"mcp\"]",
                "mcp_servers.agentic-compact.env_vars=[\"CODEX_HOME\"]",
                "mcp_servers.agentic-compact.default_tools_approval_mode=\"approve\""
            ])
        );
        assert_eq!(
            fixture["commands"]["pluginAdd"],
            serde_json::json!(["plugin", "add", "agentic-compact@agentic-compact", "--json"])
        );
        assert_eq!(
            fixture["commands"]["marketplaceRemove"],
            serde_json::json!([
                "plugin",
                "marketplace",
                "remove",
                "agentic-compact",
                "--json"
            ])
        );
        assert_eq!(fixture["verifiedInIsolatedCodexHome"], true);
    }
}
