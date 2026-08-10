use super::*;

use std::fmt::Debug;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_application::CancellationSignal;
use tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore;
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkV1, ContentDigest,
    EdgeAuthorityV1, FileOccurrenceId, LanguageDescriptorRevision, LanguageId, PolicyRevisionId,
    RelationEdgeKindV1, SanitizedCodeFileV1, SanitizerRevision, SensitivityDecision,
    SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan, SymbolOccurrenceId,
};
use tracedecay_graph_db::NeverCancelled;

#[allow(dead_code)]
fn assert_begin_test_run_future_is_send(cg: &TraceDecay, deadline: Deadline) {
    fn assert_send<T: Send>(_: T) {}
    assert_send(begin_test_run(cg, &[], deadline, None));
}

#[derive(Clone, Copy)]
struct FixtureSymbol<'a> {
    path: &'a str,
    qualified_name: &'a str,
    annotated_test: bool,
}

fn fixture_id<T>(value: impl Into<String>) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.into()).expect("valid fixture identity")
}

fn fixture_digest<T>(kind: &str, value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    fixture_id(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn verified_graph(
    fixture_symbols: &[FixtureSymbol<'_>],
) -> crate::tracedecay::queries::graph::VerifiedGraphQuery {
    let generation = fixture_id::<CodeGenerationId>("generation.affected-tests.1");
    let mut files = Vec::new();
    let mut chunks = Vec::new();
    let mut symbols = Vec::new();
    let mut edges = Vec::new();
    let mut file_occurrences = HashMap::<&str, FileOccurrenceId>::new();

    for (ordinal, fixture) in fixture_symbols.iter().enumerate() {
        let file = match file_occurrences.get(fixture.path) {
            Some(occurrence) => occurrence.clone(),
            None => {
                let occurrence: FileOccurrenceId =
                    fixture_id(format!("file.{}", file_occurrences.len()));
                files.push(SanitizedCodeFileV1 {
                    file_occurrence_id: occurrence.clone(),
                    logical_path: fixture.path.to_owned(),
                    language: Some(LanguageId::new("rust").expect("valid fixture language")),
                    content_digest: fixture_digest("file", fixture.path),
                    disposition: SnapshotFileDispositionV1::Present,
                });
                file_occurrences.insert(fixture.path, occurrence.clone());
                occurrence
            }
        };
        let occurrence = fixture_id::<SymbolOccurrenceId>(format!("symbol.{ordinal}"));
        push_fixture_symbol(
            &generation,
            &file,
            fixture.path,
            &occurrence,
            fixture.qualified_name,
            "function",
            u32::try_from(chunks.len()).expect("fixture chunk ordinal"),
            &mut symbols,
            &mut chunks,
        );
        if fixture.annotated_test {
            let marker = fixture_id::<SymbolOccurrenceId>(format!("annotation.{ordinal}"));
            push_fixture_symbol(
                &generation,
                &file,
                fixture.path,
                &marker,
                "test",
                "annotation_usage",
                u32::try_from(chunks.len()).expect("fixture chunk ordinal"),
                &mut symbols,
                &mut chunks,
            );
            edges.push(CanonicalRelationEdgeV1 {
                from_occurrence: marker,
                to_occurrence: occurrence,
                kind: RelationEdgeKindV1::Annotates,
                authority: EdgeAuthorityV1::SyntaxExact,
                evidence_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 1,
                },
            });
        }
    }

    let symbols = GenerationSymbolIndexV1::new(generation.clone(), symbols)
        .expect("valid fixture symbol index");
    let cancellation = CancellationSignal::active("cancellation.affected-tests.fixture")
        .expect("valid fixture cancellation");
    let store = HermeticCodeGraphProjectionStore::memory(&cancellation)
        .expect("open hermetic fixture projection");
    store
        .publish_indexed_with_cancellation(
            &generation,
            &edges,
            &chunks,
            &files,
            &symbols,
            Arc::new(NeverCancelled),
        )
        .expect("publish indexed fixture generation");
    let store = store
        .verified_store(&generation)
        .expect("open verified fixture generation");
    let graph_cancellation =
        tracedecay_usecases::graph::application_graph_cancellation(&cancellation);
    let reader = store
        .interactive_reader_with_cancellation(&generation, Arc::clone(&graph_cancellation))
        .expect("open generation-pinned fixture reader");
    crate::tracedecay::queries::graph::VerifiedGraphQuery::from_reader(reader, graph_cancellation)
}

#[allow(clippy::too_many_arguments)]
fn push_fixture_symbol(
    generation: &CodeGenerationId,
    file: &FileOccurrenceId,
    path: &str,
    occurrence: &SymbolOccurrenceId,
    qualified_name: &str,
    kind: &str,
    ordinal: u32,
    symbols: &mut Vec<LineageSymbolRecordV1>,
    chunks: &mut Vec<CodeSearchChunkV1>,
) {
    symbols.push(LineageSymbolRecordV1 {
        occurrence: occurrence.clone(),
        identity: fixture_digest("symbol-identity", occurrence.as_str()),
        qualified_name: qualified_name.to_owned(),
        simple_name: qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(qualified_name)
            .to_owned(),
        kind: kind.to_owned(),
        visibility: "private".to_owned(),
        branches: 0,
        loops: 0,
        max_nesting: 0,
        line_span: 1,
        start_line: ordinal,
        signature: None,
        skip_test_coverage: false,
        file_identity: fixture_digest("file-identity", path),
        content_digest: fixture_digest("symbol-content", occurrence.as_str()),
    });
    chunks.push(CodeSearchChunkV1 {
        id: fixture_id(format!("chunk.{ordinal}")),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation.clone(),
            file_occurrence_id: file.clone(),
            symbol_occurrence_id: Some(occurrence.clone()),
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: u64::from(ordinal),
                end_byte: u64::from(ordinal) + 1,
            },
            grain: CodeSearchChunkGrainV1::SymbolBody,
            ordinal,
        },
        content_digest: fixture_digest::<ContentDigest>("chunk", occurrence.as_str()),
        language_descriptor_revision: fixture_id::<LanguageDescriptorRevision>(
            "language.rust.fixture.v1",
        ),
        chunker_revision: fixture_id::<ChunkerRevision>("chunker.fixture.v1"),
        sanitizer_revision: fixture_id::<SanitizerRevision>("sanitizer.fixture.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: fixture_id::<PolicyRevisionId>("policy.fixture.v1"),
        },
        exact_terms: Vec::new(),
        subtokens: Vec::new(),
        sanitized_text: BoundedSanitizedText::new("fixture symbol").expect("bounded fixture text"),
    });
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
    let graph = verified_graph(&[
        FixtureSymbol {
            path: "tests/edited_only.rs",
            qualified_name: "nested::first",
            annotated_test: false,
        },
        FixtureSymbol {
            path: "tests/edited_only.rs",
            qualified_name: "nested::second",
            annotated_test: false,
        },
    ]);
    let expected_root = project.to_path_buf();
    let result = handle_run_affected_tests_with_runner(
        &cg,
        &graph,
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
            assert_eq!(profile, TestProfile::Debug);
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
    let receipt = &output["terminal"]["receipt"];
    let started_at = receipt["started_at"].as_i64().unwrap();
    let ended_at = receipt["ended_at"].as_i64().unwrap();
    assert!(
        started_at > 1_577_836_800_000_000,
        "the receipt must use the canonical application wall clock"
    );
    assert!(ended_at >= started_at);
    assert!(
        output["terminal"]["operation_id"]
            .as_str()
            .is_some_and(|operation_id| !operation_id.is_empty()),
        "the retained result needs an observable operation identity"
    );

    cg.checkpoint().await.unwrap();
    cg.close();
}

/// A test nested inside a source module is only reachable through the module
/// chain its own file contributes to the crate. Dispatching the in-file chain
/// alone (`tests::successful_login_creates_session`) makes Cargo filter every
/// test out and still exit `0`, so the managed run executes nothing while
/// reporting success.
#[tokio::test]
async fn nested_source_module_dispatches_the_crate_relative_test_identity() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("src/auth")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub mod auth;\n").unwrap();
    std::fs::write(project.join("src/auth/mod.rs"), "pub mod login;\n").unwrap();
    std::fs::write(
        project.join("src/auth/login.rs"),
        concat!(
            "pub fn login() -> bool {\n    true\n}\n\n",
            "#[cfg(test)]\nmod tests {\n    #[test]\n",
            "    fn successful_login_creates_session() {\n",
            "        assert!(super::login());\n    }\n}\n",
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project,
        "project.mcp-nested-module-tests",
    )
    .await
    .unwrap();
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

    let graph = verified_graph(&[FixtureSymbol {
        path: "src/auth/login.rs",
        qualified_name: "tests::successful_login_creates_session",
        annotated_test: true,
    }]);
    const EXPECTED: &str = "auth::login::tests::successful_login_creates_session";
    let result = handle_run_affected_tests_with_runner(
        &cg,
        &graph,
        json!({
            "changed_paths": ["src/auth/login.rs"],
            "timeout_secs": 60,
            "max_tests": 5,
            "format": "json"
        }),
        None,
        None,
        move |_root, _profile, tests, _timeout_duration, _control| async move {
            assert_eq!(
                tests,
                [EXPECTED],
                "the dispatched filter must carry the module chain of the test's own file"
            );
            Ok(TestRunOutput {
                exit_code: Some(0),
                stdout: format!("test {EXPECTED} ... ok\n"),
                stderr: String::new(),
                output_bytes: 64,
            })
        },
    )
    .await
    .unwrap();

    let text = result.value["content"][0]["text"].as_str().unwrap();
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["dispatched_tests"], json!([EXPECTED]));
    assert_eq!(output["results"][0]["test"], EXPECTED);
    assert_eq!(output["passed"], 1);
    assert_eq!(
        output["terminal"]["receipt"]["termination"], "completed",
        "an executed nested-module test must reach a completed terminal"
    );
    assert!(
        output["results"][0]["covers_source_ids"]
            .as_array()
            .is_some_and(|covered| !covered.is_empty()),
        "the crate-relative identity must still resolve back to its covered sources"
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

    let graph = verified_graph(&[FixtureSymbol {
        path: "src/lib.rs",
        qualified_name: "util",
        annotated_test: false,
    }]);
    let result = handle_run_affected_tests_with_runner(
        &cg,
        &graph,
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

#[test]
fn unsupported_profile_is_rejected_before_test_selection() {
    let result = RunAffectedArgs::parse(&json!({"profile": "bench", "format": "json"}))
        .expect_err("an unsupported profile must not silently become a debug test run");
    let output = tool_result_body(result);

    assert_eq!(output["error"]["kind"], "invalid_request");
    assert_eq!(output["error"]["operation"], "profile");
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

    let graph = verified_graph(&[FixtureSymbol {
        path: "tests/edited.rs",
        qualified_name: "timed_target",
        annotated_test: false,
    }]);
    let result = handle_run_affected_tests_with_runner(
        &cg,
        &graph,
        json!({"changed_paths": ["tests/edited.rs"], "format": "json"}),
        None,
        None,
        |_root, _profile, _tests, _timeout_duration, _control| async move {
            Err(TestRunFailure::Timeout {
                output_bytes: 17,
                partial: None,
            })
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
async fn cancellation_retains_results_completed_before_the_later_test() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join("tests")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn util() {}\n").unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"partial-cancelled-runner-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("tests/edited.rs"),
        "#[test]\nfn first() {}\n\n#[test]\nfn second() {}\n",
    )
    .unwrap();

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project,
        "project.mcp-affected-tests-partial-cancel",
    )
    .await
    .unwrap();
    {
        let database = cg.dashboard_database_guard();
        database
            .execute_write_batch(
                "seed partial cancelled managed test-run schema",
                crate::diagnostics_store::SCHEMA,
            )
            .await
            .unwrap();
    }

    let graph = verified_graph(&[
        FixtureSymbol {
            path: "tests/edited.rs",
            qualified_name: "first",
            annotated_test: false,
        },
        FixtureSymbol {
            path: "tests/edited.rs",
            qualified_name: "second",
            annotated_test: false,
        },
    ]);
    let result = handle_run_affected_tests_with_runner(
        &cg,
        &graph,
        json!({"changed_paths": ["tests/edited.rs"], "format": "json"}),
        None,
        None,
        |_root, _profile, _tests, _timeout_duration, _control| async move {
            Err(TestRunFailure::Cancelled {
                output_bytes: 20,
                partial: Some(TestRunOutput {
                    exit_code: Some(0),
                    stdout: "test first ... ok\n".to_owned(),
                    stderr: String::new(),
                    output_bytes: 20,
                }),
            })
        },
    )
    .await
    .unwrap();
    let output = tool_result_body(result);

    assert_eq!(output["error"]["kind"], "cargo");
    assert_eq!(output["terminal"]["receipt"]["termination"], "cancelled");
    assert_eq!(output["passed"], 1);
    assert_eq!(output["failed"], 0);
    assert_eq!(output["results"][0]["test"], "first");
    assert_eq!(output["results"][0]["passed"], true);

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

    let graph = verified_graph(&[FixtureSymbol {
        path: "tests/edited.rs",
        qualified_name: "selected_target",
        annotated_test: false,
    }]);
    let vacuous = handle_run_affected_tests_with_runner(
        &cg,
        &graph,
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
        &graph,
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

#[tokio::test]
async fn reported_passing_and_failing_tests_complete_with_observed_results() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join("tests")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn util() {}\n").unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"failing-test-result-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("tests/edited.rs"),
        "#[test]\nfn passing_target() {}\n\n#[test]\nfn failing_target() {}\n",
    )
    .unwrap();

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project,
        "project.mcp-affected-tests-failing-result",
    )
    .await
    .unwrap();
    {
        let database = cg.dashboard_database_guard();
        database
            .execute_write_batch(
                "seed failing managed test-run result schema",
                crate::diagnostics_store::SCHEMA,
            )
            .await
            .unwrap();
    }

    let graph = verified_graph(&[
        FixtureSymbol {
            path: "tests/edited.rs",
            qualified_name: "passing_target",
            annotated_test: false,
        },
        FixtureSymbol {
            path: "tests/edited.rs",
            qualified_name: "failing_target",
            annotated_test: false,
        },
    ]);
    let result = handle_run_affected_tests_with_runner(
        &cg,
        &graph,
        json!({"changed_paths": ["tests/edited.rs"], "format": "json"}),
        None,
        None,
        |_root, _profile, _tests, _timeout_duration, _control| async move {
            Ok(TestRunOutput {
                exit_code: Some(101),
                stdout: "test passing_target ... ok\ntest failing_target ... FAILED\n".to_owned(),
                stderr: "test result: FAILED\n".to_owned(),
                output_bytes: 76,
            })
        },
    )
    .await
    .unwrap();
    let output = tool_result_body(result);

    assert_eq!(output["terminal"]["receipt"]["termination"], "completed");
    assert_eq!(output["exit_code"], 101);
    assert_eq!(output["passed"], 1);
    assert_eq!(output["failed"], 1);
    assert_eq!(output["results"][0]["test"], "passing_target");
    assert_eq!(output["results"][0]["passed"], true);
    assert_eq!(output["results"][1]["test"], "failing_target");
    assert_eq!(output["results"][1]["passed"], false);

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
    let args = cargo_test_args(TestProfile::Debug, "nested::alpha");

    assert_eq!(
        args,
        ["test", "--no-fail-fast", "--", "--exact", "nested::alpha"]
    );
}

#[test]
fn cargo_test_args_keep_release_before_libtest_separator() {
    let args = cargo_test_args(TestProfile::Release, "nested::alpha");

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
