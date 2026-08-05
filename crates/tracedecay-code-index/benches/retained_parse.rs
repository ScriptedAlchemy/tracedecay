use std::{
    error::Error,
    fmt::Write as _,
    hint::black_box,
    process::Command,
    time::{Duration, Instant},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_code_extraction::{
    RustExtractor,
    incremental::{ParseCompleteness, ParseDocumentIdentity, ParseLimits, ParseReport, ParseReuse},
    parsed_extraction::{ParsedExtraction, ParsedExtractionDisposition},
};
use tracedecay_code_index::retained_parse::{RetainedParsePoolLimits, SharedRetainedParsePool};
use tracedecay_domain::{
    CommitId, ExtractionResult, ProjectId, RefId, RepositoryDirtyStateV1, RepositoryId, TreeId,
    WorktreeId,
};

const WARMUPS: usize = 5;
const MEASURED: usize = 30;
const CURRENT_FUNCTION_COUNT: usize = 4_096;
const TEN_X_FUNCTION_COUNT: usize = CURRENT_FUNCTION_COUNT * 10;

#[derive(Serialize)]
struct Sample {
    repetition: usize,
    wall_ns: u64,
    parser_ns: u64,
    changed_bytes: usize,
    changed_ranges: usize,
    extraction_disposition: &'static str,
    visited_top_level_nodes: usize,
    extracted_bytes: usize,
    canonical_rows_sha256: String,
    reset_extractions: u64,
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
    scales: Vec<ScaleEvaluation>,
}

#[derive(Serialize)]
struct ScaleEvaluation {
    scale: &'static str,
    workload: Workload,
    cold: Measurements,
    incremental: Measurements,
    criteria: Vec<Criterion>,
}

#[derive(Serialize)]
struct Acceptance {
    accepted: bool,
    criteria: Vec<Criterion>,
}

#[derive(Serialize)]
struct Criterion {
    name: String,
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
    let build = build_identity();
    let scales = vec![
        evaluate_scale("current", CURRENT_FUNCTION_COUNT)?,
        evaluate_scale("10x", TEN_X_FUNCTION_COUNT)?,
    ];
    let mut criteria = vec![Criterion {
        name: "immutable_clean_source".to_owned(),
        passed: !build.dirty && !build.commit.is_empty() && !build.tree.is_empty(),
        observed: format!(
            "commit={}, tree={}, dirty={}",
            build.commit, build.tree, build.dirty
        ),
    }];
    criteria.extend(
        scales
            .iter()
            .flat_map(|scale| scale.criteria.iter())
            .map(|criterion| Criterion {
                name: criterion.name.clone(),
                passed: criterion.passed,
                observed: criterion.observed.clone(),
            }),
    );
    let accepted = criteria.iter().all(|criterion| criterion.passed);
    let evaluation = Evaluation {
        schema_version: 2,
        evidence_status: if accepted { "accepted" } else { "rejected" },
        workload_id: "retained-tree-canonical-rust-current-plus-10x-v2",
        acceptance: Acceptance { accepted, criteria },
        build,
        environment: environment(),
        scales,
    };
    println!("{}", serde_json::to_string_pretty(&evaluation)?);
    if accepted {
        Ok(())
    } else {
        Err("retained parse evaluation did not satisfy its declared criteria".into())
    }
}

fn evaluate_scale(
    scale: &'static str,
    function_count: usize,
) -> Result<ScaleEvaluation, Box<dyn Error>> {
    let before = source_with_literal(function_count, "1");
    let after = source_with_literal(function_count, "123456");
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
    let max_extracted_bytes = incremental
        .iter()
        .map(|sample| sample.extracted_bytes)
        .max()
        .unwrap_or(0);
    let max_retained_bytes = incremental
        .iter()
        .map(|sample| sample.retained_source_bytes)
        .max()
        .unwrap_or(0);
    let cold_digests = cold
        .iter()
        .map(|sample| sample.canonical_rows_sha256.as_str())
        .collect::<Vec<_>>();
    let canonical_matches = incremental
        .iter()
        .all(|sample| cold_digests.contains(&sample.canonical_rows_sha256.as_str()));
    let prefix = |criterion: &str| format!("{scale}_{criterion}");
    let criteria = vec![
        Criterion {
            name: prefix("all_updates_reused_prior_tree"),
            passed: incremental
                .iter()
                .all(|sample| sample.reuse == "incremental"),
            observed: format!("{}/{} incremental", incremental.len(), MEASURED),
        },
        Criterion {
            name: prefix("all_updates_complete"),
            passed: incremental.iter().all(|sample| sample.complete),
            observed: format!("{}/{} complete", incremental.len(), MEASURED),
        },
        Criterion {
            name: prefix("all_extractions_changed_region_bounded"),
            passed: incremental.iter().all(|sample| {
                sample.extraction_disposition == "changed_regions"
                    && sample.reset_extractions == 0
                    && sample.visited_top_level_nodes == 1
            }),
            observed: format!(
                "max_extracted_bytes={max_extracted_bytes}, source_bytes={}",
                after.len()
            ),
        },
        Criterion {
            name: prefix("changed_parse_and_extraction_under_one_percent"),
            passed: max_changed_bytes.saturating_mul(100) < after.len()
                && max_extracted_bytes.saturating_mul(100) < after.len(),
            observed: format!(
                "parse={max_changed_bytes}, extraction={max_extracted_bytes}, source={}",
                after.len()
            ),
        },
        Criterion {
            name: prefix("canonical_rows_equal_cold_extraction"),
            passed: canonical_matches,
            observed: format!("{} incremental digests matched cold", incremental.len()),
        },
        Criterion {
            name: prefix("retained_source_within_document_bound"),
            passed: max_retained_bytes == after.len(),
            observed: format!("{max_retained_bytes} bytes"),
        },
        Criterion {
            name: prefix("incremental_median_faster_than_cold_median"),
            passed: incremental_distribution.p50_ns < cold_distribution.p50_ns,
            observed: format!(
                "incremental={}ns, cold={}ns",
                incremental_distribution.p50_ns, cold_distribution.p50_ns
            ),
        },
    ];
    Ok(ScaleEvaluation {
        scale,
        workload: Workload {
            warmups: WARMUPS,
            measured_repetitions: MEASURED,
            language: "rust",
            function_count,
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
        criteria,
    })
}

fn measure_cold(repetition: usize, source: &str) -> Result<Sample, Box<dyn Error>> {
    let pool = evaluation_pool()?;
    let extractor = RustExtractor;
    let started = Instant::now();
    let (report, extraction) = pool.parse_and_extract(
        identity("commit-cold", "tree-cold"),
        "rust",
        source,
        &extractor,
    )?;
    let wall = started.elapsed();
    Ok(sample(
        repetition,
        wall.as_nanos() as u64,
        &report,
        extraction,
        &pool,
    )?)
}

fn measure_incremental(
    repetition: usize,
    before: &str,
    after: &str,
) -> Result<Sample, Box<dyn Error>> {
    let pool = evaluation_pool()?;
    let extractor = RustExtractor;
    pool.parse_and_extract(
        identity("commit-before", "tree-before"),
        "rust",
        before,
        &extractor,
    )?;
    let started = Instant::now();
    let (report, extraction) = pool.parse_and_extract(
        identity("commit-after", "tree-after"),
        "rust",
        after,
        &extractor,
    )?;
    let wall = started.elapsed();
    Ok(sample(
        repetition,
        wall.as_nanos() as u64,
        &report,
        extraction,
        &pool,
    )?)
}

fn sample(
    repetition: usize,
    wall_ns: u64,
    report: &ParseReport,
    extraction: ParsedExtraction,
    pool: &SharedRetainedParsePool,
) -> Result<Sample, Box<dyn Error>> {
    let reuse = match report.reuse {
        ParseReuse::Initial => "initial",
        ParseReuse::Incremental => "incremental",
        ParseReuse::Noop => "noop",
        ParseReuse::Reset { .. } => "reset",
    };
    let extraction_disposition = match extraction.disposition {
        ParsedExtractionDisposition::FullDocument => "full_document",
        ParsedExtractionDisposition::ChangedRegions => "changed_regions",
        ParsedExtractionDisposition::Reset { .. } => "reset",
    };
    let stats = pool.stats();
    Ok(Sample {
        repetition,
        wall_ns,
        parser_ns: report.metrics.parse_elapsed.as_nanos() as u64,
        changed_bytes: report.metrics.changed_bytes,
        changed_ranges: report.metrics.changed_range_count,
        extraction_disposition,
        visited_top_level_nodes: extraction.metrics.visited_top_level_nodes,
        extracted_bytes: extraction.metrics.visited_bytes,
        canonical_rows_sha256: extraction_digest(extraction.result)?,
        reset_extractions: stats.reset_extractions,
        retained_source_bytes: stats.retained_source_bytes,
        reuse,
        complete: report.completeness == ParseCompleteness::Complete,
    })
}

fn evaluation_pool() -> Result<SharedRetainedParsePool, Box<dyn Error>> {
    Ok(SharedRetainedParsePool::new(RetainedParsePoolLimits {
        max_documents: 2,
        max_total_source_bytes: 8 * 1024 * 1024,
        document: ParseLimits {
            max_source_bytes: 4 * 1024 * 1024,
            max_changed_ranges: 256,
            max_parse_time: Duration::from_secs(3),
        },
    })?)
}

fn extraction_digest(mut result: ExtractionResult) -> Result<String, Box<dyn Error>> {
    for node in &mut result.nodes {
        node.updated_at = 0;
    }
    result.duration_ms = 0;
    result.sanitize();
    Ok(sha256(&serde_json::to_vec(&result)?))
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

fn source_with_literal(function_count: usize, literal: &str) -> String {
    let mut source = String::with_capacity(function_count * 64);
    for index in 0..function_count {
        let value = if index == function_count / 2 {
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
