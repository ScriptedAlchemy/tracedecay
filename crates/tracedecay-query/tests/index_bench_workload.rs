//! The daemon-free index bench runs the production indexing workload on a
//! real-shaped corpus, not only on the committed fixture corpus.

use std::process::Command;

/// One function of 505 `let binding_N = b[N];` statements: 8 tokens each plus
/// `{`, `0`, `}` is 4,043 tokens, inside the automatic clone-body token cap,
/// and its two serialized token streams exceed 512 KiB.
fn generated_bindings_source() -> String {
    let statements: String = (0..505)
        .map(|ordinal| format!("    let binding_{ordinal:06} = b[{ordinal}];\n"))
        .collect();
    format!("pub fn generated_bindings(b: &[u64]) -> u64 {{\n{statements}    0\n}}\n")
}

#[test]
fn index_bench_drains_a_clone_body_the_daemon_page_admits() {
    let scratch = tempfile::tempdir().unwrap();
    let corpus = scratch.path().join("corpus");
    std::fs::create_dir_all(corpus.join("src")).unwrap();
    std::fs::write(corpus.join("src/lib.rs"), generated_bindings_source()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay-index-bench"))
        .arg("--corpus")
        .arg(&corpus)
        .env("TMPDIR", scratch.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "index bench failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["admitted_files"], 1);
    assert_eq!(
        report["clones"]["census"],
        serde_json::json!({
            "bodies": 1,
            "eligible_bodies": 1,
            "language_bodies": {"rust": 1},
            "source_tokens": 4043,
        })
    );
}
