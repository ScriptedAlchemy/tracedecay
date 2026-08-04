use super::*;

#[allow(dead_code)]
fn assert_begin_test_run_future_is_send(cg: &TraceDecay, deadline: Deadline) {
    fn assert_send<T: Send>(_: T) {}
    assert_send(begin_test_run(cg, &[], deadline, None));
}

#[tokio::test]
async fn directly_changed_test_file_dispatches_each_full_test_identity() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join("tests")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn util() -> u32 { 1 }\n").unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("tests/edited_only.rs"),
        "mod nested {\n    #[test]\n    fn first() {}\n\n    #[test]\n    fn second() {}\n}\n",
    )
    .unwrap();

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project,
        "project.mcp-affected-tests",
    )
    .await
    .unwrap();
    cg.index_all().await.unwrap();
    {
        let database = cg.dashboard_database_guard();
        database
            .execute_write_batch(
                "seed managed test-run diagnostics schema",
                crate::diagnostics_store::SCHEMA,
            )
            .await
            .unwrap();
    }
    let expected_root = project.to_path_buf();
    let result = handle_run_affected_tests_with_runner(
        &cg,
        json!({
            "changed_paths": ["tests/edited_only.rs"],
            "timeout_secs": 60,
            "max_tests": 5,
            "format": "json"
        }),
        None,
        None,
        move |root, profile, tests, timeout_duration, _control| async move {
            assert_eq!(root, expected_root);
            assert_eq!(profile, "debug");
            assert_eq!(timeout_duration, Duration::from_mins(1));
            assert_eq!(tests, ["nested::first", "nested::second"]);
            Ok(TestRunOutput {
                exit_code: Some(0),
                stdout: "test nested::first ... ok\ntest nested::second ... ok\n".to_string(),
                stderr: String::new(),
                output_bytes: 62,
            })
        },
    )
    .await
    .unwrap();

    let text = result.value["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        output["dispatched_tests"],
        json!(["nested::first", "nested::second"])
    );
    assert_eq!(output["results"][0]["test"], "nested::first");
    assert_eq!(output["results"][1]["test"], "nested::second");
    assert_eq!(output["passed"], 2);
    assert_eq!(
        output["terminal"]["receipt"]["termination"], "completed",
        "the direct producer result must expose the terminal receipt it retained"
    );
    assert_eq!(
        output["terminal"]["result_tool"], "tracedecay_test_results",
        "the producer must direct consumers to the canonical retained-result reader"
    );
    assert_eq!(
        output["terminal"]["receipt"]["budget"]["bytes_consumed"], 62,
        "the terminal receipt must account for the bounded subprocess output"
    );
    assert!(
        output["terminal"]["operation_id"]
            .as_str()
            .is_some_and(|operation_id| !operation_id.is_empty()),
        "the retained result needs an observable operation identity"
    );

    cg.checkpoint().await.unwrap();
    cg.close();
}

#[tokio::test]
async fn non_string_changed_paths_are_rejected_before_test_selection() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let dir = tempfile::TempDir::new().unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        dir.path(),
        "project.mcp-affected-tests-invalid-input",
    )
    .await
    .unwrap();

    let result = handle_run_affected_tests_with_runner(
        &cg,
        json!({
            "changed_paths": ["tests/valid.rs", 7],
            "format": "json"
        }),
        None,
        None,
        |_root, _profile, _tests, _timeout_duration, _control| async move {
            panic!("invalid producer input must never reach the test runner")
        },
    )
    .await
    .unwrap();

    let text = result.value["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"], "invalid_request");
    assert_eq!(output["error"]["operation"], "changed_paths");
    assert!(
        output["note"].is_null(),
        "malformed producer input must not be relabelled as an empty change set"
    );

    cg.close();
}

#[test]
fn zero_max_tests_is_rejected_before_any_test_runner_can_start() {
    let result = RunAffectedArgs::parse(&json!({"max_tests": 0, "format": "json"}))
        .expect_err("zero max tests must not become an unfiltered cargo invocation");
    let output = tool_result_body(result);

    assert_eq!(output["error"]["kind"], "invalid_request");
    assert_eq!(output["error"]["operation"], "max_tests");
}

#[test]
fn timeout_above_the_managed_test_limit_is_rejected() {
    let result = RunAffectedArgs::parse(&json!({
        "timeout_secs": MAX_TEST_TIMEOUT_SECS + 1,
        "format": "json"
    }))
    .expect_err("a managed test run cannot select an unbounded deadline");
    let output = tool_result_body(result);

    assert_eq!(output["error"]["kind"], "invalid_request");
    assert_eq!(output["error"]["operation"], "timeout_secs");
}

#[test]
fn zero_timeout_is_rejected() {
    let result = RunAffectedArgs::parse(&json!({"timeout_secs": 0, "format": "json"}))
        .expect_err("zero timeout must not disable the managed test deadline");
    let output = tool_result_body(result);

    assert_eq!(output["error"]["kind"], "invalid_request");
    assert_eq!(output["error"]["operation"], "timeout_secs");
}

#[tokio::test]
async fn timed_out_test_runner_returns_a_terminal_receipt() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join("tests")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn util() {}\n").unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"timed-runner-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("tests/edited.rs"),
        "#[test]\nfn timed_target() {}\n",
    )
    .unwrap();

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project,
        "project.mcp-affected-tests-timeout",
    )
    .await
    .unwrap();
    cg.index_all().await.unwrap();
    {
        let database = cg.dashboard_database_guard();
        database
            .execute_write_batch(
                "seed timed managed test-run diagnostics schema",
                crate::diagnostics_store::SCHEMA,
            )
            .await
            .unwrap();
    }

    let result = handle_run_affected_tests_with_runner(
        &cg,
        json!({"changed_paths": ["tests/edited.rs"], "format": "json"}),
        None,
        None,
        |_root, _profile, _tests, _timeout_duration, _control| async move {
            Err(TestRunFailure::Timeout { output_bytes: 17 })
        },
    )
    .await
    .unwrap();

    let text = result.value["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"], "cargo");
    assert_eq!(output["terminal"]["receipt"]["termination"], "timed_out");
    assert_eq!(
        output["terminal"]["receipt"]["budget"]["bytes_consumed"],
        17
    );

    cg.close();
}

#[tokio::test]
async fn vacuous_or_nonzero_test_output_is_a_failed_terminal() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join("tests")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn util() {}\n").unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"failed-runner-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("tests/edited.rs"),
        "#[test]\nfn selected_target() {}\n",
    )
    .unwrap();

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project,
        "project.mcp-affected-tests-failed-output",
    )
    .await
    .unwrap();
    cg.index_all().await.unwrap();
    {
        let database = cg.dashboard_database_guard();
        database
            .execute_write_batch(
                "seed failed managed test-run diagnostics schema",
                crate::diagnostics_store::SCHEMA,
            )
            .await
            .unwrap();
    }

    let vacuous = handle_run_affected_tests_with_runner(
        &cg,
        json!({"changed_paths": ["tests/edited.rs"], "format": "json"}),
        None,
        None,
        |_root, _profile, _tests, _timeout_duration, _control| async move {
            Ok(TestRunOutput {
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                output_bytes: 0,
            })
        },
    )
    .await
    .unwrap();
    assert_failed_terminal(tool_result_body(vacuous));

    let nonzero = handle_run_affected_tests_with_runner(
        &cg,
        json!({"changed_paths": ["tests/edited.rs"], "format": "json"}),
        None,
        None,
        |_root, _profile, _tests, _timeout_duration, _control| async move {
            Ok(TestRunOutput {
                exit_code: Some(1),
                stdout: String::new(),
                stderr: "test harness failed".to_owned(),
                output_bytes: 19,
            })
        },
    )
    .await
    .unwrap();
    assert_failed_terminal(tool_result_body(nonzero));

    cg.close();
}

#[test]
fn parses_libtest_pass_and_fail() {
    let stdout = "\
running 3 tests
test foo ... ok
test bar ... FAILED
test baz ... ignored
test result: FAILED. 1 passed; 1 failed; 1 ignored
";
    let results = parse_libtest_output(stdout);
    assert_eq!(results, vec![("foo".into(), true), ("bar".into(), false)]);
}

#[test]
fn cargo_test_args_use_one_exact_identity() {
    let args = cargo_test_args("debug", "nested::alpha");

    assert_eq!(
        args,
        ["test", "--no-fail-fast", "--", "--exact", "nested::alpha"]
    );
}

#[test]
fn cargo_test_args_keep_release_before_libtest_separator() {
    let args = cargo_test_args("release", "nested::alpha");

    assert_eq!(
        args,
        [
            "test",
            "--no-fail-fast",
            "--release",
            "--",
            "--exact",
            "nested::alpha"
        ]
    );
}

#[test]
fn tail_handles_short_input() {
    assert_eq!(tail("hello", 100), "hello");
    assert_eq!(tail("0123456789", 4), "6789");
}

fn tool_result_body(result: ToolResult) -> Value {
    let text = result.value["content"][0]["text"]
        .as_str()
        .expect("json tool result");
    serde_json::from_str(text).expect("tool body")
}

fn assert_failed_terminal(output: Value) {
    assert_eq!(output["error"]["kind"], "cargo");
    assert_eq!(output["terminal"]["receipt"]["termination"], "failed");
    assert!(output["note"].is_null());
}
