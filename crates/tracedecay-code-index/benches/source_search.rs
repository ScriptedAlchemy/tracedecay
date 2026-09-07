use std::env;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use tracedecay_code_index::grep_search::{GrepSearchQuery, search_tree_with_cancel};

const TRACKED_FILES: usize = 128;
const GENERATED_FILES: usize = 2_048;
const WARMUPS: usize = 5;
const SAMPLES: usize = 20;

fn main() {
    if let Err(error) = run() {
        eprintln!("source search benchmark: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args()
        .skip(1)
        .filter(|argument| argument != "--bench")
        .collect::<Vec<_>>();
    if !arguments.is_empty() {
        return Err("usage: cargo bench -p tracedecay-code-index --bench source_search".to_owned());
    }

    let corpus =
        tempfile::tempdir().map_err(|error| format!("create benchmark corpus: {error}"))?;
    build_corpus(corpus.path())?;

    let tracked_query = GrepSearchQuery {
        pattern: "benchmark_target".to_owned(),
        fixed_strings: true,
        path_glob: None,
        case_sensitive: true,
        context_lines: 0,
        max_results: TRACKED_FILES,
    };
    let generated_query = GrepSearchQuery {
        pattern: "generated_target".to_owned(),
        fixed_strings: true,
        path_glob: Some("dist/**/*.js".to_owned()),
        case_sensitive: true,
        context_lines: 0,
        max_results: GENERATED_FILES,
    };
    verify_workload(corpus.path(), &tracked_query, TRACKED_FILES)?;
    verify_workload(corpus.path(), &generated_query, GENERATED_FILES)?;

    report(
        "tracked_default_generated_pruned",
        measure(corpus.path(), &tracked_query)?,
    );
    report(
        "explicit_generated_scope",
        measure(corpus.path(), &generated_query)?,
    );
    Ok(())
}

fn build_corpus(root: &Path) -> Result<(), String> {
    let tracked = root.join("src");
    let generated = root.join("dist/assets");
    fs::create_dir_all(&tracked).map_err(|error| format!("create tracked directory: {error}"))?;
    fs::create_dir_all(&generated)
        .map_err(|error| format!("create generated directory: {error}"))?;

    for index in 0..TRACKED_FILES {
        fs::write(
            tracked.join(format!("module_{index:04}.rs")),
            format!("pub const BENCH_{index}: &str = \"benchmark_target\";\n"),
        )
        .map_err(|error| format!("write tracked fixture: {error}"))?;
    }
    for index in 0..GENERATED_FILES {
        fs::write(
            generated.join(format!("bundle_{index:04}.js")),
            format!("export const bench{index} = 'generated_target';\n"),
        )
        .map_err(|error| format!("write generated fixture: {error}"))?;
    }
    Ok(())
}

fn verify_workload(
    root: &Path,
    query: &GrepSearchQuery,
    expected_hits: usize,
) -> Result<(), String> {
    let result = search_tree_with_cancel(root, query, || false)
        .map_err(|error| format!("verify workload: {error}"))?;
    if result.cancelled || result.hits.len() != expected_hits {
        return Err(format!(
            "workload returned {} hits (cancelled={}), expected {expected_hits}",
            result.hits.len(),
            result.cancelled
        ));
    }
    Ok(())
}

fn measure(root: &Path, query: &GrepSearchQuery) -> Result<Vec<Duration>, String> {
    for _ in 0..WARMUPS {
        search_tree_with_cancel(root, query, || false)
            .map_err(|error| format!("warm source search: {error}"))?;
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        search_tree_with_cancel(root, query, || false)
            .map_err(|error| format!("measure source search: {error}"))?;
        samples.push(start.elapsed());
    }
    samples.sort_unstable();
    Ok(samples)
}

fn report(name: &str, samples: Vec<Duration>) {
    let median = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
    println!(
        "{name}: files(tracked={TRACKED_FILES}, generated={GENERATED_FILES}) \
         samples={SAMPLES} median={median:?} p95={p95:?}"
    );
}
