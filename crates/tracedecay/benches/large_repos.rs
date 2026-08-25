//! Criterion benchmark: tracedecay MCP tools against large, real-world repos.
//!
//! What it does:
//!  1. Reads `TRACEDECAY_BENCH_REPOS_DIR` (on-disk cache for the cloned repos).
//!     If unset, prints a message and registers zero benchmarks.
//!  2. For each selected repo (see `repos::REPOS`, optionally filtered with
//!     `TRACEDECAY_BENCH_REPOS=name1,name2`), shallow-clones it (`git fetch
//!     --depth 1`) at a constant ref the first time it is encountered.
//!  3. Opens the repos in one isolated production daemon composition. The
//!     daemon performs normal final-schema admission and waits for its
//!     background code-index scheduler to publish a complete generation.
//!  4. Samples that generation through mounted MCP calls to build one query
//!     catalog per repo:
//!     ≥ 5 queries per tool, each holding concrete `node_id` / qualified-name
//!     / file-pattern arguments drawn from real graph state. Write queries
//!     (`str_replace`, `multi_str_replace`, `insert_at`, `ast_grep_rewrite`)
//!     also declare a scratch file that is rewritten before every timed
//!     iteration via `iter_batched`.
//!  5. Runs every (repo × tool × query) combination through criterion with
//!     `sample_size = 10` and `measurement_time = 30s`.
//!  6. When all benches finish, runs `git stash --include-untracked` inside
//!     each prepared repo so mutations made by the write benches are reverted.
//!
//! Environment variables:
//!   TRACEDECAY_BENCH_REPOS_DIR   required — root directory for cloned repos
//!   TRACEDECAY_BENCH_REPOS       optional — comma-separated repo subset
//!   TRACEDECAY_BENCH_SKIP_CLONE  optional — fail rather than clone

mod queries;
mod repos;

use std::path::PathBuf;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde_json::Value;
use tokio::runtime::Runtime;

use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use queries::{Query, QueryKind, SCRATCH_DIR, ToolGroup, build_context, build_queries};
use repos::{Repo, ensure_cloned, repos_root, restore_repo, selected_repos};

/// Per-repo state we hand to criterion: mounted project + frozen query catalog.
struct RepoBench {
    dir: PathBuf,
    name: &'static str,
    groups: Vec<ToolGroup>,
}

async fn prepare_repo(
    harness: &ProductionProjectCompositionHarnessV1,
    dir: PathBuf,
    repo: Repo,
) -> Result<RepoBench, String> {
    let ctx = build_context(harness, &dir)
        .await
        .map_err(|error| format!("sample {}: {error}", repo.name))?;
    let groups = build_queries(&ctx);
    Ok(RepoBench {
        dir,
        name: repo.name,
        groups,
    })
}

fn run_query(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    q: &Query,
) -> Value {
    rt.block_on(async {
        // Preserve the complete wire response so criterion cannot optimize the
        // mounted JSON-RPC dispatch or response rendering away.
        match harness
            .call_tool(project_root, q.tool, q.args.clone())
            .await
        {
            Ok(response) => {
                if response.error.is_some()
                    || response
                        .result
                        .as_ref()
                        .and_then(|result| result.get("isError"))
                        .and_then(Value::as_bool)
                        == Some(true)
                {
                    panic!("{} returned an error response: {response:?}", q.tool);
                }
                match serde_json::to_value(response) {
                    Ok(value) => value,
                    Err(error) => panic!("{} response serialization failed: {error}", q.tool),
                }
            }
            Err(error) => panic!("{} transport failed: {error}", q.tool),
        }
    })
}

/// Re-create `scratch_path` (relative to `project_root`) with `init_content`.
/// Runs before every timed iteration of a write bench so the edit primitive's
/// uniqueness check keeps passing.
fn reset_scratch(project_root: &std::path::Path, scratch_path: &str, init_content: &str) {
    let abs = project_root.join(scratch_path);
    if let Some(parent) = abs.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&abs, init_content) {
        eprintln!(
            "[bench] WARNING: failed to write scratch {}: {e}",
            abs.display()
        );
    }
}

fn bench_all(c: &mut Criterion) {
    let Some(root) = repos_root() else {
        eprintln!(
            "[bench] TRACEDECAY_BENCH_REPOS_DIR is unset — skipping large-repo benchmarks. \
             Set it to a writable directory to enable them."
        );
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&root) {
        eprintln!("[bench] cannot create {}: {e}", root.display());
        return;
    }

    let rt = Runtime::new().expect("create tokio runtime");

    let repos = selected_repos();
    if repos.is_empty() {
        eprintln!("[bench] no repos selected (TRACEDECAY_BENCH_REPOS filter excluded everything)");
        return;
    }

    let mut cloned = Vec::new();
    for repo in &repos {
        match ensure_cloned(&root, *repo) {
            Ok(dir) => cloned.push((*repo, dir)),
            Err(e) => eprintln!("[bench] skipping {}: {e}", repo.name),
        }
    }
    if cloned.is_empty() {
        return;
    }

    eprintln!(
        "[bench] mounting {} repositories in the production composition...",
        cloned.len()
    );
    let project_roots = cloned.iter().map(|(_, dir)| dir.clone());
    let harness = match rt.block_on(ProductionProjectCompositionHarnessV1::open(
        &root,
        project_roots,
    )) {
        Ok(harness) => harness,
        Err(error) => {
            eprintln!("[bench] production composition failed: {error}");
            return;
        }
    };

    let mut prepared: Vec<RepoBench> = Vec::new();
    for (repo, dir) in cloned {
        match rt.block_on(prepare_repo(&harness, dir, repo)) {
            Ok(rb) => prepared.push(rb),
            Err(e) => eprintln!("[bench] skipping {}: {e}", repo.name),
        }
    }

    for rb in &prepared {
        for group in &rb.groups {
            let mut g = c.benchmark_group(format!("{}/{}", rb.name, group.tool));
            g.throughput(Throughput::Elements(1));
            for (i, q) in group.queries.iter().enumerate() {
                let id = BenchmarkId::new(q.label, i);
                match &q.kind {
                    QueryKind::Read => {
                        g.bench_with_input(id, q, |b, q| {
                            b.iter(|| run_query(&rt, &harness, &rb.dir, q));
                        });
                    }
                    QueryKind::Write {
                        scratch_path,
                        init_content,
                    } => {
                        let root = rb.dir.clone();
                        let scratch = scratch_path.clone();
                        let init = init_content.clone();
                        g.bench_with_input(id, q, |b, q| {
                            b.iter_batched(
                                || reset_scratch(&root, &scratch, &init),
                                |()| run_query(&rt, &harness, &rb.dir, q),
                                BatchSize::SmallInput,
                            );
                        });
                    }
                }
            }
            g.finish();
        }
    }

    rt.block_on(harness.shutdown());

    // Revert all scratch-file churn (and any other accidental edits) in each
    // repo we touched. `git stash --include-untracked` puts everything aside;
    // we then drop the stash so the working tree matches the pinned ref again.
    for rb in &prepared {
        eprintln!(
            "[bench] reverting changes in {} (git stash + drop)...",
            rb.name
        );
        let _ = std::fs::remove_dir_all(rb.dir.join(SCRATCH_DIR));
        if let Err(e) = restore_repo(&rb.dir) {
            eprintln!("[bench] WARNING: revert failed for {}: {e}", rb.name);
        }
    }
}

fn criterion_config() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(30))
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_all
}
criterion_main!(benches);
