mod common;
mod lance_case;
mod sqlite_case;

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use common::{BuildMetrics, CaseMetrics, Comparison, read_json, write_json};

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = env::args_os().skip(1);
    let command = arguments
        .next()
        .context("usage: vector-publication-ab <sqlite|lance|compare> ...")?;
    match command.to_string_lossy().as_ref() {
        "sqlite" => {
            let root = required_path(&mut arguments, "SQLite data root")?;
            let output = required_path(&mut arguments, "SQLite artifact")?;
            let executable = env::current_exe()?;
            write_json(&output, &sqlite_case::run(&root, &executable)?)?;
        }
        "lance" => {
            let root = required_path(&mut arguments, "Lance data root")?;
            let output = required_path(&mut arguments, "Lance artifact")?;
            let executable = env::current_exe()?;
            write_json(&output, &lance_case::run(&root, &executable).await?)?;
        }
        "compare" => {
            let sqlite_path = required_path(&mut arguments, "SQLite artifact")?;
            let lance_path = required_path(&mut arguments, "Lance artifact")?;
            let build_path = required_path(&mut arguments, "build artifact")?;
            let output = required_path(&mut arguments, "comparison artifact")?;
            compare(&sqlite_path, &lance_path, &build_path, &output)?;
        }
        "build-metrics" => {
            let time_path = required_path(&mut arguments, "time output")?;
            let binary_path = required_path(&mut arguments, "benchmark binary")?;
            let output = required_path(&mut arguments, "build artifact")?;
            let raw = std::fs::read_to_string(&time_path)?;
            let mut fields = raw.split_whitespace();
            let elapsed_seconds = fields
                .next()
                .context("missing build elapsed seconds")?
                .parse()?;
            let peak_rss_kib = fields.next().context("missing build peak RSS")?.parse()?;
            write_json(
                &output,
                &BuildMetrics {
                    elapsed_seconds,
                    peak_rss_kib,
                    binary_bytes: std::fs::metadata(binary_path)?.len(),
                },
            )?;
        }
        "sqlite-crash" => {
            let path = required_path(&mut arguments, "SQLite database")?;
            let phase = required_string(&mut arguments, "crash phase")?;
            sqlite_case::crash_child(&path, phase == "after")?;
        }
        "lance-crash" => {
            let root = required_path(&mut arguments, "Lance data root")?;
            let phase = required_string(&mut arguments, "crash phase")?;
            lance_case::crash_child(&root, phase == "after").await?;
        }
        other => anyhow::bail!("unknown command {other}"),
    }
    Ok(())
}

fn compare(sqlite_path: &Path, lance_path: &Path, build_path: &Path, output: &Path) -> Result<()> {
    let sqlite: CaseMetrics = read_json(sqlite_path)?;
    let lance: CaseMetrics = read_json(lance_path)?;
    let build: BuildMetrics = read_json(build_path)?;
    let exact_parity = sqlite.exact.results == lance.exact.results;
    let recommendation = if exact_parity
        && lance.ann_recall_at_10.unwrap_or_default() >= 0.90
        && lance.exact.p95_ms < sqlite.exact.p95_ms * 0.5
        && lance.peak_rss_kib < sqlite.peak_rss_kib * 2
        && lance.crash.before_commit_old_visible
        && lance.crash.after_commit_new_visible
    {
        "LanceDB warrants a separate production design review, but not direct adoption: its publication pointer still requires a second transactional authority."
    } else {
        "Keep normalized SQLite as the publication authority. LanceDB 0.31 did not clear the combined recall, latency, memory, build-cost, and native publication-CAS threshold."
    };
    write_json(
        output,
        &Comparison {
            workload: "150000x768-f32 initial publication plus 1/100/4096-row deltas; 10 deterministic cosine queries".to_owned(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            sqlite,
            lance,
            build,
            exact_parity,
            recommendation: recommendation.to_owned(),
        },
    )
}

fn required_path(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<PathBuf> {
    arguments
        .next()
        .map(PathBuf::from)
        .with_context(|| format!("missing {name}"))
}

fn required_string(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<String> {
    arguments
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .with_context(|| format!("missing {name}"))
}
