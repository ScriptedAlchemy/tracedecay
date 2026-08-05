use std::{error::Error, fmt::Write as _, hint::black_box, process::Command, time::Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_code_extraction::incremental::{
    ParseCompleteness, ParseDocumentIdentity, ParseReuse,
};
use tracedecay_code_index::retained_parse::SharedRetainedParsePool;
use tracedecay_domain::{
    CommitId, ProjectId, RefId, RepositoryDirtyStateV1, RepositoryId, TreeId, WorktreeId,
};

const WARMUPS: usize = 5;
const MEASURED: usize = 30;
const FUNCTION_COUNT: usize = 4_096;

#[derive(Serialize)]
struct Sample {
    repetition: usize,
    wall_ns: u64,
    parser_ns: u64,
    changed_bytes: usize,
    changed_ranges: usize,
    retained_source_bytes: usize,
    reuse: &'static str,
    complete: bool,
}

#[derive(Serialize)]
struct Distribution {
    samples: usize,
    min_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    max_ns: u64,
}

#[derive(Serialize)]
struct Evaluation {
    schema_version: u64,
    evidence_status: &'static str,
    workload_id: &'static str,
    acceptance: Acceptance,
    build: BuildIdentity,
    environment: Environment,
    workload: Workload,
    cold: Measurements,
    incremental: Measurements,
}

#[derive(Serialize)]
struct Acceptance {
    accepted: bool,
    criteria: Vec<Criterion>,
}

#[derive(Serialize)]
struct Criterion {
    name: &'static str,
    passed: bool,
    observed: String,
}

#[derive(Serialize)]
struct BuildIdentity {
    commit: String,
    tree: String,
    dirty: bool,
    profile: &'static str,
    command: &'static str,
}

#[derive(Serialize)]
struct Environment {
    target: String,
    kernel: String,
    cpu: String,
    logical_cpus: usize,
    rustc: String,
    cargo: String,
}

#[derive(Serialize)]
struct Workload {
    warmups: usize,
    measured_repetitions: usize,
    language: &'static str,
    function_count: usize,
    before_bytes: usize,
    after_bytes: usize,
    before_sha256: String,
    after_sha256: String,
    edit_description: &'static str,
}

#[derive(Serialize)]
struct Measurements {
    wall: Distribution,
    raw_samples: Vec<Sample>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let before = source_with_literal("1");
    let after = source_with_literal("123456");

    for _ in 0..WARMUPS {
        black_box(measure_cold(0, &after)?);
        black_box(measure_incremental(0, &before, &after)?);
    }

    let cold = (0..MEASURED)
        .map(|repetition| measure_cold(repetition, &after))
        .collect::<Result<Vec<_>, _>>()?;
    let incremental = (0..MEASURED)
        .map(|repetition| measure_incremental(repetition, &before, &after))
        .collect::<Result<Vec<_>, _>>()?;
    let cold_distribution = distribution(&cold);
    let incremental_distribution = distribution(&incremental);
    let max_changed_bytes = incremental
        .iter()
        .map(|sample| sample.changed_bytes)
        .max()
        .unwrap_or(0);
    let max_retained_bytes = incremental
        .iter()
        .map(|sample| sample.retained_source_bytes)
        .max()
        .unwrap_or(0);
    let all_incremental = incremental
        .iter()
        .all(|sample| sample.reuse == "incremental");
    let all_complete = incremental.iter().all(|sample| sample.complete);
    let build = build_identity();

    let criteria = vec![
        Criterion {
            name: "immutable_clean_source",
            passed: !build.dirty && !build.commit.is_empty() && !build.tree.is_empty(),
            observed: format!(
                "commit={}, tree={}, dirty={}",
                build.commit, build.tree, build.dirty
            ),
        },
        Criterion {
            name: "all_measured_updates_reused_prior_tree",
            passed: all_incremental,
            observed: format!("{}/{} incremental", incremental.len(), MEASURED),
        },
        Criterion {
            name: "all_measured_updates_complete",
            passed: all_complete,
            observed: format!("{}/{} complete", incremental.len(), MEASURED),
        },
        Criterion {
            name: "changed_work_bounded_to_less_than_one_percent",
            passed: max_changed_bytes.saturating_mul(100) < after.len(),
            observed: format!("{max_changed_bytes}/{} bytes", after.len()),
        },
        Criterion {
            name: "retained_source_within_document_bound",
            passed: max_retained_bytes == after.len(),
            observed: format!("{max_retained_bytes} bytes"),
        },
        Criterion {
            name: "incremental_median_faster_than_cold_median",
            passed: incremental_distribution.p50_ns < cold_distribution.p50_ns,
            observed: format!(
                "incremental={}ns, cold={}ns",
                incremental_distribution.p50_ns, cold_distribution.p50_ns
            ),
        },
    ];
    let accepted = criteria.iter().all(|criterion| criterion.passed);
    let evaluation = Evaluation {
        schema_version: 1,
        evidence_status: if accepted { "accepted" } else { "rejected" },
        workload_id: "retained-tree-rust-single-literal-v1",
        acceptance: Acceptance { accepted, criteria },
        build,
        environment: environment(),
        workload: Workload {
            warmups: WARMUPS,
            measured_repetitions: MEASURED,
            language: "rust",
            function_count: FUNCTION_COUNT,
            before_bytes: before.len(),
            after_bytes: after.len(),
            before_sha256: sha256(before.as_bytes()),
            after_sha256: sha256(after.as_bytes()),
            edit_description: "replace one integer literal in the middle function",
        },
        cold: Measurements {
            wall: cold_distribution,
            raw_samples: cold,
        },
        incremental: Measurements {
            wall: incremental_distribution,
            raw_samples: incremental,
        },
    };
    println!("{}", serde_json::to_string_pretty(&evaluation)?);
    if accepted {
        Ok(())
    } else {
        Err("retained parse evaluation did not satisfy its declared criteria".into())
    }
}

fn measure_cold(repetition: usize, source: &str) -> Result<Sample, Box<dyn Error>> {
    let pool = SharedRetainedParsePool::default();
    let started = Instant::now();
    let report = pool.parse(identity("commit-cold", "tree-cold"), "rust", source)?;
    let wall = started.elapsed();
    Ok(sample(
        repetition,
        wall.as_nanos() as u64,
        &report,
        pool.stats().retained_source_bytes,
    ))
}

fn measure_incremental(
    repetition: usize,
    before: &str,
    after: &str,
) -> Result<Sample, Box<dyn Error>> {
    let pool = SharedRetainedParsePool::default();
    pool.parse(identity("commit-before", "tree-before"), "rust", before)?;
    let started = Instant::now();
    let report = pool.parse(identity("commit-after", "tree-after"), "rust", after)?;
    let wall = started.elapsed();
    Ok(sample(
        repetition,
        wall.as_nanos() as u64,
        &report,
        pool.stats().retained_source_bytes,
    ))
}

fn sample(
    repetition: usize,
    wall_ns: u64,
    report: &tracedecay_code_extraction::incremental::ParseReport,
    retained_source_bytes: usize,
) -> Sample {
    let reuse = match report.reuse {
        ParseReuse::Initial => "initial",
        ParseReuse::Incremental => "incremental",
        ParseReuse::Noop => "noop",
        ParseReuse::Reset { .. } => "reset",
    };
    Sample {
        repetition,
        wall_ns,
        parser_ns: report.metrics.parse_elapsed.as_nanos() as u64,
        changed_bytes: report.metrics.changed_bytes,
        changed_ranges: report.metrics.changed_range_count,
        retained_source_bytes,
        reuse,
        complete: report.completeness == ParseCompleteness::Complete,
    }
}

fn distribution(samples: &[Sample]) -> Distribution {
    let mut values = samples
        .iter()
        .map(|sample| sample.wall_ns)
        .collect::<Vec<_>>();
    values.sort_unstable();
    Distribution {
        samples: values.len(),
        min_ns: percentile(&values, 0),
        p50_ns: percentile(&values, 50),
        p95_ns: percentile(&values, 95),
        max_ns: percentile(&values, 100),
    }
}

fn percentile(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let index = (values.len() - 1).saturating_mul(percentile) / 100;
    values[index]
}

fn source_with_literal(literal: &str) -> String {
    let mut source = String::with_capacity(FUNCTION_COUNT * 64);
    for index in 0..FUNCTION_COUNT {
        let value = if index == FUNCTION_COUNT / 2 {
            literal
        } else {
            "1"
        };
        writeln!(
            source,
            "#[inline]\npub fn generated_{index}() -> usize {{ {value} }}\n"
        )
        .expect("writing to a String cannot fail");
    }
    source
}

fn identity(commit: &str, tree: &str) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: ProjectId::new("project.retained-eval").expect("project id"),
        repository_id: RepositoryId::new("repository.retained-eval").expect("repository id"),
        worktree_id: Some(WorktreeId::new("worktree.retained-eval").expect("worktree id")),
        reference: Some(RefId::new("refs/heads/evaluation").expect("ref id")),
        commit: Some(CommitId::new(commit).expect("commit id")),
        tree: Some(TreeId::new(tree).expect("tree id")),
        dirty: RepositoryDirtyStateV1::Dirty,
        logical_path: "src/generated.rs".to_owned(),
    }
}

fn build_identity() -> BuildIdentity {
    BuildIdentity {
        commit: command_output("git", &["rev-parse", "HEAD"]),
        tree: command_output("git", &["rev-parse", "HEAD^{tree}"]),
        dirty: !command_output("git", &["status", "--porcelain"]).is_empty(),
        profile: "release",
        command: "cargo bench -p tracedecay-code-index --no-default-features --features lite --bench retained_parse",
    }
}

fn environment() -> Environment {
    Environment {
        target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        kernel: command_output("uname", &["-srvmo"]),
        cpu: std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|contents| {
                contents.lines().find_map(|line| {
                    line.strip_prefix("model name")
                        .and_then(|value| value.split_once(':'))
                        .map(|(_, value)| value.trim().to_owned())
                })
            })
            .unwrap_or_else(|| "unavailable".to_owned()),
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(0),
        rustc: command_output("rustc", &["--version"]),
        cargo: command_output("cargo", &["--version"]),
    }
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_owned())
        .unwrap_or_default()
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
