use tempfile::TempDir;
use tracedecay::bench::{BenchOptions, OutputFormat, run_bench};
use tracedecay::tracedecay::TraceDecay;

#[tokio::test]
async fn bench_fails_closed_until_admitted_graph_authority_is_mounted() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("a.rs"),
        "pub fn hello() {}\npub fn world() {}\n",
    )
    .unwrap();
    let cg =
        TraceDecay::init_with_options(tmp.path(), crate::fixture_profile::open_options(tmp.path()))
            .await
            .unwrap();

    let queries_path = tmp.path().join("q.toml");
    std::fs::write(
        &queries_path,
        r#"[[query]]
task = "where is hello defined"

[[query]]
task = "what does world do"
"#,
    )
    .unwrap();

    let error = run_bench(
        &cg,
        &queries_path,
        BenchOptions {
            format: OutputFormat::Json,
            max_nodes: 20,
        },
    )
    .await
    .expect_err("bench must not bypass admitted graph authority");

    match error {
        tracedecay::errors::TraceDecayError::ProjectRoute {
            reason_code,
            retryable,
            detail,
        } => {
            assert_eq!(reason_code, "verified-code-context-benchmark-unavailable");
            assert!(!retryable);
            assert!(detail.contains("admitted code-graph authority"));
        }
        other => panic!("unexpected error: {other}"),
    }
}
