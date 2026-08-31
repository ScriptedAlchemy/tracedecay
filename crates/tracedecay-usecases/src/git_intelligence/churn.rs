//! Git integration helpers for churn analysis.
//! Shells out to `git log` at runtime to gather temporal data.
//! No data is persisted to the TraceDecay DB.

use std::collections::HashMap;
use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};

/// Returns a map of `file_path` → `commit_count` for the last `days` days.
/// Shells out to `git log --format= --name-only --since='{days} days ago'`.
/// Returns a typed unavailable error if Git cannot be spawned and an empty map
/// when the project is not a Git repository.
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
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TraceDecayError::HostCliUnavailable {
                    program: "git".to_string(),
                    lifecycle: "Git churn analysis".to_string(),
                }
            } else {
                TraceDecayError::Io(error)
            }
        })?;

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
