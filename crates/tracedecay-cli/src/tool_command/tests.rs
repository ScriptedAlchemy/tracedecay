use super::*;
use serde_json::{Value, json};
use tracedecay_contracts::{
    ApplicationProblem, ApplicationProblemEnvelope, OpaqueCursor, PageRequest, RequestId,
    ResultContractRef, SafeDiagnostic,
};
use tracedecay_daemon_service::application_surface::retained::decode_request as decode_retained_request;
use tracedecay_daemon_service::application_surface::{
    parse_http_application_surface_request, resolve_application_surface_dispatch_with_controls,
    resolve_catalog_tool_binding,
};
use tracedecay_tool_catalog::{BindingId, BindingSurface, SchemaId};

fn defs() -> Vec<ToolDefinition> {
    get_tool_definitions().expect("tool definitions")
}

fn def(name: &str) -> ToolDefinition {
    defs()
        .into_iter()
        .find(|d| d.name == format!("tracedecay_{name}"))
        .unwrap()
}

#[test]
fn fact_store_tool_lookup_rejects_broad_and_accepts_exact_routes() {
    let definitions = defs();
    assert!(
        definitions
            .iter()
            .all(|definition| definition.name != "tracedecay_fact_store")
    );
    for name in [
        "fact_store_add",
        "fact_store_search",
        "fact_store_probe",
        "fact_store_related",
        "fact_store_reason",
        "fact_store_contradict",
        "fact_store_get",
        "fact_store_update",
        "fact_store_remove",
        "fact_store_supersede",
        "fact_store_list",
    ] {
        let canonical = canonical_tool_name(name);
        assert!(
            definitions
                .iter()
                .any(|definition| definition.name == canonical),
            "{canonical} must resolve through the CLI catalog"
        );
    }
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
fn application_operations_resolve_by_identity_and_by_cli_spelling() {
    for operation in ApplicationSurfaceOperation::ALL {
        assert_eq!(
            cli_application_operation(&canonical_tool_name(operation.as_str())),
            Some(operation),
            "{} must resolve by its canonical identity",
            operation.as_str()
        );
        assert_eq!(
            cli_application_operation(&canonical_tool_name(operation.mcp_operation_name())),
            Some(operation),
            "{} must resolve by its CLI binding spelling",
            operation.as_str()
        );
    }
    for spelling in ["diagnostics_read", "diagnostics", "tracedecay_diagnostics"] {
        assert_eq!(
            cli_application_operation(&canonical_tool_name(spelling)),
            Some(ApplicationSurfaceOperation::DiagnosticsRead),
            "{spelling}"
        );
    }
    assert_eq!(
        cli_application_operation(&canonical_tool_name("totally-fake-tool")),
        None
    );
}

#[test]
fn retryable_surface_refusals_stop_at_the_attempt_and_deadline_bounds() {
    let delay = Duration::from_millis(10);
    let roomy_deadline = Instant::now() + Duration::from_secs(1);
    assert_eq!(
        bounded_surface_retry_delay(Some(delay), 1, roomy_deadline),
        Some(delay)
    );
    assert_eq!(
        bounded_surface_retry_delay(Some(delay), 2, roomy_deadline),
        Some(delay)
    );
    assert_eq!(
        bounded_surface_retry_delay(Some(delay), 3, roomy_deadline),
        None,
        "the third typed refusal is surfaced instead of retried"
    );
    assert_eq!(
        bounded_surface_retry_delay(Some(delay), 1, Instant::now() + delay),
        None,
        "a retry that cannot complete inside the request deadline is refused"
    );
}

#[test]
fn whole_payload_invocation_parses_without_a_tool_definition() {
    let parsed = parse_whole_payload_invocation_with_stdin(
        &[
            "--project".to_owned(),
            "/tmp/project".to_owned(),
            "--args".to_owned(),
            r#"{"format":"json","include_branch_diagnostics":false}"#.to_owned(),
            "--json".to_owned(),
        ],
        || panic!("inline JSON must not read stdin"),
    )
    .expect("whole-payload invocation")
    .expect("whole-payload fast path");

    assert_eq!(
        parsed.tool_args,
        json!({"format": "json", "include_branch_diagnostics": false})
    );
    assert_eq!(parsed.project.as_deref(), Some("/tmp/project"));
    assert!(parsed.raw_json);
    assert!(!parsed.dry_run);
    assert!(!parsed.show_help);
}

#[test]
fn multi_root_execute_accepts_its_emitted_continuation_object() {
    let digest = |byte: char| {
        tracedecay_domain::ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("test digest")
    };
    let generation = tracedecay_domain::RootScopeOutcomeV1::new(
        digest('b'),
        tracedecay_domain::ScopeOutcome::<Option<tracedecay_domain::RootGenerationV1>>::Denied,
    )
    .expect("denied root generation");
    let emitted = tracedecay_contracts::MultiRootContinuationV1::new(
        digest('a'),
        vec![generation.clone()],
        vec![tracedecay_domain::RootScopeOutcomeV1::new(
            digest('b'),
            tracedecay_domain::ScopeOutcome::<Option<tracedecay_contracts::OpaqueCursor>>::Denied,
        )
        .expect("denied root cursor")],
        digest('c'),
        digest('d'),
        1,
    )
    .expect("page-zero continuation");
    let continuation = serde_json::to_value(emitted).expect("emitted continuation JSON");
    let arguments = json!({
        "scope_set_id": "scope-set.cli-replay",
        "scope_set_revision": 1,
        "scope_set_digest": format!("sha256:{}", "a".repeat(64)),
        "operation": {"kind": "query", "request": {}},
        "page": 1,
        "continuation": continuation
    });

    let parsed = parse_invocation_with_stdin(
        &def("multi_root_execute"),
        &["--args".to_owned(), arguments.to_string()],
        || panic!("inline JSON must not read stdin"),
    )
    .expect("the public CLI must replay the continuation emitted by page zero");

    assert_eq!(parsed.tool_args["continuation"], continuation);
}

#[test]
fn whole_payload_invocation_defers_schema_dependent_flags() {
    for args in [
        vec!["--query".to_owned(), "needle".to_owned()],
        vec!["--args".to_owned(), "{}".to_owned(), "--dry-run".to_owned()],
        vec!["--help".to_owned()],
    ] {
        assert!(
            parse_whole_payload_invocation_with_stdin(&args, || {
                panic!("schema-dependent invocations must not read stdin")
            })
            .expect("schema-dependent invocation detection")
            .is_none(),
            "{args:?} must retain the schema-driven path"
        );
    }

    for args in [
        vec!["--args".to_owned(), "-".to_owned(), "--help".to_owned()],
        vec!["--args".to_owned(), "-".to_owned(), "--dry-run".to_owned()],
        vec!["--dry-run".to_owned(), "--args".to_owned(), "-".to_owned()],
    ] {
        let mut stdin_reads = 0;
        assert!(
            parse_whole_payload_invocation_with_stdin(&args, || {
                stdin_reads += 1;
                Ok("{}".to_owned())
            })
            .expect("schema-dependent invocation detection")
            .is_none()
        );
        assert_eq!(
            stdin_reads, 0,
            "fast-path detection must leave stdin for the schema-driven parser: {args:?}"
        );
    }
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
fn args_escape_hatch_reads_bare_path() {
    // `--args` is a whole-payload arg, so a bare file path works without the
    // `@` sigil used by per-key file values.
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
fn array_value_collected_via_repetition() {
    let d = def("affected");
    let parsed = parse_invocation(
        &d,
        &[
            "--files".to_string(),
            "src/a.rs".to_string(),
            "--files".to_string(),
            "src/b.rs".to_string(),
        ],
    )
    .unwrap();
    // After parse, the second occurrence wraps into an array. finalize is
    // only called via the run path; here we just observe the merged shape.
    let files = &parsed.tool_args["files"];
    assert!(files.is_array(), "expected array, got {files}");
    let arr = files.as_array().unwrap();
    assert_eq!(arr.len(), 2);
}

#[test]
fn finalize_arrays_splits_csv() {
    let d = def("affected");
    let mut map = Map::new();
    map.insert("files".to_string(), json!("src/a.rs,src/b.rs,src/c.rs"));
    finalize_arrays(&d, &mut map);
    let arr = map["files"].as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], json!("src/a.rs"));
    assert_eq!(arr[2], json!("src/c.rs"));
}

#[test]
fn explicit_project_lcm_dispatch_allows_first_touch_init() {
    // An explicit absolute --project must pass through untouched; a bare
    // `/tmp/project` is drive-relative on Windows and would be re-rooted on
    // the current drive, which is not the property under test.
    let project = if cfg!(windows) {
        r"C:\tmp\project"
    } else {
        "/tmp/project"
    };
    let dispatch =
        DaemonToolDispatch::project_scoped(Some(project.to_string()), "tracedecay_lcm_status");

    assert!(dispatch.allow_init);
    assert_eq!(dispatch.project_path, Some(PathBuf::from(project)));
}

#[test]
fn user_storage_scope_dispatch_never_invents_a_project_from_cwd() {
    let dispatch = DaemonToolDispatch::for_tool(
        None,
        "tracedecay_lcm_status",
        &json!({
            "provider": "hermes",
            "storage_scope": "user",
        }),
    );

    assert_eq!(dispatch.project_path, None);
    assert!(!dispatch.allow_init);
}

#[test]
fn profile_scoped_session_refresh_dispatch_is_projectless() {
    for tool_name in [
        "tracedecay_session_refresh_begin",
        "tracedecay_session_refresh_status",
        "tracedecay_session_refresh_cancel",
    ] {
        let dispatch = DaemonToolDispatch::for_tool(
            Some("/explicit/project".to_owned()),
            tool_name,
            &json!({ "scope": { "kind": "profile", "profile_id": "profile.refresh" } }),
        );
        assert_eq!(dispatch.project_path, None, "{tool_name}");
        assert!(!dispatch.allow_init, "{tool_name}");

        let project_scoped = DaemonToolDispatch::for_tool(
            Some("/explicit/project".to_owned()),
            tool_name,
            &json!({ "scope": { "kind": "project", "project": { "id": "project.refresh" } } }),
        );
        assert_eq!(
            project_scoped.project_path,
            Some(tracedecay_configuration::resolve_path(Some(
                "/explicit/project".to_owned()
            ))),
            "{tool_name}"
        );
    }
    // A non-refresh tool with an object `scope` is not a profile request: it
    // keeps its explicit project.
    let dispatch = DaemonToolDispatch::for_tool(
        Some("/explicit/project".to_owned()),
        "tracedecay_message_search",
        &json!({ "scope": { "kind": "profile" } }),
    );
    assert_eq!(
        dispatch.project_path,
        Some(tracedecay_configuration::resolve_path(Some(
            "/explicit/project".to_owned()
        )))
    );
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
fn invalid_enum_errors_with_allowed_values() {
    let d = def("gini");
    let err = parse_invocation(&d, &["--metric".to_string(), "bogus".to_string()]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("`bogus` is not one of:"), "got: {msg}");
    assert!(msg.contains("complexity"), "got: {msg}");
    assert!(msg.contains("fan_in"), "got: {msg}");
}

/// The fact category vocabulary reaches the schema through a `$defs`
/// reference inside a nullable `anyOf`; client-side validation must resolve
/// it and reject an unknown value with the admitted list.
#[test]
fn invalid_ref_enum_errors_with_allowed_values() {
    let d = def("fact_store_add");
    let err = parse_invocation(
        &d,
        &[
            "--content".to_string(),
            "categorized fact".to_string(),
            "--category".to_string(),
            "pitfall".to_string(),
        ],
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("`pitfall` is not one of:"), "got: {msg}");
    for admitted in [
        "general",
        "user_pref",
        "project",
        "tool",
        "decision",
        "code_area",
    ] {
        assert!(msg.contains(admitted), "missing `{admitted}` in: {msg}");
    }
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
    let d = def("lcm_expand_query");
    let parsed = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"provider":"hermes","session_id":"s1","prompt":"what changed?","query":null}"#
                .to_string(),
        ],
    )
    .unwrap();
    assert!(parsed.tool_args["query"].is_null());
}

#[test]
fn dispatch_routing_keys_bypass_unknown_key_gate() {
    // LCM response handles can target a separate live project, and Hermes
    // may forward cwd; these must keep flowing through the gate.
    let d = def("fact_store_list");
    let parsed = parse_invocation(
        &d,
        &[
            "--args".to_string(),
            r#"{"response_handle_project_root":"/tmp/r","cwd":"/tmp"}"#.to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        parsed.tool_args["response_handle_project_root"],
        json!("/tmp/r")
    );
    assert_eq!(parsed.tool_args["cwd"], json!("/tmp"));
}

#[test]
fn removed_storage_routing_keys_fail_validation() {
    let d = def("fact_store_list");
    for removed in ["storage_scope", "hermes_home"] {
        let payload = format!(r#"{{"{removed}":"removed"}}"#);
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
fn fact_feedback_bare_helpful_flag_does_not_swallow_note_flag() {
    let d = def("fact_feedback");
    let parsed = parse_invocation(
        &d,
        &[
            "--fact-id".to_string(),
            "5".to_string(),
            "--action".to_string(),
            "helpful".to_string(),
            "--reason".to_string(),
            "great context".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        parsed.tool_args,
        json!({ "fact_id": "5", "action": "helpful", "reason": "great context" })
    );
}

#[test]
fn bare_boolean_flag_before_next_flag_does_not_swallow_it() {
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
fn join_content_text_routes_the_daemon_metrics_footer_to_stderr() {
    // `--format json` payloads are parsed from stdout as one document; the
    // daemon appends its token accounting as a separate block, which must not
    // trail the payload (run 34296614024: "Extra data: line 4 column 1").
    let value = json!({
        "content": [
            { "type": "text", "text": r#"{"code":[],"coverage":{"exact":"complete"}}"# },
            { "type": "text", "text": "\ntracedecay_metrics: before=151600 after=3721" }
        ]
    });
    assert_eq!(
        join_content_text(&value),
        r#"{"code":[],"coverage":{"exact":"complete"}}"#
    );
    assert_eq!(
        token_accounting_footers(&value),
        vec!["tracedecay_metrics: before=151600 after=3721".to_owned()]
    );
    assert!(token_accounting_footers(&json!({ "content": [] })).is_empty());
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

/// `main` maps `Ok` to exit 0 and any `Err` to a failing `ExitCode`, so the
/// outcome these assertions inspect *is* the process exit status.
#[test]
fn successful_tool_result_exits_zero() {
    let markdown = json!({
        "content": [{ "type": "text", "text": "**success:** true\n**files:** []" }]
    });
    let json_success = json!({
        "content": [{
            "type": "text",
            "text": "{\"success\":true,\"files\":[]}"
        }],
        "isError": false
    });
    assert!(
        tool_result_process_outcome(&markdown, "tracedecay_str_replace").is_ok(),
        "a successful markdown tool result must keep exit 0"
    );
    assert!(
        tool_result_process_outcome(&json_success, "tracedecay_str_replace").is_ok(),
        "an explicit isError:false JSON result must keep exit 0"
    );
    assert_eq!(
        rendered_tool_output(&markdown, false),
        "**success:** true\n**files:** []"
    );
    let json_stdout = rendered_tool_output(&json_success, true);
    // Pretty serialization escapes the nested `content[0].text` JSON string.
    // Parse the complete envelope back and compare it structurally instead of
    // searching for an unescaped substring that valid output cannot contain.
    let reparsed: Value = serde_json::from_str(&json_stdout)
        .unwrap_or_else(|error| panic!("JSON stdout must itself be valid JSON: {error}"));
    assert_eq!(
        reparsed, json_success,
        "JSON stdout must keep the exact daemon payload: {json_stdout}"
    );
    assert_eq!(
        reparsed["isError"],
        json!(false),
        "JSON stdout must keep the daemon isError flag: {json_stdout}"
    );
}

#[test]
fn application_error_tool_result_exits_nonzero() {
    // The exact envelope `tracedecay tool str_replace` returns for an
    // `old_str` that is not in the file: the payload is a normal result and
    // the daemon marks the outcome `isError`.
    let failed = json!({
        "content": [{
            "type": "text",
            "text": "{\"success\":false,\"message\":\"old_str not found in README.md\"}"
        }],
        "isError": true
    });
    let stdout_json = rendered_tool_output(&failed, true);
    let stdout_text = rendered_tool_output(&failed, false);
    assert!(
        stdout_json.contains("old_str not found in README.md"),
        "{stdout_json}"
    );
    assert!(stdout_json.contains("\"isError\": true"), "{stdout_json}");
    assert_eq!(
        stdout_text,
        "{\"success\":false,\"message\":\"old_str not found in README.md\"}"
    );

    let error = tool_result_process_outcome(&failed, "tracedecay_str_replace")
        .expect_err("an application failure must fail the CLI process");
    let message = error.to_string();
    assert!(message.contains("tracedecay_str_replace"), "{message}");
    assert!(message.contains("application failure"), "{message}");
    assert!(
        !message.contains("old_str not found in README.md"),
        "stderr must not scrape a payload that is already on stdout: {message}"
    );
}

/// A truthful *degraded* answer is not a failure. Retrieval lanes that report
/// themselves `unavailable` inside an otherwise successful payload, partial
/// coverage, and warming generations all keep exit 0 — only an outcome the
/// daemon itself marked `isError` changes the status.
#[test]
fn typed_unavailable_coverage_inside_a_successful_result_stays_exit_zero() {
    let markdown = json!({
        "content": [{
            "type": "text",
            "text": "### Coverage\nPartial recall — some retrieval lanes did not answer:\n\
                     - exact: unavailable (generation_rebuilding)\n\
                     - semantic: unavailable (generation_rebuilding)"
        }]
    });
    let warming_json = json!({
        "content": [{
            "type": "text",
            "text": "{\"coverage\":\"partial\",\"exact\":\"unavailable\",\"reason\":\"generation_rebuilding\"}"
        }]
    });
    let nested_is_error = json!({
        "content": [{
            "type": "text",
            "text": "{\"isError\":true,\"message\":\"nested marker is not the daemon flag\"}"
        }]
    });
    assert!(
        tool_result_process_outcome(&markdown, "tracedecay_context").is_ok(),
        "a degraded but successfully reported answer must not fail the process"
    );
    assert!(
        tool_result_process_outcome(&warming_json, "tracedecay_context").is_ok(),
        "a JSON warming/partial payload without top-level isError must keep exit 0"
    );
    assert!(
        tool_result_process_outcome(&nested_is_error, "tracedecay_context").is_ok(),
        "only the daemon's top-level isError flag may change the process status"
    );
    assert!(
        rendered_tool_output(&markdown, false).contains("unavailable (generation_rebuilding)"),
        "markdown stdout must keep the typed degraded payload"
    );
    assert!(
        rendered_tool_output(&warming_json, true).contains("generation_rebuilding"),
        "JSON stdout must keep the typed warming payload"
    );
}

#[test]
fn application_error_without_a_json_message_still_exits_nonzero() {
    // Markdown-rendering handlers have no JSON `message` field; the status
    // still changes from top-level `isError`, and the rendered payload stays
    // on stdout instead of being scraped into stderr.
    let failed = json!({
        "content": [{ "type": "text", "text": "**success:** false\n**outcome:** failed" }],
        "isError": true
    });
    assert_eq!(
        rendered_tool_output(&failed, false),
        "**success:** false\n**outcome:** failed"
    );
    assert!(
        rendered_tool_output(&failed, true).contains("**success:** false"),
        "JSON stdout must keep the exact markdown daemon payload"
    );

    let error = tool_result_process_outcome(&failed, "tracedecay_run_affected_tests")
        .expect_err("an application failure must fail the CLI process");
    let message = error.to_string();
    assert!(
        message.contains("tracedecay_run_affected_tests"),
        "{message}"
    );
    assert!(message.contains("application failure"), "{message}");
    assert!(
        !message.contains("**success:** false"),
        "stderr must not scrape a payload that is already on stdout: {message}"
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
        )
        .expect("construct canonical configuration conflict problem")),
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
        // `health_read` takes no parameters at all, so the documented
        // invocation is the empty object on every transport.
        ("tracedecay_health_read", json!({})),
        ("tracedecay_git_status", json!({})),
        ("tracedecay_git_diff", json!({})),
        ("tracedecay_git_history", json!({"count": 3})),
        (
            "tracedecay_source_outline",
            json!({"file": "src/update_cmd.rs"}),
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

/// One request the way each transport carries it. CLI and MCP share the
/// argument object, page controls included; HTTP carries the page in its
/// query string, so its body omits exactly the fields the query supplies.
struct TransportEquivalentRequest {
    tool_name: &'static str,
    arguments: Value,
    http_body: Value,
    http_page: PageRequest,
}

fn http_page(page_size: u32, cursor: Option<&str>) -> PageRequest {
    PageRequest::new(
        page_size,
        cursor.map(|cursor| OpaqueCursor::new(cursor).expect("cursor")),
    )
    .expect("page")
}

fn transport_equivalent_requests() -> Vec<TransportEquivalentRequest> {
    let mut requests: Vec<TransportEquivalentRequest> = documented_json_invocations()
        .into_iter()
        .map(|(tool_name, arguments)| TransportEquivalentRequest {
            tool_name,
            http_body: arguments.clone(),
            arguments,
            http_page: http_page(10, None),
        })
        .collect();
    // Diagnostics retains its shipped flat CLI/MCP shape, while HTTP accepts
    // the canonical request and carries page controls in the query.
    requests.push(TransportEquivalentRequest {
        tool_name: "tracedecay_diagnostics",
        arguments: json!({
            "scope": "file",
            "path": "src/update_cmd.rs",
            "maximum_diagnostics": 25,
            "cursor": "diagnostics-page-2",
        }),
        http_body: json!({"scope": {"file": "src/update_cmd.rs"}}),
        http_page: http_page(25, Some("diagnostics-page-2")),
    });
    // Callable-code continuations ride in `meta.cursor`; the HTTP query cursor
    // must be the same continuation, not a second channel.
    let symbol_search = json!({
        "query": "ApplicationSurfaceOperation",
        "scope": {"path_prefix": "src"},
        "lazy_index_ignored_dependencies": false,
        "meta": {"projection": "evidence", "order": "source_position"},
    });
    let mut symbol_search_arguments = symbol_search.clone();
    symbol_search_arguments["meta"]["cursor"] = json!("symbols-page-2");
    requests.push(TransportEquivalentRequest {
        tool_name: "tracedecay_code_symbol_search",
        arguments: symbol_search_arguments,
        http_body: symbol_search,
        http_page: http_page(10, Some("symbols-page-2")),
    });
    requests
}

/// The canonical session refresh request in both owner scopes. The daemon
/// decodes exactly this shape on every transport; the CLI and MCP arguments
/// are the HTTP body plus the presentation-only `format`.
fn session_refresh_equivalent_requests() -> Vec<TransportEquivalentRequest> {
    let session = |store_id: &str, root_id: &str| json!({ "id": "session.refresh", "store_id": store_id, "root_id": root_id });
    let target = json!({
        "temporal_mode": { "kind": "current" },
        "grain": "logical_message",
        "frontier": { "observed_through": 9, "committed_through": 4 }
    });
    let profile_scope = json!({ "kind": "profile", "profile_id": "profile.refresh" });
    let project_scope = json!({
        "kind": "project",
        "project": {
            "id": "project.refresh",
            "profile_id": "profile.refresh",
            "repository_id": "repository.refresh",
            "worktree_id": "/worktree/refresh",
            "branch_id": "branch.refresh"
        }
    });
    let mut requests = Vec::new();
    for (tool_name, scope, session, handle) in [
        (
            "tracedecay_session_refresh_begin",
            profile_scope.clone(),
            session("store.profile.refresh", "root.profile.refresh"),
            Value::Null,
        ),
        (
            "tracedecay_session_refresh_status",
            profile_scope.clone(),
            session("store.profile.refresh", "root.profile.refresh"),
            json!("srh_profile"),
        ),
        (
            "tracedecay_session_refresh_cancel",
            profile_scope,
            session("store.profile.refresh", "root.profile.refresh"),
            json!("srh_profile"),
        ),
        (
            "tracedecay_session_refresh_begin",
            project_scope.clone(),
            session("store.project.refresh", "branch.refresh"),
            Value::Null,
        ),
        (
            "tracedecay_session_refresh_status",
            project_scope,
            session("store.project.refresh", "branch.refresh"),
            json!("srh_project"),
        ),
    ] {
        let body = json!({
            "scope": scope,
            "session": session,
            "source": { "scope": "codex" },
            "target": target,
            "handle": handle,
        });
        requests.push(TransportEquivalentRequest {
            tool_name,
            arguments: body.clone(),
            http_body: body,
            http_page: http_page(10, None),
        });
    }
    requests
}

/// The retained half of the transport equivalence: CLI and MCP normalize
/// through the shared argument adapter and, like the HTTP body, land on
/// `decode_request` for the exact operation. All three must decode the same
/// canonical `RetainedSurfaceRequestV1` and bind one request/result contract.
fn assert_retained_transports_decode_one_canonical_request(
    equivalent: &TransportEquivalentRequest,
    operation: tracedecay_contracts::RetainedSurfaceOperation,
) {
    let tool_name = equivalent.tool_name;
    let (cli_body, cli_format) = cli_surface_invocation(
        tool_name,
        with_format(equivalent.arguments.clone(), "json"),
        false,
    )
    .unwrap_or_else(|error| panic!("{tool_name} CLI normalization failed: {error}"));
    let mcp = adapt_application_tool_request(
        tool_name,
        with_format(equivalent.arguments.clone(), "json"),
    )
    .unwrap_or_else(|error| panic!("{tool_name} MCP normalization failed: {error}"));
    assert_eq!(cli_format, RequestedOutputFormat::Json, "{tool_name}");
    assert_eq!(
        mcp.requested_format,
        RequestedOutputFormat::Json,
        "{tool_name}"
    );

    let decoded = [
        (BindingSurface::Cli, cli_body),
        (BindingSurface::Mcp, mcp.request),
        (BindingSurface::Http, equivalent.http_body.clone()),
    ]
    .map(|(surface, body)| {
        let request = decode_retained_request(operation, body)
            .unwrap_or_else(|error| panic!("{tool_name} {surface:?} request: {error}"));
        assert_eq!(request.operation(), operation, "{tool_name} {surface:?}");
        let binding = resolve_catalog_tool_binding(surface, tool_name)
            .unwrap_or_else(|error| panic!("{tool_name} {surface:?} binding: {error}"))
            .unwrap_or_else(|| panic!("{tool_name} has no {surface:?} catalog binding"));
        (
            surface,
            serde_json::to_value(&request).expect("decoded request"),
            binding.request_schema,
            binding.result_schema,
        )
    });
    let (_, cli_request, cli_request_schema, cli_result_schema) = &decoded[0];
    let expected_scope = equivalent.arguments["scope"]["kind"]
        .as_str()
        .expect("fixture scope kind");
    assert_eq!(
        cli_request["request"]["scope"]["kind"], expected_scope,
        "{tool_name}: the canonical request must carry the owner scope unchanged"
    );
    for (surface, request, request_schema, result_schema) in &decoded[1..] {
        assert_eq!(
            request, cli_request,
            "{tool_name}: {surface:?} decoded a different canonical request than the CLI"
        );
        assert_eq!(
            request_schema, cli_request_schema,
            "{tool_name}: {surface:?} bound a different request contract than the CLI"
        );
        assert_eq!(
            result_schema, cli_result_schema,
            "{tool_name}: {surface:?} bound a different result contract than the CLI"
        );
    }
}

/// The same request, decoded by every transport, must reach the same reviewed
/// request and result contract as the same canonical
/// [`ApplicationSurfaceRequest`]. CLI and MCP decode through the shared
/// argument adapter; HTTP decodes through its own body-plus-query projection.
/// A transport that grew its own request shape would fail here before any
/// daemon was involved. Retained operations (the session refresh lifecycle in
/// both owner scopes) take the retained decoder for the same proof.
#[test]
fn cli_mcp_and_http_decode_one_canonical_request() {
    for equivalent in session_refresh_equivalent_requests() {
        let operation =
            tracedecay_contracts::RetainedSurfaceOperation::from_tool_name(equivalent.tool_name)
                .unwrap_or_else(|| panic!("{} is a retained operation", equivalent.tool_name));
        assert_retained_transports_decode_one_canonical_request(&equivalent, operation);
    }
    for equivalent in transport_equivalent_requests() {
        let tool_name = equivalent.tool_name;
        let operation = ApplicationSurfaceOperation::from_tool_name(tool_name)
            .unwrap_or_else(|| panic!("{tool_name} is an application surface operation"));

        let (cli_body, cli_format) = cli_surface_invocation(
            tool_name,
            with_format(equivalent.arguments.clone(), "json"),
            false,
        )
        .unwrap_or_else(|error| panic!("{tool_name} CLI normalization failed: {error}"));
        let mcp = adapt_application_tool_request(
            tool_name,
            with_format(equivalent.arguments.clone(), "json"),
        )
        .unwrap_or_else(|error| panic!("{tool_name} MCP normalization failed: {error}"));
        assert_eq!(cli_format, RequestedOutputFormat::Json, "{tool_name}");
        assert_eq!(
            mcp.requested_format,
            RequestedOutputFormat::Json,
            "{tool_name}"
        );

        let decoded = [
            (
                BindingSurface::Cli,
                parse_application_surface_request(operation, cli_body)
                    .unwrap_or_else(|error| panic!("{tool_name} CLI request: {error}")),
            ),
            (
                BindingSurface::Mcp,
                parse_application_surface_request(operation, mcp.request)
                    .unwrap_or_else(|error| panic!("{tool_name} MCP request: {error}")),
            ),
            (
                BindingSurface::Http,
                parse_http_application_surface_request(
                    operation,
                    equivalent.http_body.clone(),
                    &equivalent.http_page,
                )
                .unwrap_or_else(|error| panic!("{tool_name} HTTP request: {error}")),
            ),
        ];

        let mut contracts = Vec::new();
        for (surface, request) in decoded {
            let request_value = serde_json::to_value(&request).expect("decoded request");
            let dispatched = resolve_application_surface_dispatch_with_controls(
                surface,
                operation,
                RequestId::new(format!(
                    "request.{}.{}",
                    format!("{surface:?}").to_ascii_lowercase(),
                    operation.as_str()
                ))
                .expect("request id"),
                request,
                equivalent.http_page.clone(),
                None,
                CancellationSignal::active(format!("cancel.{}", operation.as_str()))
                    .expect("cancellation"),
                RequestedOutputFormat::Json,
            )
            .unwrap_or_else(|error| panic!("{tool_name} {surface:?} dispatch: {error}"));
            assert_eq!(
                serde_json::to_value(&dispatched.invocation.invocation.request)
                    .expect("dispatched request"),
                request_value,
                "{tool_name} {surface:?} dispatch must carry the decoded request unchanged"
            );
            contracts.push((
                surface,
                request_value,
                dispatched.invocation.request_schema.clone(),
                dispatched.invocation.result_schema.clone(),
            ));
        }
        let (_, cli_request, cli_request_schema, cli_result_schema) = &contracts[0];
        for (surface, request, request_schema, result_schema) in &contracts[1..] {
            assert_eq!(
                request, cli_request,
                "{tool_name}: {surface:?} decoded a different canonical request than the CLI"
            );
            assert_eq!(
                request_schema, cli_request_schema,
                "{tool_name}: {surface:?} bound a different request contract than the CLI"
            );
            assert_eq!(
                result_schema, cli_result_schema,
                "{tool_name}: {surface:?} bound a different result contract than the CLI"
            );
        }
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
fn application_surface_rejects_invalid_output_formats() {
    for format in [json!("yaml"), json!(42), Value::Null] {
        let error = cli_surface_invocation(
            "tracedecay_storage_status",
            json!({"format": format}),
            false,
        )
        .expect_err("format outside the schema must fail");
        assert!(matches!(
            error,
            ApplicationSurfaceAdapterError::InvalidSurfaceRequest
        ));
    }
}
