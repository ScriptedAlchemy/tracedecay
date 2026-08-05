//! Post-plan verification owned by the API-migration transaction.

use std::collections::{BTreeSet, VecDeque};
use std::io::Write;
use std::process::{Command, Stdio};

use tracedecay_application::{
    ApiMigrationDiagnosticDeltaV1, ApiMigrationFormatterReportV1, ApiMigrationOperationRequestV1,
    ApiMigrationPlanV1,
};

use crate::diagnostics::{Diagnostic, Scope};
use crate::errors::{Result, TraceDecayError};

use super::super::TraceDecay;

type DiagnosticIdentity = (String, u32, String, String, String);

pub(super) fn verify_formatter(plan: &ApiMigrationPlanV1) -> Result<ApiMigrationFormatterReportV1> {
    let mut report = ApiMigrationFormatterReportV1::default();
    for file in plan
        .files
        .iter()
        .filter(|file| file.expected_content != file.intended_content)
    {
        if !file.path.ends_with(".rs") {
            report.not_applicable_files.push(file.path.clone());
            continue;
        }
        let mut child = Command::new("rustfmt")
            .args(["--edition", "2024", "--emit", "stdout"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| config_error(format!("cannot start rustfmt: {error}")))?;
        child
            .stdin
            .take()
            .ok_or_else(|| config_error("rustfmt stdin is unavailable"))?
            .write_all(file.intended_content.as_bytes())
            .map_err(|error| {
                config_error(format!("cannot send migration bytes to rustfmt: {error}"))
            })?;
        let output = child
            .wait_with_output()
            .map_err(|error| config_error(format!("cannot wait for rustfmt: {error}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(config_error(format!(
                "rustfmt rejected {}: {}",
                file.path,
                stderr.chars().take(512).collect::<String>()
            )));
        }
        if output.stdout != file.intended_content.as_bytes() {
            report.would_change_files.push(file.path.clone());
        }
        report.checked_files.push(file.path.clone());
    }
    Ok(report)
}

pub(super) async fn capture_diagnostics(
    graph: &TraceDecay,
    files: &[String],
) -> Result<BTreeSet<DiagnosticIdentity>> {
    let mut identities = BTreeSet::new();
    for file in files {
        let diagnostics =
            crate::diagnostics::run_all(graph.project_root(), &Scope::File { path: file.clone() })
                .await?;
        identities.extend(
            diagnostics
                .into_iter()
                .filter(|diagnostic| diagnostic.file == *file)
                .map(diagnostic_identity),
        );
    }
    Ok(identities)
}

pub(super) fn diagnostic_delta(
    before: &BTreeSet<DiagnosticIdentity>,
    after: &BTreeSet<DiagnosticIdentity>,
) -> ApiMigrationDiagnosticDeltaV1 {
    ApiMigrationDiagnosticDeltaV1 {
        introduced_errors: after
            .difference(before)
            .filter(|identity| identity.2 == "error")
            .count(),
        resolved_errors: before
            .difference(after)
            .filter(|identity| identity.2 == "error")
            .count(),
        introduced_warnings: after
            .difference(before)
            .filter(|identity| identity.2 == "warning")
            .count(),
        resolved_warnings: before
            .difference(after)
            .filter(|identity| identity.2 == "warning")
            .count(),
    }
}

pub(super) async fn affected_tests(graph: &TraceDecay, changed: &[String]) -> Result<Vec<String>> {
    const MAX_FILES: usize = 4_096;
    const MAX_DEPTH: usize = 8;

    let test_files = graph.get_files_with_test_annotations().await?;
    let mut seen = changed.iter().cloned().collect::<BTreeSet<_>>();
    let mut queue = changed
        .iter()
        .cloned()
        .map(|file| (file, 0_usize))
        .collect::<VecDeque<_>>();
    while let Some((file, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH {
            continue;
        }
        for dependent in graph.get_file_dependents(&file).await? {
            if seen.len() >= MAX_FILES {
                return Err(config_error(
                    "affected-test traversal exceeded its 4096-file safety bound",
                ));
            }
            if seen.insert(dependent.clone()) {
                queue.push_back((dependent, depth + 1));
            }
        }
    }
    Ok(seen
        .into_iter()
        .filter(|file| test_files.contains(file))
        .collect())
}

pub(super) async fn verify_renamed_callers(
    graph: &TraceDecay,
    plan: &ApiMigrationPlanV1,
) -> Result<bool> {
    for operation in &plan.operations {
        let ApiMigrationOperationRequestV1::RenameBoundSymbol {
            symbol, new_name, ..
        } = operation
        else {
            continue;
        };
        let target_qualified_name = symbol
            .qualified_name
            .strip_suffix(&symbol.old_name)
            .map_or_else(|| new_name.clone(), |prefix| format!("{prefix}{new_name}"));
        let targets = graph
            .get_nodes_by_qualified_name(&target_qualified_name)
            .await?
            .into_iter()
            .filter(|candidate| {
                candidate.file_path == symbol.file
                    && candidate.kind.as_str() == symbol.kind
                    && candidate.name == *new_name
            })
            .count();
        if targets != 1 || graph.get_node(&symbol.node_id).await?.is_some() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn diagnostic_identity(diagnostic: Diagnostic) -> DiagnosticIdentity {
    (
        diagnostic.file,
        diagnostic.line_start,
        diagnostic.level,
        diagnostic.code,
        diagnostic.message,
    )
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}
