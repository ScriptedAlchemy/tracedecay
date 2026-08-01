use super::*;
use serde_json::{Value, json};
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, RequestId,
    ResultContractRef, SafeDiagnostic,
};
use tracedecay_tool_catalog::{BindingId, SchemaId};

fn defs() -> Vec<ToolDefinition> {
    get_tool_definitions()
}

fn def(name: &str) -> ToolDefinition {
    defs()
        .into_iter()
        .find(|d| d.name == format!("tracedecay_{name}"))
        .unwrap()
}

#[test]
fn canonicalizes_alias_and_strip_prefix() {
    assert_eq!(canonical_tool_name("query"), "tracedecay_search");
    assert_eq!(
        canonical_tool_name("tracedecay_search"),
        "tracedecay_search"
    );
    assert_eq!(canonical_tool_name("dead-code"), "tracedecay_dead_code");
}

#[test]
fn parses_positional_required_string() {
    let d = def("search");
    let parsed = parse_invocation(&d, &["foo".to_string()]).unwrap();
    assert_eq!(parsed.tool_args, json!({ "query": "foo" }));
}

#[test]
fn coerces_integer_flag() {
    let d = def("search");
    let parsed = parse_invocation(
        &d,
        &["foo".to_string(), "--limit".to_string(), "25".to_string()],
    )
    .unwrap();
    assert_eq!(parsed.tool_args, json!({ "query": "foo", "limit": 25 }));
}

#[test]
fn rejects_non_numeric_flag() {
    let d = def("search");
    let err = parse_invocation(
        &d,
        &["foo".to_string(), "--limit".to_string(), "abc".to_string()],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("number") || msg.contains("integer"),
        "got: {msg}"
    );
}

#[test]
fn coerces_boolean_flag() {
    let d = def("context");
    let parsed = parse_invocation(
        &d,
        &[
            "describe X".to_string(),
            "--include-code".to_string(),
            "true".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(parsed.tool_args["include_code"], json!(true));
}

#[test]
fn missing_required_errors() {
    let d = def("search");
    let err = parse_invocation(&d, &[]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("missing required parameter"), "got: {msg}");
}

#[test]
fn args_escape_hatch() {
    let d = def("search");
    let parsed = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"query":"foo","limit":3}"#.to_string(),
        ],
    )
    .unwrap();
    assert_eq!(parsed.tool_args["query"], json!("foo"));
    assert_eq!(parsed.tool_args["limit"], json!(3));
}

#[test]
fn api_migration_cli_preserves_typed_plan_and_apply_payloads() {
    let plan_payload = json!({
        "family_id": "family.cli",
        "operations": [{
            "kind": "assert_stable_value",
            "operation_id": "protect-wire-name",
            "depends_on": [],
            "enclosing_symbol": {
                "node_id": "node-wire-name",
                "qualified_name": "crate::wire_name",
                "kind": "function",
                "file": "src/lib.rs",
                "old_name": "wire_name"
            },
            "category": "wire field",
            "exact_bytes": "\"stable_name\"",
            "occurrence_indexes": [0]
        }]
    });
    let parsed_plan = parse_invocation(
        &def("api_migration_plan"),
        &[
            "--args".to_owned(),
            serde_json::to_string(&plan_payload).unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(parsed_plan.tool_args, plan_payload);

    let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let apply_payload = json!({
        "plan": {
            "family_id": "family.cli",
            "repository_revision": "HEAD",
            "graph_revision": digest,
            "operations": plan_payload["operations"],
            "sites": [],
            "files": [],
            "blocked": false,
            "plan_digest": digest
        },
        "plan_digest": digest,
        "dry_run": true,
        "verify": true
    });
    let parsed_apply = parse_invocation(
        &def("api_migration_apply"),
        &[
            "--args".to_owned(),
            serde_json::to_string(&apply_payload).unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(parsed_apply.tool_args, apply_payload);
}

#[test]
fn args_escape_hatch_reads_at_file() {
    let d = def("search");
    let dir = std::env::temp_dir().join(format!("ts-args-at-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Payload comfortably above Linux's 128 KiB MAX_ARG_STRLEN to prove
    // the @file path carries what a literal argv string cannot.
    let big = "x".repeat(200 * 1024);
    let path = dir.join("payload.json");
    std::fs::write(&path, format!(r#"{{"query":"{big}","limit":7}}"#)).unwrap();
    let parsed =
        parse_invocation(&d, &["--args".to_string(), format!("@{}", path.display())]).unwrap();
    assert_eq!(parsed.tool_args["limit"], json!(7));
    assert_eq!(
        parsed.tool_args["query"].as_str().map(str::len),
        Some(big.len())
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn args_escape_hatch_reads_stdin_dash() {
    let d = def("search");
    let parsed = parse_invocation_with_stdin(&d, &["--args".to_string(), "-".to_string()], || {
        Ok(r#"{"query":"stdin","limit":9}"#.to_string())
    })
    .unwrap();
    assert_eq!(parsed.tool_args, json!({ "query": "stdin", "limit": 9 }));
}

#[test]
fn args_escape_hatch_reads_stdin_at_dash() {
    let d = def("search");
    let parsed = parse_invocation_with_stdin(&d, &["--args".to_string(), "@-".to_string()], || {
        Ok(r#"{"query":"stdin-at"}"#.to_string())
    })
    .unwrap();
    assert_eq!(parsed.tool_args, json!({ "query": "stdin-at" }));
}

#[test]
fn args_escape_hatch_reads_bare_path() {
    // `--args` is a whole-payload arg, so a bare file path works without the
    // `@` sigil — matching `memory curate --llm-ops <file>`.
    let d = def("search");
    let dir = std::env::temp_dir().join(format!("ts-args-bare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("payload.json");
    std::fs::write(&path, r#"{"query":"bare","limit":4}"#).unwrap();
    let parsed = parse_invocation(&d, &["--args".to_string(), path.display().to_string()]).unwrap();
    assert_eq!(parsed.tool_args, json!({ "query": "bare", "limit": 4 }));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn args_escape_hatch_missing_bare_file_errors() {
    let d = def("search");
    let err = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            "/nonexistent/tracedecay-args.json".to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("--args:"), "got: {msg}");
    assert!(msg.contains("readable file"), "got: {msg}");
    assert!(
        msg.contains("/nonexistent/tracedecay-args.json"),
        "got: {msg}"
    );
}

#[test]
fn args_escape_hatch_missing_at_file_errors() {
    let d = def("search");
    let err = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            "@/nonexistent/tracedecay-args.json".to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("failed to read @"), "got: {msg}");
}

#[test]
fn reserved_flags_extracted() {
    let d = def("search");
    let parsed = parse_invocation(
        &d,
        &[
            "foo".to_string(),
            "--json".to_string(),
            "--project".to_string(),
            "/tmp/x".to_string(),
        ],
    )
    .unwrap();
    assert!(parsed.raw_json);
    assert_eq!(parsed.project.as_deref(), Some("/tmp/x"));
}

#[test]
fn help_flag_short_circuits() {
    let d = def("search");
    let parsed = parse_invocation(&d, &["--help".to_string()]).unwrap();
    assert!(parsed.show_help);
}

#[test]
fn unknown_tool_name_errors() {
    // canonical_tool_name only normalises — unknown names are caught by
    // the lookup in run(). Simulate the lookup here.
    let canonical = canonical_tool_name("totally-fake-tool");
    let found = defs().into_iter().any(|d| d.name == canonical);
    assert!(!found);
}

#[test]
fn array_value_collected_via_repetition() {
    let d = def("context");
    let parsed = parse_invocation(
        &d,
        &[
            "x".to_string(),
            "--keywords".to_string(),
            "auth".to_string(),
            "--keywords".to_string(),
            "login".to_string(),
        ],
    )
    .unwrap();
    // After parse, the second occurrence wraps into an array. finalize is
    // only called via the run path; here we just observe the merged shape.
    let kw = &parsed.tool_args["keywords"];
    assert!(kw.is_array(), "expected array, got {kw}");
    let arr = kw.as_array().unwrap();
    assert_eq!(arr.len(), 2);
}

#[test]
fn finalize_arrays_splits_csv() {
    let d = def("context");
    let mut map = Map::new();
    map.insert("keywords".to_string(), json!("auth,login,session"));
    finalize_arrays(&d, &mut map);
    let arr = map["keywords"].as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], json!("auth"));
    assert_eq!(arr[2], json!("session"));
}

#[test]
fn explicit_project_lcm_dispatch_allows_first_touch_init() {
    let dispatch = DaemonToolDispatch::project_scoped(
        Some("/tmp/project".to_string()),
        "tracedecay_lcm_status",
    );

    assert!(dispatch.allow_init);
}

#[test]
fn user_storage_scope_dispatch_never_invents_a_project_from_cwd() {
    let dispatch = DaemonToolDispatch::for_tool(
        None,
        "tracedecay_lcm_preflight",
        &json!({
            "provider": "hermes",
            "session_id": "stock-check-session",
            "storage_scope": "user",
            "transcript_projection": true,
            "messages": [],
        }),
    );

    assert_eq!(dispatch.project_path, None);
    assert!(!dispatch.allow_init);
}

#[test]
fn user_memory_scope_dispatch_is_projectless() {
    let dispatch = DaemonToolDispatch::for_tool(
        None,
        "tracedecay_fact_store",
        &json!({
            "action": "list",
            "memory_scope": "user",
        }),
    );

    assert_eq!(dispatch.project_path, None);
}

#[test]
fn filesystem_root_path_is_never_accepted_as_discovered_project() {
    assert!(is_filesystem_root(std::path::Path::new("/")));
    assert!(!is_filesystem_root(std::path::Path::new("/tmp/project")));
    assert!(!is_filesystem_root(std::path::Path::new(".")));
}

// --- Validation gate and corrective-error contract ---

#[test]
fn unknown_key_errors_with_did_you_mean_and_valid_keys() {
    let d = def("search");
    let err = parse_invocation(
        &d,
        &[
            "--query".to_string(),
            "gamma".to_string(),
            "--limt".to_string(),
            "2".to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("unknown parameter `--limt`"), "got: {msg}");
    assert!(msg.contains("did you mean `--limit`?"), "got: {msg}");
    assert!(msg.contains("--query (required)"), "got: {msg}");
}

#[test]
fn unknown_key_in_args_payload_errors_too() {
    let d = def("search");
    let err = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"query":"x","limt":2}"#.to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("unknown parameter `--limt`"), "got: {msg}");
}

#[test]
fn invalid_enum_errors_with_allowed_values() {
    let d = def("gini");
    let err = parse_invocation(&d, &["--metric".to_string(), "bogus".to_string()]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("`bogus` is not one of:"), "got: {msg}");
    assert!(msg.contains("complexity"), "got: {msg}");
    assert!(msg.contains("fan_in"), "got: {msg}");
}

#[test]
fn valid_enum_passes() {
    let d = def("gini");
    let parsed = parse_invocation(&d, &["--metric".to_string(), "fan_in".to_string()]).unwrap();
    assert_eq!(parsed.tool_args["metric"], json!("fan_in"));
}

#[test]
fn args_payload_missing_required_errors() {
    let d = def("search");
    let err =
        parse_invocation(&d, &["--args".to_string(), r#"{"limit":3}"#.to_string()]).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("missing required parameter `--query`"),
        "got: {msg}"
    );
}

#[test]
fn args_payload_wrong_type_errors() {
    let d = def("search");
    let err = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"query":["not","a","string"]}"#.to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("--query expects a JSON string"), "got: {msg}");
}

#[test]
fn args_payload_required_null_errors() {
    let d = def("search");
    let err =
        parse_invocation(&d, &["--args".to_string(), r#"{"query":null}"#.to_string()]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("--query expects a JSON string"), "got: {msg}");
}

#[test]
fn args_payload_optional_null_is_absent() {
    let d = def("lcm_compress");
    let parsed = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"provider":"hermes","session_id":"s1","messages":[{"role":"user","content":"hello"}],"focus_topic":null}"#
                .to_string(),
        ],
    )
    .unwrap();
    assert!(parsed.tool_args["focus_topic"].is_null());
}

#[test]
fn dispatch_routing_keys_bypass_unknown_key_gate() {
    // Dispatch reads top-level project_root, and LCM response handles can
    // target a separate live project; these must keep flowing through the gate.
    let d = def("fact_store");
    let parsed = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"action":"list","project_root":"/tmp/p","response_handle_project_root":"/tmp/r","cwd":"/tmp"}"#
                .to_string(),
        ],
    )
    .unwrap();
    assert_eq!(parsed.tool_args["action"], json!("list"));
}

#[test]
fn removed_storage_routing_keys_fail_validation() {
    let d = def("fact_store");
    for removed in ["storage_scope", "hermes_home"] {
        let payload = format!(r#"{{"action":"list","{removed}":"removed"}}"#);
        let error = parse_invocation(&d, &["--args".to_string(), payload]).unwrap_err();
        let flag = format!("--{}", removed.replace('_', "-"));
        assert!(
            error.to_string().contains("unknown parameter") && error.to_string().contains(&flag),
            "removed argument should fail clearly: {error}"
        );
    }
}

#[test]
fn lcm_cli_help_exposes_scope_without_hermes_profile_routing() {
    for tool_name in [
        "lcm_status",
        "lcm_load_session",
        "lcm_grep",
        "lcm_describe",
        "lcm_expand",
        "lcm_expand_query",
        "lcm_preflight",
        "lcm_compress",
        "lcm_session_boundary",
        "lcm_doctor",
        "hermes_skill_bridge",
    ] {
        let help = render_tool_cli_help(&def(tool_name));
        if tool_name.starts_with("lcm_") {
            assert!(help.contains("--storage-scope"), "{tool_name}: {help}");
        }
        assert!(!help.contains("--hermes-home"), "{tool_name}: {help}");
        assert!(!help.contains("hermes_profile"), "{tool_name}: {help}");
    }
}

#[test]
fn fact_type_alias_maps_to_category() {
    let d = def("fact_store");
    let parsed = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"action":"add","content":"hello","fact_type":"decision"}"#.to_string(),
        ],
    )
    .unwrap();
    assert_eq!(parsed.tool_args["category"], json!("decision"));
    assert!(parsed.tool_args.get("fact_type").is_none());
}

#[test]
fn fact_type_alias_conflict_errors() {
    let d = def("fact_store");
    let err = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"action":"add","content":"hello","category":"decision","fact_type":"project"}"#
                .to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("legacy alias"), "got: {msg}");
}

#[test]
fn per_key_json_array_of_pairs_parses() {
    let d = def("multi_str_replace");
    let parsed = parse_invocation(
        &d,
        &[
            "--path".to_string(),
            "lib.rs".to_string(),
            "--replacements".to_string(),
            r#"[["alpha","gamma"]]"#.to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        parsed.tool_args["replacements"],
        json!([["alpha", "gamma"]])
    );
}

#[test]
fn comma_split_array_of_pairs_gets_corrective_error() {
    let d = def("multi_str_replace");
    let err = parse_invocation(
        &d,
        &[
            "--path".to_string(),
            "lib.rs".to_string(),
            "--replacements".to_string(),
            "alpha,gamma".to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("array of arrays"), "got: {msg}");
    assert!(msg.contains("--args -"), "got: {msg}");
}

#[test]
fn per_key_json_object_parses() {
    let d = def("message_search");
    let parsed = parse_invocation(
        &d,
        &[
            "--query".to_string(),
            "zeta".to_string(),
            "--project-selector".to_string(),
            r#"{"project_id":"other"}"#.to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        parsed.tool_args["project_selector"],
        json!({"project_id": "other"})
    );
}

#[test]
fn per_key_non_json_object_gets_corrective_type_error() {
    let d = def("message_search");
    let err = parse_invocation(
        &d,
        &[
            "--query".to_string(),
            "zeta".to_string(),
            "--project-selector".to_string(),
            "other".to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("expects a JSON object"), "got: {msg}");
    assert!(msg.contains("--args"), "got: {msg}");
}

#[test]
fn key_equals_value_form_accepted() {
    let d = def("search");
    let parsed =
        parse_invocation(&d, &["--query=foo".to_string(), "--limit=3".to_string()]).unwrap();
    assert_eq!(parsed.tool_args, json!({ "query": "foo", "limit": 3 }));
}

#[test]
fn reserved_flag_equals_value_form_accepted() {
    let d = def("search");
    let parsed = parse_invocation(&d, &["--args={\"query\":\"eq\"}".to_string()]).unwrap();
    assert_eq!(parsed.tool_args, json!({ "query": "eq" }));
}

#[test]
fn fact_feedback_bare_helpful_flag_does_not_swallow_note_flag() {
    // Regression for the live-verified defect: `tracedecay tool fact_feedback
    // --fact-id <id> --helpful --note <text>` used to fail with "expected a
    // boolean, got --note" because the bare `--helpful` consumed `--note` as
    // its own value. It must now bind `helpful=true` and let `--note` parse
    // normally.
    let d = def("fact_feedback");
    let parsed = parse_invocation(
        &d,
        &[
            "--fact-id".to_string(),
            "5".to_string(),
            "--helpful".to_string(),
            "--note".to_string(),
            "great context".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        parsed.tool_args,
        json!({ "fact_id": "5", "helpful": true, "note": "great context" })
    );
}

#[test]
fn bare_boolean_flag_at_end_of_args_defaults_to_true() {
    let d = def("context");
    let parsed = parse_invocation(&d, &["how".to_string(), "--include-code".to_string()]).unwrap();
    assert_eq!(
        parsed.tool_args,
        json!({ "task": "how", "include_code": true })
    );
}

#[test]
fn bare_boolean_flag_before_next_flag_does_not_swallow_it() {
    // A bare `--include-code` immediately followed by another flag must not
    // consume that flag as its own value (previously this produced a
    // confusing "expected a boolean ... got --json" error and silently
    // dropped `--json` from parsing). It now defaults to `true` and leaves
    // `--json` to be parsed on its own.
    let d = def("context");
    let parsed = parse_invocation(
        &d,
        &[
            "how".to_string(),
            "--include-code".to_string(),
            "--json".to_string(),
        ],
    )
    .unwrap();
    assert!(parsed.raw_json);
    assert_eq!(
        parsed.tool_args,
        json!({ "task": "how", "include_code": true })
    );
}

#[test]
fn boolean_flag_with_explicit_value_after_it_is_still_consumed() {
    let d = def("context");
    let parsed = parse_invocation(
        &d,
        &[
            "how".to_string(),
            "--include-code".to_string(),
            "false".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        parsed.tool_args,
        json!({ "task": "how", "include_code": false })
    );
}

#[test]
fn boolean_flag_with_invalid_explicit_value_still_errors() {
    let d = def("context");
    let err = parse_invocation(
        &d,
        &[
            "how".to_string(),
            "--include-code".to_string(),
            "maybe".to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("expected a boolean"), "got: {msg}");
    assert!(msg.contains("--include-code true"), "got: {msg}");
}

#[test]
fn single_dash_known_flag_gets_did_you_mean() {
    let d = def("search");
    let err = parse_invocation(&d, &["-query".to_string(), "foo".to_string()]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("did you mean `--query`?"), "got: {msg}");
}

#[test]
fn missing_required_error_includes_usage_example() {
    let d = def("search");
    let err = parse_invocation(&d, &[]).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("missing required parameter `--query`"),
        "got: {msg}"
    );
    assert!(msg.contains("tracedecay tool search --query"), "got: {msg}");
}

#[test]
fn dry_run_flag_remains_reserved_when_the_tool_does_not_declare_it() {
    let d = def("search");
    let parsed = parse_invocation(&d, &["foo".to_string(), "--dry-run".to_string()]).unwrap();
    assert!(parsed.dry_run);
    assert_eq!(parsed.tool_args, json!({ "query": "foo" }));
}

#[test]
fn dry_run_flag_is_forwarded_when_the_tool_declares_it() {
    let d = def("str_replace");
    let parsed = parse_invocation(
        &d,
        &[
            "--path".to_string(),
            "src/lib.rs".to_string(),
            "--old-str".to_string(),
            "old".to_string(),
            "--new-str".to_string(),
            "new".to_string(),
            "--dry-run".to_string(),
            "true".to_string(),
        ],
    )
    .unwrap();

    assert!(!parsed.dry_run);
    assert_eq!(parsed.tool_args["dry_run"], json!(true));
}

#[test]
fn unknown_tool_suggestion_finds_nearest_name() {
    let suggestion = nearest_tool_name("tracedecay_dead_coed", &defs());
    assert_eq!(suggestion.as_deref(), Some("dead_code"));
}

#[test]
fn edit_distance_basics() {
    assert_eq!(edit_distance("limit", "limit"), 0);
    assert_eq!(edit_distance("limt", "limit"), 1);
    assert_eq!(edit_distance("", "abc"), 3);
}

#[test]
fn validation_skips_opaque_schemas() {
    // A definition without properties must be treated as opaque: no unknown
    // key rejection, so dynamic tools can't be bricked by the walker.
    let d = ToolDefinition {
        name: "tracedecay_opaque".to_string(),
        description: String::new(),
        input_schema: json!({ "type": "object" }),
        annotations: None,
        meta: None,
    };
    let parsed = parse_invocation(
        &d,
        &["--args".to_string(), r#"{"anything":"goes"}"#.to_string()],
    )
    .unwrap();
    assert_eq!(parsed.tool_args["anything"], json!("goes"));
}

#[test]
fn join_content_text_returns_single_block() {
    let value = json!({ "content": [{ "type": "text", "text": "only payload" }] });
    assert_eq!(join_content_text(&value), "only payload");
}

#[test]
fn join_content_text_joins_warning_and_payload() {
    // A prepended warning block must not shadow the payload+metrics block;
    // the CLI historically printed only content[0].text and dropped this.
    let value = json!({
        "content": [
            { "type": "text", "text": "warning: index is stale" },
            { "type": "text", "text": "actual payload\ntracedecay_metrics: 123" }
        ]
    });
    assert_eq!(
        join_content_text(&value),
        "warning: index is stale\n\nactual payload\ntracedecay_metrics: 123"
    );
}

#[test]
fn join_content_text_skips_empty_blocks() {
    let value = json!({
        "content": [
            { "type": "text", "text": "" },
            { "type": "text", "text": "payload" }
        ]
    });
    assert_eq!(join_content_text(&value), "payload");
}

#[test]
fn join_content_text_empty_when_no_content() {
    assert_eq!(join_content_text(&json!({})), "");
    assert_eq!(join_content_text(&json!({ "content": [] })), "");
}

#[test]
fn reject_tool_result_truncation_detects_content_envelope() {
    let value = json!({
        "content": [{
            "type": "text",
            "text": "{\"truncated\":true,\"original_chars\":16000,\"preview\":\"{}\",\"handle\":\"h1\"}"
        }]
    });
    let err = reject_tool_result_truncation(&value, "tracedecay_search").unwrap_err();
    let message = err.to_string();
    assert!(message.contains("truncated JSON"), "{message}");
    assert!(message.contains("tracedecay_retrieve"), "{message}");
    assert!(
        reject_tool_result_truncation(
            &json!({ "content": [{ "type": "text", "text": "{\"ok\":true}" }] }),
            "tracedecay_search"
        )
        .is_ok()
    );
    assert!(
        reject_tool_result_truncation(
            &json!({
                "content": [{
                    "type": "text",
                    "text": "{\"truncated\":true,\"matches\":[]}"
                }]
            }),
            "tracedecay_grep"
        )
        .is_ok()
    );
}

#[test]
fn canonical_problem_markdown_matches_the_golden_contract() {
    let result: ApplicationResult<Value> = Err(ApplicationProblemEnvelope::new(
        ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 3).unwrap(),
        RequestId::new("request.cli.golden").unwrap(),
        ApplicationProblem::unavailable(
            SafeDiagnostic::new(
                "daemon_unavailable",
                "The owning TraceDecay daemon is unavailable",
            )
            .unwrap(),
        ),
    ));
    let view = crate::cli::output::view::CanonicalHumanView::from_application_result(
        "feedback_list",
        &BindingId::new("binding.cli.feedback-list.v1").unwrap(),
        &result,
    )
    .unwrap();
    let rendered = crate::cli::output::markdown::render(view);

    assert_eq!(
        rendered.as_str(),
        concat!(
            "## feedback\\_list\n",
            "\n- Operation: `feedback_list`",
            "\n- Binding: `binding.cli.feedback-list.v1`",
            "\n- Status: `problem`",
            "\n- Contract: `schema.test.result@3`",
            "\n- Problem: `daemon_unavailable`",
            "\n- Problem kind: `unavailable`",
            "\n- Problem revision: `1`",
            "\n- Owning layer: `application`",
            "\n- Terminality: `pre_admission`",
            "\n- Request: `request.cli.golden`",
            "\n- Trace: `request.cli.golden`",
            "\n- Message: The owning TraceDecay daemon is unavailable",
            "\n- Retryable: `true`",
            "\n- Retry: `after_delay`",
            "\n- Retry scope: `same_request`",
            "\n- Retry after: `none`",
            "\n- Cancellation stage: `none`",
            "\n- Details: none",
            "\n- Legal actions: `retry`",
            "\n- Coverage: `not_available`",
        )
    );
}

#[test]
fn application_problem_makes_the_tool_command_fail() {
    let request_id = RequestId::new("request.cli.configuration-conflict").unwrap();
    let result = ApplicationSurfaceInvocationResult {
        operation: ApplicationSurfaceOperation::ConfigurationSet,
        binding_id: BindingId::new("binding.cli.configuration-set.v1").unwrap(),
        result: Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 1).unwrap(),
            request_id,
            ApplicationProblem::unavailable(
                SafeDiagnostic::new(
                    "configuration_revision_conflict",
                    "The expected configuration revision is stale",
                )
                .unwrap(),
            ),
        )),
        requested_format: RequestedOutputFormat::Json,
    };

    let error = print_cli_application_surface(result, true)
        .expect_err("a canonical application problem must fail the CLI process");
    assert!(
        error
            .to_string()
            .contains("configuration_revision_conflict"),
        "{error}"
    );
}

/// Documented read-only invocations from `tracedecay tool <name> --help`, each
/// paired with the `format: "json"` presentation key the help text advertises.
fn documented_json_invocations() -> Vec<(&'static str, Value)> {
    vec![
        ("tracedecay_storage_status", json!({})),
        ("tracedecay_git_status", json!({})),
        ("tracedecay_git_diff", json!({})),
        ("tracedecay_git_history", json!({"count": 3})),
        (
            "tracedecay_source_outline",
            json!({"file": "src/update_cmd.rs"}),
        ),
        (
            "tracedecay_file_metadata",
            json!({"files": ["src/update_cmd.rs"]}),
        ),
    ]
}

fn with_format(mut args: Value, format: &str) -> Value {
    args.as_object_mut()
        .expect("documented invocations are objects")
        .insert("format".to_owned(), json!(format));
    args
}

#[test]
fn documented_format_argument_never_reaches_the_reviewed_request() {
    for (tool_name, args) in documented_json_invocations() {
        let operation = ApplicationSurfaceOperation::from_tool_name(tool_name)
            .unwrap_or_else(|| panic!("{tool_name} is an application surface operation"));

        let (request, format) =
            cli_surface_invocation(tool_name, with_format(args.clone(), "json"), false)
                .unwrap_or_else(|error| {
                    panic!("{tool_name} rejected a documented argument: {error}")
                });
        assert_eq!(format, RequestedOutputFormat::Json, "{tool_name}");
        assert_eq!(request, args, "{tool_name}");

        parse_application_surface_request(operation, request).unwrap_or_else(|error| {
            panic!("{tool_name} did not match its reviewed schema: {error}")
        });
    }
}

#[test]
fn cli_and_mcp_normalize_documented_arguments_identically() {
    for (tool_name, args) in documented_json_invocations() {
        let operation = ApplicationSurfaceOperation::from_tool_name(tool_name)
            .unwrap_or_else(|| panic!("{tool_name} is an application surface operation"));
        let arguments = with_format(args, "json");

        let (cli_request, cli_format) = cli_surface_invocation(tool_name, arguments.clone(), false)
            .unwrap_or_else(|error| panic!("{tool_name} CLI normalization failed: {error}"));
        // The MCP transport reaches the reviewed schema through the same
        // adapter; an argument accepted there must be accepted here.
        let mcp = normalize_application_tool_args(tool_name, arguments)
            .unwrap_or_else(|error| panic!("{tool_name} MCP normalization failed: {error}"));

        assert_eq!(cli_request, mcp.request, "{tool_name}");
        assert_eq!(cli_format, mcp.requested_format, "{tool_name}");

        let cli_parsed = serde_json::to_value(
            parse_application_surface_request(operation, cli_request).expect("cli request"),
        )
        .expect("cli request is serializable");
        let mcp_parsed = serde_json::to_value(
            parse_application_surface_request(operation, mcp.request).expect("mcp request"),
        )
        .expect("mcp request is serializable");
        assert_eq!(cli_parsed, mcp_parsed, "{tool_name}");
    }
}

#[test]
fn json_flag_and_json_format_select_the_same_output() {
    let (flag_request, flag_format) =
        cli_surface_invocation("tracedecay_storage_status", json!({}), true).expect("flag");
    let (format_request, format_format) = cli_surface_invocation(
        "tracedecay_storage_status",
        json!({"format": "json"}),
        false,
    )
    .expect("format");

    assert_eq!(flag_format, RequestedOutputFormat::Json);
    assert_eq!(format_format, RequestedOutputFormat::Json);
    assert_eq!(flag_request, format_request);
}

#[test]
fn markdown_remains_the_default_presentation() {
    let (_request, format) =
        cli_surface_invocation("tracedecay_storage_status", json!({}), false).expect("default");
    assert_eq!(format, RequestedOutputFormat::Markdown);
}
