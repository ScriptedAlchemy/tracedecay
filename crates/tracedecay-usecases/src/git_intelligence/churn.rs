//! Git integration helpers for churn analysis.
//! Shells out to `git log` at runtime to gather temporal data.
//! No data is persisted to the TraceDecay DB.

use std::collections::HashMap;
use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};

/// Returns a map of `file_path` → `commit_count` for the last `days` days.
/// Shells out to `git log --format= --name-only --since='{days} days ago'`.
/// Returns a typed unavailable error if Git cannot be spawned and an empty map
/// when the project is not a Git repository — including when its directory does
/// not exist at all.
#[hotpath::measure(label = "usecases.git_intelligence.file_churn", future = true)]
pub async fn file_churn(project_root: &Path, days: u32) -> Result<HashMap<String, usize>> {
    let git = tracedecay_runtime_core::git::try_git_program().map_err(|_| {
        TraceDecayError::HostCliUnavailable {
            program: "git".to_string(),
            lifecycle: "Git churn analysis".to_string(),
        }
    })?;
    let output = tokio::process::Command::new(git)
        .args([
            "log",
            "--format=",
            "--name-only",
            &format!("--since={days} days ago"),
        ])
        .current_dir(project_root)
        .output()
        .await;
    let output = match output {
        Ok(output) => output,
        // A spawn reports `NotFound` both for a missing program and for a
        // missing working directory, and `try_git_program` above already
        // resolved the program: a `NotFound` here is the checkout, not the host
        // CLI. Reporting it as an unavailable Git turns "this project root is
        // gone" into a host-installation problem, so it stays the same
        // "no churn to read" answer a non-repository directory gets. Windows
        // reports the missing working directory as `ERROR_DIRECTORY`
        // (`NotADirectory`) rather than `NotFound`.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(HashMap::new());
        }
        Err(error) => return Err(TraceDecayError::Io(error)),
    };

    if !output.status.success() {
        // Not a git repo, or another non-fatal git error
        return Ok(HashMap::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut churn: HashMap<String, usize> = HashMap::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        *churn.entry(trimmed.to_string()).or_insert(0) += 1;
    }
    Ok(churn)
}
