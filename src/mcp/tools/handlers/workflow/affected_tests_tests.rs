use super::*;

#[allow(dead_code)]
fn assert_begin_test_run_future_is_send(cg: &TraceDecay, deadline: Deadline) {
    fn assert_send<T: Send>(_: T) {}
    assert_send(begin_test_run(cg, &[], deadline, None));
}

#[tokio::test]
async fn directly_changed_test_file_is_dispatched_and_retains_terminal_receipt() {
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
        "#[test]\nfn edited_only_test() {\n    assert_eq!(2, 2);\n}\n",
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
            assert_eq!(tests, ["edited_only_test"]);
            Ok(TestRunOutput {
                exit_code: Some(0),
                stdout: "test edited_only_test ... ok\n".to_string(),
                stderr: String::new(),
                output_bytes: 31,
            })
        },
    )
    .await
    .unwrap();

    let text = result.value["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["dispatched_tests"], json!(["edited_only_test"]));
    assert_eq!(output["results"][0]["test"], "edited_only_test");
    assert_eq!(output["passed"], 1);
    assert_eq!(
        output["terminal"]["receipt"]["termination"], "completed",
        "the direct producer result must expose the terminal receipt it retained"
    );
    assert_eq!(
        output["terminal"]["result_tool"], "tracedecay_test_results",
        "the producer must direct consumers to the canonical retained-result reader"
    );
    assert_eq!(
        output["terminal"]["receipt"]["budget"]["bytes_consumed"], 31,
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
fn cargo_test_args_put_multiple_filters_after_libtest_separator() {
    let args = cargo_test_args("debug", &["alpha".to_string(), "beta".to_string()]);

    assert_eq!(args, ["test", "--no-fail-fast", "--", "alpha", "beta"]);
}

#[test]
fn cargo_test_args_keep_release_before_libtest_separator() {
    let args = cargo_test_args("release", &["alpha".to_string(), "beta".to_string()]);

    assert_eq!(
        args,
        ["test", "--no-fail-fast", "--release", "--", "alpha", "beta"]
    );
}

#[test]
fn tail_handles_short_input() {
    assert_eq!(tail("hello", 100), "hello");
    assert_eq!(tail("0123456789", 4), "6789");
}
