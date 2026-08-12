//! Bounded pending-index authority for automatic-effect crash recovery.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_application::retained_surfaces::{
    MemoryAutomationRunProblemV1, RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation,
};
use tracedecay_application::{
    ApplicationProblemEnvelope, CancellationSignal, DirectorySyncPolicy, ProblemOwningLayer,
    ResolvedScope, retained_surface_application_operation, retained_surface_execution_problem,
};
use tracedecay_domain::{ManifestDigest, ProjectId, RunId};
use tracedecay_store::FactReadControl;

use super::{
    AutomationSettledTerminal, contract_error, digest,
    journal::{self, DurableAutomationAdmission},
    projection::project_recovered_committed_receipts,
};
use crate::errors::Result;

const INDEX_SCHEMA_VERSION: u32 = 1;
const MAX_PENDING_AUTOMATION_EFFECTS: usize = 256;
const MAX_INDEX_BYTES: u64 = 128 * 1024;
const INDEX_FILENAME: &str = "pending-index.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutomationEffectRecoveryReport {
    pub(crate) inspected: usize,
    pub(crate) partial_effects: usize,
    pub(crate) reset_required: usize,
    pub(crate) already_terminal: usize,
    pub(crate) deferred: usize,
}

pub(crate) async fn reconcile_reserved_automation_effects_for_project(
    memory: &crate::tracedecay::TraceDecay,
    dashboard_root: &Path,
    cancellation: &CancellationSignal,
) -> Result<AutomationEffectRecoveryReport> {
    let owner = memory.project_memory_owner()?;
    let tracedecay_domain::FactOwnerV1::Project { project_id } = &owner else {
        return Err(contract_error(
            "automation recovery requires a project owner",
        ));
    };
    let scope = crate::daemon::project_open_owners::resolved_scope_for_project(
        memory.project_root(),
        project_id,
    )
    .map_err(|error| contract_error(format!("automation recovery scope is invalid: {error:?}")))?;
    let root = dashboard_root.to_path_buf();
    let indexed_scope = scope.clone();
    let indexed =
        tokio::task::spawn_blocking(move || indexed_journals_blocking(&root, &indexed_scope))
            .await
            .map_err(|error| {
                contract_error(format!("automation recovery index reader failed: {error}"))
            })??;
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::MemoryAutomationRun)
            .map_err(contract_error)?;
    let mut report = AutomationEffectRecoveryReport::default();
    for indexed in indexed {
        if cancellation.is_cancelled() {
            break;
        }
        report.inspected += 1;
        if indexed.project_id != *project_id || indexed.scope_digest != scope.scope_digest {
            report.deferred += 1;
            continue;
        }
        let path = indexed.path.clone();
        let record =
            match tokio::task::spawn_blocking(move || journal::read_indexed_record_blocking(&path))
                .await
            {
                Ok(Ok(record)) => record,
                Ok(Err(error)) => {
                    tracing::warn!(event = "automation_effect_recovery_deferred", error = %error);
                    report.deferred += 1;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(event = "automation_effect_recovery_deferred", error = %error);
                    report.deferred += 1;
                    continue;
                }
            };
        let Some(record) = record else {
            remove_pending_async(dashboard_root, &indexed.path).await?;
            continue;
        };
        if record.terminal().is_some() {
            remove_pending_async(dashboard_root, &indexed.path).await?;
            report.already_terminal += 1;
            continue;
        }
        let admission = record.admission().clone();
        if admission.schema_version != INDEX_SCHEMA_VERSION
            || !admission.request.validate()
            || admission.process_run_id == crate::runtime_identity::process_run_id()
            || admission.owner != owner
            || admission.scope != scope
            || indexed.path.file_name().and_then(|name| name.to_str())
                != Some(&automation_journal_filename(&admission.request.run_id)?)
            || !admission_has_exact_authority(&admission, &operation)?
        {
            tracing::warn!(
                event = "automation_effect_recovery_deferred",
                journal = %indexed.path.display(),
                reason = "binding_or_authority_mismatch",
            );
            report.deferred += 1;
            continue;
        }
        let read_cancellation = cancellation.clone();
        let read_control = FactReadControl::new(Arc::new(move || read_cancellation.is_cancelled()));
        let recovered = match memory
            .project_memory_application()
            .await?
            .project_memory_automation_run_receipts(admission.request.run_id.clone(), &read_control)
            .await
        {
            Ok(recovered) => recovered,
            Err(error) => {
                tracing::warn!(event = "automation_effect_recovery_deferred", error = %error);
                report.deferred += 1;
                continue;
            }
        };
        if cancellation.is_cancelled() {
            break;
        }
        let committed =
            project_recovered_committed_receipts(&admission.request.run_id, &recovered)?;
        if let Some(reason) = special_recovery_defer_reason(&admission, committed.is_empty()) {
            tracing::warn!(
                event = "automation_effect_recovery_deferred",
                journal = %indexed.path.display(),
                reason,
            );
            report.deferred += 1;
            continue;
        }
        if admission.retirement.is_some() && !committed.is_empty() {
            return Err(contract_error(
                "proposal retirement recovery found unrelated canonical memory commits",
            ));
        }
        let terminal = if committed.is_empty() {
            AutomationSettledTerminal::Problem(admission.recovery_problem.clone())
        } else {
            recovered_partial_terminal(&admission, committed, &operation)?
        };
        let path = indexed.path.clone();
        let requested = admission.clone();
        let root = dashboard_root.to_path_buf();
        let write_cancellation = cancellation.clone();
        let committed = tokio::task::spawn_blocking(move || {
            let Some(_) = journal::persist_recovered_terminal_blocking(
                &path,
                &requested,
                terminal,
                Some(&write_cancellation),
            )?
            else {
                return Ok(false);
            };
            remove_pending_blocking(&root, &path)?;
            Ok(true)
        })
        .await
        .map_err(|error| contract_error(format!("automation recovery writer failed: {error}")))??;
        if !committed {
            break;
        }
        if recovered.is_empty() {
            report.reset_required += 1;
        } else {
            report.partial_effects += 1;
        }
    }
    Ok(report)
}

pub(super) fn special_recovery_defer_reason(
    admission: &DurableAutomationAdmission,
    committed_receipts_empty: bool,
) -> Option<&'static str> {
    if committed_receipts_empty && admission.retirement.is_some() {
        Some("retirement_requires_exact_finalization")
    } else if committed_receipts_empty && admission.reset_source_digest.is_some() {
        Some("shipped_proposals_require_exact_reset_diagnostic")
    } else {
        None
    }
}

fn admission_has_exact_authority(
    admission: &DurableAutomationAdmission,
    operation: &tracedecay_application::ApplicationOperation,
) -> Result<bool> {
    Ok(digest(&(
        "tracedecay.memory-automation-run.effect-authority.v1",
        &admission.actor,
        &admission.scope,
        &admission.grant_id,
        admission.grant_revision,
        &admission.grant_digest,
        admission.disclosure,
        operation.capability_id(),
        operation.use_case_id(),
        operation.result_contract(),
        &admission.configuration_digest,
        &admission.input_digest,
        &admission.request,
        &admission.effect_receipt_template,
    ))? == admission.effect_authority_digest)
}

fn recovered_partial_terminal(
    admission: &DurableAutomationAdmission,
    committed: Vec<tracedecay_application::retained_surfaces::MemoryAutomationCommittedReceiptV1>,
    operation: &tracedecay_application::ApplicationOperation,
) -> Result<AutomationSettledTerminal> {
    let state = digest(&(
        "tracedecay.memory-automation-run.partial-state.v1",
        admission.request.run_id.as_str(),
        &committed,
    ))?;
    let mut receipt = admission.effect_receipt_template.clone();
    receipt.committed_state = Some(state);
    let problem =
        retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: "application.memory-automation-run.recovered-partial-effect".to_owned(),
            committed_receipt: receipt,
            detail:
                "Canonical memory effects committed before the outer run terminal was published."
                    .to_owned(),
        });
    let envelope = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        admission.request_id.clone(),
        problem,
    )
    .map(|problem| problem.with_owning_layer(ProblemOwningLayer::Application))
    .map_err(contract_error)?;
    Ok(AutomationSettledTerminal::Problem(
        MemoryAutomationRunProblemV1::new(
            admission.request.run_id.clone(),
            admission.request.task_kind(),
            admission.scope.clone(),
            envelope,
            committed,
            &admission.request_id,
        )
        .map_err(contract_error)?,
    ))
}

fn automation_journal_filename(run_id: &RunId) -> Result<String> {
    let key = digest(&("tracedecay.memory-automation-run.terminal-key.v1", run_id))?;
    Ok(format!(
        "{}.json",
        key.as_str().trim_start_matches("sha256:")
    ))
}

async fn remove_pending_async(dashboard_root: &Path, journal_path: &Path) -> Result<()> {
    let root = dashboard_root.to_path_buf();
    let path = journal_path.to_path_buf();
    tokio::task::spawn_blocking(move || remove_pending_blocking(&root, &path))
        .await
        .map_err(|error| contract_error(format!("automation pending cleanup failed: {error}")))?
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingIndexEntry {
    journal_file: String,
    project_id: ProjectId,
    scope_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingIndex {
    schema_version: u32,
    entries: Vec<PendingIndexEntry>,
}

pub(super) struct IndexedJournal {
    pub(super) path: PathBuf,
    pub(super) project_id: ProjectId,
    pub(super) scope_digest: ManifestDigest,
}

pub(super) fn add_pending_blocking(
    dashboard_root: &Path,
    journal_path: &Path,
    admission: &DurableAutomationAdmission,
) -> Result<()> {
    let entry = entry_for(journal_path, &admission.scope)?;
    mutate_index(dashboard_root, |index| {
        if index.entries.iter().any(|candidate| candidate == &entry) {
            return Ok(());
        }
        if index
            .entries
            .iter()
            .any(|candidate| candidate.journal_file == entry.journal_file && candidate != &entry)
        {
            return Err(contract_error(
                "automation pending index journal identity conflicts with its project binding",
            ));
        }
        if index.entries.len() >= MAX_PENDING_AUTOMATION_EFFECTS {
            return Err(contract_error(
                "automation pending recovery index reached its bounded capacity",
            ));
        }
        index.entries.push(entry);
        index
            .entries
            .sort_by(|left, right| left.journal_file.cmp(&right.journal_file));
        Ok(())
    })
}

pub(super) fn remove_pending_blocking(dashboard_root: &Path, journal_path: &Path) -> Result<()> {
    let journal_file = journal_filename(journal_path)?;
    mutate_index(dashboard_root, |index| {
        index
            .entries
            .retain(|entry| entry.journal_file != journal_file);
        Ok(())
    })
}

pub(super) fn indexed_journals_blocking(
    dashboard_root: &Path,
    scope: &ResolvedScope,
) -> Result<Vec<IndexedJournal>> {
    let index_path = index_path(dashboard_root);
    with_index_lock(&index_path, || {
        let index = read_index(&index_path)?;
        let automation_root = automation_root(dashboard_root);
        Ok(index
            .entries
            .into_iter()
            .filter(|entry| {
                entry.project_id == scope.project_id && entry.scope_digest == scope.scope_digest
            })
            .map(|entry| IndexedJournal {
                path: automation_root.join(&entry.journal_file),
                project_id: entry.project_id,
                scope_digest: entry.scope_digest,
            })
            .collect())
    })
}

fn mutate_index(
    dashboard_root: &Path,
    mutate: impl FnOnce(&mut PendingIndex) -> Result<()>,
) -> Result<()> {
    let path = index_path(dashboard_root);
    with_index_lock(&path, || {
        let mut index = read_index(&path)?;
        mutate(&mut index)?;
        let bytes = serde_json::to_vec_pretty(&index).map_err(contract_error)?;
        if bytes.len() > MAX_INDEX_BYTES as usize {
            return Err(contract_error(
                "automation pending recovery index exceeds its byte bound",
            ));
        }
        tracedecay_application::atomic_write(
            &path,
            "memory-automation-pending-index",
            &bytes,
            DirectorySyncPolicy::Strict,
        )
        .map_err(|error| contract_error(format!("automation pending index write failed: {error}")))
    })
}

fn read_index(path: &Path) -> Result<PendingIndex> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingIndex {
                schema_version: INDEX_SCHEMA_VERSION,
                entries: Vec::new(),
            });
        }
        Err(error) => {
            return Err(contract_error(format!(
                "automation pending index metadata read failed: {error}"
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES
    {
        return Err(contract_error(
            "automation pending index is not a bounded regular file",
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        contract_error(format!("automation pending index read failed: {error}"))
    })?;
    let index: PendingIndex = serde_json::from_slice(&bytes).map_err(contract_error)?;
    if index.schema_version != INDEX_SCHEMA_VERSION
        || index.entries.len() > MAX_PENDING_AUTOMATION_EFFECTS
    {
        return Err(contract_error(
            "automation pending index has an unsupported or unbounded shape",
        ));
    }
    for entry in &index.entries {
        validate_journal_filename(&entry.journal_file)?;
        entry.project_id.validate().map_err(contract_error)?;
        entry.scope_digest.validate().map_err(contract_error)?;
    }
    Ok(index)
}

fn entry_for(path: &Path, scope: &ResolvedScope) -> Result<PendingIndexEntry> {
    Ok(PendingIndexEntry {
        journal_file: journal_filename(path)?,
        project_id: scope.project_id.clone(),
        scope_digest: scope.scope_digest.clone(),
    })
}

fn journal_filename(path: &Path) -> Result<String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| contract_error("automation journal filename is invalid"))?
        .to_owned();
    validate_journal_filename(&filename)?;
    Ok(filename)
}

fn validate_journal_filename(filename: &str) -> Result<()> {
    let Some(stem) = filename.strip_suffix(".json") else {
        return Err(contract_error(
            "automation journal filename suffix is invalid",
        ));
    };
    if stem.len() != 64
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(contract_error(
            "automation journal filename digest is invalid",
        ));
    }
    Ok(())
}

fn automation_root(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join("automation_effects")
}

fn index_path(dashboard_root: &Path) -> PathBuf {
    automation_root(dashboard_root).join(INDEX_FILENAME)
}

fn with_index_lock<T>(path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let parent = path
        .parent()
        .ok_or_else(|| contract_error("automation pending index path has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        contract_error(format!(
            "automation pending index directory creation failed: {error}"
        ))
    })?;
    let lock_path = crate::storage::append_lock_path(path);
    let lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path).map_err(|error| {
        contract_error(format!("automation pending index lock failed: {error}"))
    })?;
    let result = operation();
    let unlock = fs2::FileExt::unlock(&lock).map_err(|error| {
        contract_error(format!("automation pending index unlock failed: {error}"))
    });
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn journal_filename_is_exact_digest_only() {
        assert!(validate_journal_filename(&format!("{}.json", "a".repeat(64))).is_ok());
        assert!(validate_journal_filename("../foreign.json").is_err());
        assert!(validate_journal_filename(&format!("{}.json", "A".repeat(64))).is_err());
    }

    #[test]
    fn oversized_pending_index_is_rejected_without_scanning_journals() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = automation_root(temp.path());
        std::fs::create_dir_all(&root).expect("automation root");
        let path = root.join(INDEX_FILENAME);
        let mut file = std::fs::File::create(&path).expect("index");
        file.set_len(MAX_INDEX_BYTES + 1).expect("oversized index");
        file.flush().expect("flush");
        assert!(read_index(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_pending_index_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = automation_root(temp.path());
        std::fs::create_dir_all(&root).expect("automation root");
        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, br#"{"schema_version":1,"entries":[]}"#).expect("outside");
        let path = root.join(INDEX_FILENAME);
        std::os::unix::fs::symlink(&outside, &path).expect("symlink");
        assert!(read_index(&path).is_err());
    }
}
