use super::*;
use serde_json::json;

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
fn profile_scoped_lcm_dispatch_detects_allowlisted_tool_and_scope() {
    assert!(is_profile_scoped_lcm_dispatch(
        "tracedecay_lcm_status",
        &json!({"storage_scope": "hermes_profile"})
    ));
}

#[test]
fn profile_scoped_lcm_dispatch_rejects_non_profile_or_non_lcm_calls() {
    assert!(!is_profile_scoped_lcm_dispatch(
        "tracedecay_lcm_status",
        &json!({"storage_scope": "project_local"})
    ));
    assert!(!is_profile_scoped_lcm_dispatch(
        "tracedecay_status",
        &json!({"storage_scope": "hermes_profile"})
    ));
}

// Registry integrity guardrail (companion to the handler lockstep tests in
// `mcp::tools::handlers`): the CLI routes profile-scoped LCM calls through
// `is_profile_scoped_lcm_dispatch`, which consults the hand-maintained
// `PROFILE_SCOPED_LCM_TOOLS` const. Any tool the MCP registry advertises as
// profile-scoped (storage_scope enum including `hermes_profile`) must also
// appear here, or its CLI invocations silently fall through to project
// initialization instead of profile-scoped dispatch. This fails in both
// directions when the const drifts from the registry.
#[test]
fn cli_profile_scoped_lcm_allowlist_matches_registry() {
    use std::collections::BTreeSet;

    let registry_profile_scoped: BTreeSet<String> = get_tool_definitions()
        .into_iter()
        .filter(|tool| {
            tool.input_schema["properties"]["storage_scope"]["enum"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == "hermes_profile"))
        })
        .map(|tool| tool.name)
        .collect();
    let cli_allowlist: BTreeSet<String> = PROFILE_SCOPED_LCM_TOOLS
        .iter()
        .map(|s| s.to_string())
        .collect();

    let missing_from_cli: Vec<String> = registry_profile_scoped
        .difference(&cli_allowlist)
        .cloned()
        .collect();
    assert!(
        missing_from_cli.is_empty(),
        "profile-scoped MCP tools missing from CLI PROFILE_SCOPED_LCM_TOOLS allowlist \
         (those calls would fall through to project init): {missing_from_cli:?}"
    );
    let stale_in_cli: Vec<String> = cli_allowlist
        .difference(&registry_profile_scoped)
        .cloned()
        .collect();
    assert!(
        stale_in_cli.is_empty(),
        "CLI PROFILE_SCOPED_LCM_TOOLS allowlist references tools no longer registered as \
         profile-scoped in the MCP registry: {stale_in_cli:?}"
    );
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
