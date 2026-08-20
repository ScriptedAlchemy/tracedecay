//! Bounded pending-index authority for automatic-effect crash recovery.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use serde::{Deserialize, Serialize};
use tracedecay_application::retained_surfaces::{
    AutomationRunProblemV1, AutomationRunRequestV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceOperation,
};
use tracedecay_application::{
    ApplicationOperation, ApplicationProblemEnvelope, CancellationSignal, CapabilityGrantId,
    DisclosureClass, EffectReceipt, ProblemOwningLayer, RequestId, ResolvedScope,
    retained_surface_application_operation, retained_surface_execution_problem,
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RunId};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, with_owned_temp_publish};
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
    pub(crate) indeterminate: usize,
    pub(crate) already_terminal: usize,
    pub(crate) deferred: usize,
}

pub(crate) async fn reconcile_reserved_automation_effects_for_project(
    memory: &crate::tracedecay::TraceDecay,
    dashboard_root: &Path,
    cancellation: &CancellationSignal,
) -> Result<AutomationEffectRecoveryReport> {
    let repair_root = dashboard_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tracedecay_agent_hosts::automation::run_ledger::repair_corrupt_run_ledger_append_intent_blocking(
            &repair_root,
        )
    })
    .await
    .map_err(|error| {
        contract_error(format!(
            "automation run append-intent repair failed to join: {error}"
        ))
    })??;
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
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .map_err(contract_error)?;
    let mut report = AutomationEffectRecoveryReport::default();
    let transition_root = dashboard_root.to_path_buf();
    let transitions = tokio::task::spawn_blocking(move || {
        indexed_retirement_transitions_blocking(&transition_root)
    })
    .await
    .map_err(|error| {
        contract_error(format!(
            "automation retirement transition reader failed: {error}"
        ))
    })??;
    for transition in transitions {
        if cancellation.is_cancelled() {
            break;
        }
        report.inspected += 1;
        match reconcile_indexed_retirement_transition(
            dashboard_root,
            &owner,
            &scope,
            project_id,
            &operation,
            &transition,
        )
        .await
        {
            Ok(()) => report.already_terminal += 1,
            Err(error) => {
                tracing::warn!(
                    event = "automation_retirement_transition_deferred",
                    journal = %transition.path.display(),
                    error = %error,
                );
                report.deferred += 1;
            }
        }
    }
    let root = dashboard_root.to_path_buf();
    let indexed_scope = scope.clone();
    let indexed =
        tokio::task::spawn_blocking(move || indexed_journals_blocking(&root, &indexed_scope))
            .await
            .map_err(|error| {
                contract_error(format!("automation recovery index reader failed: {error}"))
            })??;
    for indexed in indexed {
        if cancellation.is_cancelled() {
            break;
        }
        report.inspected += 1;
        match reconcile_indexed_automation_effect(
            memory,
            dashboard_root,
            cancellation,
            &owner,
            &scope,
            project_id,
            &operation,
            &indexed,
        )
        .await
        {
            Ok(EntryRecoveryOutcome::PartialEffect) => report.partial_effects += 1,
            Ok(EntryRecoveryOutcome::ResetRequired) => report.reset_required += 1,
            Ok(EntryRecoveryOutcome::Indeterminate) => report.indeterminate += 1,
            Ok(EntryRecoveryOutcome::AlreadyTerminal) => report.already_terminal += 1,
            Ok(EntryRecoveryOutcome::Deferred | EntryRecoveryOutcome::Dormant) => {
                report.deferred += 1;
            }
            Ok(EntryRecoveryOutcome::Cancelled) => break,
            Err(error) => {
                tracing::warn!(
                    event = "automation_effect_recovery_deferred",
                    journal = %indexed.path.display(),
                    error = %error,
                );
                report.deferred += 1;
            }
        }
    }
    if !cancellation.is_cancelled() {
        let retirement_root = dashboard_root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            reject_unbound_retirement_witness_if_index_empty(&retirement_root)
        })
        .await
        .map_err(|error| {
            contract_error(format!(
                "automation retirement witness audit failed to join: {error}"
            ))
        })??;
    }
    Ok(report)
}

pub(super) fn reject_unbound_retirement_witness_if_index_empty(
    dashboard_root: &Path,
) -> Result<()> {
    let path = index_path(dashboard_root);
    with_index_lock(&path, || {
        let index = read_index(&path)?;
        if index.entries.is_empty() && index.retirement_transitions.is_empty() {
            super::retirement::reject_unbound_retirement_witness(dashboard_root)?;
        }
        Ok(())
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryRecoveryOutcome {
    PartialEffect,
    ResetRequired,
    Indeterminate,
    AlreadyTerminal,
    Deferred,
    Dormant,
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_indexed_retirement_transition(
    dashboard_root: &Path,
    owner: &tracedecay_domain::FactOwnerV1,
    scope: &ResolvedScope,
    project_id: &ProjectId,
    operation: &tracedecay_application::ApplicationOperation,
    indexed: &IndexedRetirementTransition,
) -> Result<()> {
    if indexed.project_id != *project_id || indexed.scope_digest != scope.scope_digest {
        return Err(contract_error(
            "automation retirement transition escaped its exact project scope",
        ));
    }
    let path = indexed.path.clone();
    let record = tokio::task::spawn_blocking(move || journal::read_indexed_record_blocking(&path))
        .await
        .map_err(|error| {
            contract_error(format!(
                "automation retirement transition journal reader failed: {error}"
            ))
        })??
        .ok_or_else(|| {
            contract_error("automation retirement transition lost its exact durable journal")
        })?;
    let admission = record.admission().clone();
    let binding = admission.retirement().ok_or_else(|| {
        contract_error("automation retirement transition journal has no source binding")
    })?;
    if !record.is_terminal()
        || record.publication().is_some()
        || admission.schema_version != INDEX_SCHEMA_VERSION
        || !admission.request.validate()
        || admission.memory_owner() != Some(owner)
        || admission.scope != *scope
        || indexed.path.file_name().and_then(|name| name.to_str())
            != Some(&automation_journal_filename(&admission.request.run_id)?)
        || binding.source_digest != indexed.source_digest
        || !admission_has_exact_authority(&admission, operation)?
    {
        return Err(contract_error(
            "automation retirement transition conflicts with its exact journal authority",
        ));
    }
    let binding = binding.clone();
    let terminal_path = indexed.path.clone();
    let terminal = tokio::task::spawn_blocking(move || {
        journal::read_indexed_terminal_blocking(&terminal_path)
    })
    .await
    .map_err(|error| {
        contract_error(format!(
            "automation retirement transition terminal reader failed: {error}"
        ))
    })??
    .ok_or_else(|| contract_error("automation retirement transition lost its terminal sidecar"))?;
    if !terminal.is_retirement_terminal() {
        return Err(contract_error(
            "automation retirement transition journal is not its exact retirement terminal",
        ));
    }

    let root = dashboard_root.to_path_buf();
    let path = indexed.path.clone();
    let capture_expected = indexed.capture_expected;
    tokio::task::spawn_blocking(move || {
        let closure =
            super::retirement::closure_for_durable_transition(&root, &binding, capture_expected)?;
        remove_pending_for_retirement_blocking(&root, &path, &admission, &closure)?;
        super::retirement::complete_after_pending_removal(&closure)?;
        finish_retirement_transition_blocking(&root, &path, &admission, &closure)
    })
    .await
    .map_err(|error| {
        contract_error(format!(
            "automation retirement transition settlement failed to join: {error}"
        ))
    })?
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_indexed_automation_effect(
    memory: &crate::tracedecay::TraceDecay,
    dashboard_root: &Path,
    cancellation: &CancellationSignal,
    owner: &tracedecay_domain::FactOwnerV1,
    scope: &ResolvedScope,
    project_id: &ProjectId,
    operation: &tracedecay_application::ApplicationOperation,
    indexed: &IndexedJournal,
) -> Result<EntryRecoveryOutcome> {
    if indexed.project_id != *project_id || indexed.scope_digest != scope.scope_digest {
        return Ok(EntryRecoveryOutcome::Deferred);
    }
    let path = indexed.path.clone();
    let record = tokio::task::spawn_blocking(move || journal::read_indexed_record_blocking(&path))
        .await
        .map_err(|error| contract_error(format!("automation recovery reader failed: {error}")))??;
    let Some(record) = record else {
        remove_pending_async(dashboard_root, &indexed.path).await?;
        return Ok(EntryRecoveryOutcome::AlreadyTerminal);
    };
    let admission = record.admission().clone();
    if admission.schema_version != INDEX_SCHEMA_VERSION
        || !admission.request.validate()
        || admission
            .memory_owner()
            .is_some_and(|candidate| candidate != owner)
        || admission.scope != *scope
        || indexed.path.file_name().and_then(|name| name.to_str())
            != Some(&automation_journal_filename(&admission.request.run_id)?)
        || !admission_has_exact_authority(&admission, operation)?
    {
        return Ok(EntryRecoveryOutcome::Deferred);
    }
    if record.is_terminal() {
        let terminal_path = indexed.path.clone();
        let terminal = tokio::task::spawn_blocking(move || {
            journal::read_indexed_terminal_blocking(&terminal_path)
        })
        .await
        .map_err(|error| contract_error(format!("automation terminal reader failed: {error}")))??
        .ok_or_else(|| contract_error("terminal journal lost its durable sidecar"))?;
        if let Some(publication) = record.publication() {
            let published =
                tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact(
                    dashboard_root,
                    admission.request.run_id.as_str(),
                    publication,
                )
                .await?;
            if published
                == tracedecay_agent_hosts::automation::run_ledger::ExactRunPublishOutcome::MissingPayload
            {
                return Ok(EntryRecoveryOutcome::Deferred);
            }
            let cleanup_path = indexed.path.clone();
            let cleanup_admission = admission.clone();
            let cleanup_terminal = terminal.clone();
            let cleanup_publication = publication.clone();
            tracedecay_agent_hosts::automation::run_ledger::discard_stale_staged_run_record_exact_after_terminal(
                dashboard_root,
                admission.request.run_id.as_str(),
                publication,
                move || {
                    Ok(journal::classify_durable_settlement_blocking(
                        &cleanup_path,
                        &cleanup_admission,
                        &cleanup_terminal,
                        Some(&cleanup_publication),
                    )?
                    .is_terminal())
                },
            )
            .await?;
        }
        super::finalize_terminal_housekeeping(
            dashboard_root,
            &indexed.path,
            &admission,
            &terminal,
            None,
        )
        .await?;
        return Ok(EntryRecoveryOutcome::AlreadyTerminal);
    }
    if record.prepared().is_some() {
        let prepared_path = indexed.path.clone();
        let terminal = tokio::task::spawn_blocking(move || {
            let record = journal::read_indexed_record_blocking(&prepared_path)?
                .ok_or_else(|| contract_error("prepared journal disappeared during recovery"))?;
            let publication = record
                .prepared()
                .cloned()
                .ok_or_else(|| contract_error("prepared journal changed during recovery"))?;
            let terminal = journal::read_indexed_terminal_blocking(&prepared_path)?
                .ok_or_else(|| contract_error("prepared journal lost its terminal sidecar"))?;
            Ok::<_, crate::errors::TraceDecayError>((terminal, publication))
        })
        .await
        .map_err(|error| contract_error(format!("prepared terminal reader failed: {error}")))??;
        let (terminal, publication) = terminal;
        if admission.retirement().is_some() || terminal.is_retirement_terminal() {
            return Ok(EntryRecoveryOutcome::Deferred);
        }
        let published =
            tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact(
                dashboard_root,
                admission.request.run_id.as_str(),
                &publication,
            )
            .await?;
        if published
            == tracedecay_agent_hosts::automation::run_ledger::ExactRunPublishOutcome::MissingPayload
        {
            return Ok(EntryRecoveryOutcome::Deferred);
        }
        let path = indexed.path.clone();
        let requested = admission.clone();
        let publication = publication.clone();
        let cleanup_publication = publication.clone();
        let cleanup_terminal = terminal.clone();
        tokio::task::spawn_blocking(move || {
            journal::promote_prepared_terminal_blocking(&path, &requested, terminal, &publication)
        })
        .await
        .map_err(|error| contract_error(format!("automation recovery writer failed: {error}")))??;
        let terminal_path = indexed.path.clone();
        let terminal_admission = admission.clone();
        let terminal_publication = cleanup_publication.clone();
        tracedecay_agent_hosts::automation::run_ledger::discard_stale_staged_run_record_exact_after_terminal(
            dashboard_root,
            admission.request.run_id.as_str(),
            &cleanup_publication,
            move || {
                Ok(journal::classify_durable_settlement_blocking(
                    &terminal_path,
                    &terminal_admission,
                    &cleanup_terminal,
                    Some(&terminal_publication),
                )?
                .is_terminal())
            },
        )
        .await?;
        remove_pending_async(dashboard_root, &indexed.path).await?;
        return Ok(EntryRecoveryOutcome::AlreadyTerminal);
    }
    let cleanup_path = indexed.path.clone();
    let cleanup_admission = admission.clone();
    let discarded =
        tracedecay_agent_hosts::automation::run_ledger::discard_unbound_staged_run_records_if(
            dashboard_root,
            admission.request.run_id.as_str(),
            move || {
                journal::unbound_reserved_cleanup_is_safe_blocking(
                    &cleanup_path,
                    &cleanup_admission,
                )
            },
        )
        .await?;
    if discarded
        == tracedecay_agent_hosts::automation::run_ledger::ExactRunUnboundDiscardOutcome::Retained
    {
        return Ok(EntryRecoveryOutcome::Deferred);
    }
    if admission.is_external() {
        let terminal = AutomationSettledTerminal::Problem(admission.recovery_problem().clone());
        return persist_reserved_recovery(
            dashboard_root,
            cancellation,
            indexed,
            admission,
            terminal,
            EntryRecoveryOutcome::Indeterminate,
        )
        .await;
    }
    let read_cancellation = cancellation.clone();
    let read_control = FactReadControl::new(Arc::new(move || read_cancellation.is_cancelled()));
    let recovered = memory
        .project_memory_application()
        .await?
        .project_memory_automation_run_receipts(admission.request.run_id.clone(), &read_control)
        .await
        .map_err(|error| {
            contract_error(format!(
                "canonical memory automation receipt recovery failed: {error}"
            ))
        })?;
    if cancellation.is_cancelled() {
        return Ok(EntryRecoveryOutcome::Cancelled);
    }
    let committed = project_recovered_committed_receipts(&admission.request, &recovered)?;
    if let Some(reason) = special_recovery_defer_reason(&admission, committed.is_empty()) {
        tracing::warn!(
            event = "automation_effect_recovery_dormant",
            journal = %indexed.path.display(),
            reason,
        );
        // The exact run-id admission remains on disk and a direct retry will
        // re-index it before exact finalization. Removing only the pending
        // index entry releases bounded recovery capacity without fabricating a
        // retirement/reset terminal or repeating a possibly executed effect.
        remove_pending_async(dashboard_root, &indexed.path).await?;
        return Ok(EntryRecoveryOutcome::Dormant);
    }
    if admission.retirement().is_some() && !committed.is_empty() {
        return Err(contract_error(
            "proposal retirement recovery found unrelated canonical memory commits",
        ));
    }
    let outcome = if recovered.is_empty() {
        EntryRecoveryOutcome::ResetRequired
    } else {
        EntryRecoveryOutcome::PartialEffect
    };
    let terminal = if committed.is_empty() {
        AutomationSettledTerminal::Problem(admission.recovery_problem().clone())
    } else {
        recovered_partial_terminal(&admission, committed, operation)?
    };
    persist_reserved_recovery(
        dashboard_root,
        cancellation,
        indexed,
        admission,
        terminal,
        outcome,
    )
    .await
}

async fn persist_reserved_recovery(
    dashboard_root: &Path,
    cancellation: &CancellationSignal,
    indexed: &IndexedJournal,
    admission: DurableAutomationAdmission,
    terminal: AutomationSettledTerminal,
    outcome: EntryRecoveryOutcome,
) -> Result<EntryRecoveryOutcome> {
    let path = indexed.path.clone();
    let root = dashboard_root.to_path_buf();
    let write_cancellation = cancellation.clone();
    let committed = tokio::task::spawn_blocking(move || {
        let Some(_) = journal::persist_recovered_terminal_blocking(
            &path,
            &admission,
            terminal,
            Some(&write_cancellation),
        )?
        else {
            return Ok::<bool, crate::errors::TraceDecayError>(false);
        };
        remove_pending_blocking(&root, &path)?;
        Ok(true)
    })
    .await
    .map_err(|error| contract_error(format!("automation recovery writer failed: {error}")))??;
    Ok(if committed {
        outcome
    } else {
        EntryRecoveryOutcome::Cancelled
    })
}

pub(super) fn special_recovery_defer_reason(
    admission: &DurableAutomationAdmission,
    committed_receipts_empty: bool,
) -> Option<&'static str> {
    if committed_receipts_empty && admission.retirement().is_some() {
        Some("retirement_requires_exact_finalization")
    } else if committed_receipts_empty && admission.reset_source_digest().is_some() {
        Some("shipped_proposals_require_exact_reset_diagnostic")
    } else {
        None
    }
}

pub(super) fn admission_has_exact_authority(
    admission: &DurableAutomationAdmission,
    operation: &tracedecay_application::ApplicationOperation,
) -> Result<bool> {
    Ok(effect_authority_digest(
        admission.schema_version,
        operation,
        &admission.request,
        &admission.input_digest,
        &admission.configuration_digest,
        &admission.grant_id,
        admission.grant_revision,
        &admission.grant_digest,
        &admission.disclosure,
        &admission.effect_receipt_template,
        &admission.actor,
        &admission.scope,
        &admission.request_id,
        &admission.recovery,
    )? == admission.effect_authority_digest)
}

#[derive(Serialize)]
struct EffectAuthorityDigestInput<'a> {
    domain: &'static str,
    schema_version: u32,
    capability_id: &'a tracedecay_tool_catalog::CapabilityId,
    use_case_id: &'a tracedecay_tool_catalog::UseCaseId,
    result_contract: &'a tracedecay_application::ResultContractRef,
    resource_addressed: bool,
    request: &'a AutomationRunRequestV1,
    input_digest: &'a ManifestDigest,
    configuration_digest: &'a ManifestDigest,
    grant_id: &'a CapabilityGrantId,
    grant_revision: u64,
    grant_digest: &'a ManifestDigest,
    disclosure: &'a DisclosureClass,
    effect_receipt_template: &'a EffectReceipt,
    actor: &'a ActorId,
    scope: &'a ResolvedScope,
    request_id: &'a RequestId,
    recovery: &'a journal::AutomationRecoveryBinding,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn effect_authority_digest(
    schema_version: u32,
    operation: &ApplicationOperation,
    request: &AutomationRunRequestV1,
    input_digest: &ManifestDigest,
    configuration_digest: &ManifestDigest,
    grant_id: &CapabilityGrantId,
    grant_revision: u64,
    grant_digest: &ManifestDigest,
    disclosure: &DisclosureClass,
    effect_receipt_template: &EffectReceipt,
    actor: &ActorId,
    scope: &ResolvedScope,
    request_id: &RequestId,
    recovery: &journal::AutomationRecoveryBinding,
) -> Result<ManifestDigest> {
    digest(&EffectAuthorityDigestInput {
        domain: "tracedecay.automation-run.effect-authority.v1",
        schema_version,
        capability_id: operation.capability_id(),
        use_case_id: operation.use_case_id(),
        result_contract: operation.result_contract(),
        resource_addressed: operation.resource_addressed(),
        request,
        input_digest,
        configuration_digest,
        grant_id,
        grant_revision,
        grant_digest,
        disclosure,
        effect_receipt_template,
        actor,
        scope,
        request_id,
        recovery,
    })
}

pub(super) fn recovered_partial_terminal(
    admission: &DurableAutomationAdmission,
    committed: Vec<tracedecay_application::retained_surfaces::AutomationCommittedReceiptV1>,
    operation: &tracedecay_application::ApplicationOperation,
) -> Result<AutomationSettledTerminal> {
    let state = digest(&(
        "tracedecay.automation-run.partial-state.v1",
        admission.request.run_id.as_str(),
        &committed,
    ))?;
    let mut receipt = admission.effect_receipt_template.clone();
    receipt.committed_state = Some(state);
    let problem =
        retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: "application.automation-run.recovered-partial-effect".to_owned(),
            committed_receipt: Box::new(receipt),
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
        AutomationRunProblemV1::new(
            &admission.request,
            admission.scope.clone(),
            envelope,
            committed,
            &admission.request_id,
        )
        .map_err(contract_error)?,
    ))
}

fn automation_journal_filename(run_id: &RunId) -> Result<String> {
    let key = digest(&("tracedecay.automation-run.terminal-key.v1", run_id))?;
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
struct PendingRetirementTransition {
    journal_file: String,
    project_id: ProjectId,
    scope_digest: ManifestDigest,
    source_digest: String,
    capture_expected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingIndex {
    schema_version: u32,
    entries: Vec<PendingIndexEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retirement_transitions: Vec<PendingRetirementTransition>,
}

pub(super) struct IndexedJournal {
    pub(super) path: PathBuf,
    pub(super) project_id: ProjectId,
    pub(super) scope_digest: ManifestDigest,
}

#[derive(Clone)]
struct IndexedRetirementTransition {
    path: PathBuf,
    project_id: ProjectId,
    scope_digest: ManifestDigest,
    source_digest: String,
    capture_expected: bool,
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

pub(super) fn remove_pending_for_retirement_blocking(
    dashboard_root: &Path,
    journal_path: &Path,
    admission: &DurableAutomationAdmission,
    closure: &super::retirement::RetirementClosure,
) -> Result<()> {
    remove_pending_for_retirement_with_writer(
        dashboard_root,
        journal_path,
        admission,
        closure,
        write_pending_index,
    )
}

fn remove_pending_for_retirement_with_writer(
    dashboard_root: &Path,
    journal_path: &Path,
    admission: &DurableAutomationAdmission,
    closure: &super::retirement::RetirementClosure,
    mut write_index: impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let expected = entry_for(journal_path, &admission.scope)?;
    let transition = retirement_transition_for(&expected, admission, closure)?;
    let path = index_path(dashboard_root);
    with_index_lock(&path, || {
        let original = read_index(&path)?;
        if original.entries.iter().any(|candidate| {
            candidate.journal_file == expected.journal_file && candidate != &expected
        }) {
            return Err(contract_error(
                "automation pending index journal identity conflicts with its project binding",
            ));
        }
        if original.retirement_transitions.iter().any(|candidate| {
            candidate.journal_file == transition.journal_file && candidate != &transition
        }) {
            return Err(contract_error(
                "automation retirement transition conflicts with its durable journal binding",
            ));
        }
        if !original.entries.contains(&expected)
            && !original.retirement_transitions.contains(&transition)
        {
            return Ok(());
        }
        let protected = publish_retirement_transition_with_writer(
            &path,
            &original,
            &transition,
            &mut write_index,
        )?;
        if !protected.entries.contains(&expected) {
            return Ok(());
        }
        remove_pending_after_transition_with_writer(&path, &protected, &expected, &mut write_index)
    })
}

fn publish_retirement_transition_with_writer(
    path: &Path,
    original: &PendingIndex,
    transition: &PendingRetirementTransition,
    write_index: &mut impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<PendingIndex> {
    if original.retirement_transitions.iter().any(|candidate| {
        candidate.journal_file == transition.journal_file && candidate != transition
    }) {
        return Err(contract_error(
            "automation retirement transition conflicts with its durable journal binding",
        ));
    }
    if original.retirement_transitions.contains(transition) {
        return Ok(original.clone());
    }
    if original.retirement_transitions.len() >= MAX_PENDING_AUTOMATION_EFFECTS {
        return Err(contract_error(
            "automation retirement transition index reached its bounded capacity",
        ));
    }
    let mut protected = original.clone();
    protected.retirement_transitions.push(transition.clone());
    protected
        .retirement_transitions
        .sort_by(|left, right| left.journal_file.cmp(&right.journal_file));
    if let Err(error) = publish_index_state(path, &protected, write_index) {
        match read_index(path) {
            Ok(visible) if visible == protected => {}
            Ok(visible) if visible == *original => {
                return Err(contract_error(format!(
                    "automation retirement transition publication failed before pending removal: {error}"
                )));
            }
            Ok(_) => {
                return Err(contract_error(format!(
                    "automation retirement transition publication left an unrecognized durable index state: {error}"
                )));
            }
            Err(read_error) => {
                return Err(contract_error(format!(
                    "automation retirement transition publication is uncertain: {error}; readback failed: {read_error}"
                )));
            }
        }
    }
    Ok(protected)
}

fn remove_pending_after_transition_with_writer(
    path: &Path,
    protected: &PendingIndex,
    expected: &PendingIndexEntry,
    write_index: &mut impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let mut removed = protected.clone();
    removed.entries.retain(|candidate| candidate != expected);
    let removed_bytes = encode_pending_index(&removed)?;
    match write_index(path, &removed_bytes) {
        Ok(()) => require_index_state(path, &removed),
        Err(removal_error) => match read_index(path) {
            Ok(visible) if visible == removed => Ok(()),
            Ok(visible) if visible == *protected => Err(contract_error(format!(
                "automation pending removal retained its marker-protected prior state: {removal_error}"
            ))),
            Ok(_) => Err(contract_error(format!(
                "automation pending removal left an unrecognized marker-protected index state: {removal_error}"
            ))),
            Err(read_error) => Err(contract_error(format!(
                "automation pending removal is uncertain while its transition marker remains required: {removal_error}; readback failed: {read_error}"
            ))),
        },
    }
}

pub(super) fn finish_retirement_transition_blocking(
    dashboard_root: &Path,
    journal_path: &Path,
    admission: &DurableAutomationAdmission,
    closure: &super::retirement::RetirementClosure,
) -> Result<()> {
    let entry = entry_for(journal_path, &admission.scope)?;
    let transition = retirement_transition_for(&entry, admission, closure)?;
    let path = index_path(dashboard_root);
    with_index_lock(&path, || {
        let original = read_index(&path)?;
        finish_retirement_transition_with_writer(
            &path,
            &original,
            &transition,
            &mut write_pending_index,
        )
    })
}

fn finish_retirement_transition_with_writer(
    path: &Path,
    original: &PendingIndex,
    transition: &PendingRetirementTransition,
    write_index: &mut impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    if !original.retirement_transitions.contains(transition) {
        return Ok(());
    }
    let mut completed = original.clone();
    completed
        .retirement_transitions
        .retain(|candidate| candidate != transition);
    match publish_index_state(path, &completed, write_index) {
        Ok(()) => Ok(()),
        Err(error) => match read_index(path) {
            Ok(visible) if visible == completed => Ok(()),
            Ok(visible) if visible == *original => Err(error),
            Ok(_) => Err(contract_error(format!(
                "automation retirement transition removal left an unrecognized durable index state: {error}"
            ))),
            Err(read_error) => Err(contract_error(format!(
                "automation retirement transition removal is uncertain: {error}; readback failed: {read_error}"
            ))),
        },
    }
}

fn retirement_transition_for(
    entry: &PendingIndexEntry,
    admission: &DurableAutomationAdmission,
    closure: &super::retirement::RetirementClosure,
) -> Result<PendingRetirementTransition> {
    let binding = admission.retirement().ok_or_else(|| {
        contract_error("automation retirement transition has no durable admission binding")
    })?;
    if binding.source_digest != closure.source_digest() {
        return Err(contract_error(
            "automation retirement transition conflicts with its source digest",
        ));
    }
    Ok(PendingRetirementTransition {
        journal_file: entry.journal_file.clone(),
        project_id: entry.project_id.clone(),
        scope_digest: entry.scope_digest.clone(),
        source_digest: binding.source_digest.clone(),
        capture_expected: closure.capture_expected(),
    })
}

fn publish_index_state(
    path: &Path,
    expected: &PendingIndex,
    write_index: &mut impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let bytes = encode_pending_index(expected)?;
    write_index(path, &bytes)?;
    require_index_state(path, expected)
}

fn require_index_state(path: &Path, expected: &PendingIndex) -> Result<()> {
    if read_index(path)? == *expected {
        Ok(())
    } else {
        Err(contract_error(
            "automation pending recovery index did not replay its exact expected state",
        ))
    }
}

fn encode_pending_index(index: &PendingIndex) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(index).map_err(contract_error)?;
    if bytes.len() > MAX_INDEX_BYTES as usize {
        return Err(contract_error(
            "automation pending recovery index exceeds its byte bound",
        ));
    }
    Ok(bytes)
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

fn indexed_retirement_transitions_blocking(
    dashboard_root: &Path,
) -> Result<Vec<IndexedRetirementTransition>> {
    let index_path = index_path(dashboard_root);
    with_index_lock(&index_path, || {
        let index = read_index(&index_path)?;
        let automation_root = automation_root(dashboard_root);
        Ok(index
            .retirement_transitions
            .into_iter()
            .map(|transition| IndexedRetirementTransition {
                path: automation_root.join(&transition.journal_file),
                project_id: transition.project_id,
                scope_digest: transition.scope_digest,
                source_digest: transition.source_digest,
                capture_expected: transition.capture_expected,
            })
            .collect())
    })
}

fn mutate_index(
    dashboard_root: &Path,
    mutate: impl FnOnce(&mut PendingIndex) -> Result<()>,
) -> Result<()> {
    mutate_index_with_writer(dashboard_root, mutate, write_pending_index)
}

fn mutate_index_with_writer(
    dashboard_root: &Path,
    mutate: impl FnOnce(&mut PendingIndex) -> Result<()>,
    write_index: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let path = index_path(dashboard_root);
    with_index_lock(&path, || {
        let mut index = read_index(&path)?;
        mutate(&mut index)?;
        let bytes = encode_pending_index(&index)?;
        write_index(&path, &bytes)
    })
}

fn write_pending_index(path: &Path, bytes: &[u8]) -> Result<()> {
    write_pending_index_with_publisher(path, bytes, |temporary, destination| {
        super::journal::replace_automation_file_atomically(
            temporary,
            destination,
            "automation pending recovery index",
        )
    })
}

pub(super) fn write_pending_index_with_publisher(
    path: &Path,
    bytes: &[u8],
    publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let expected: PendingIndex = serde_json::from_slice(bytes).map_err(contract_error)?;
    with_owned_temp_publish(
        path,
        "automation-run-pending-index",
        publish,
        |output| output.write_all(bytes),
        DirectorySyncPolicy::Strict,
    )
    .map_err(|error| contract_error(format!("automation pending index write failed: {error}")))?;
    if read_index(path)? != expected {
        return Err(contract_error(
            "automation pending index replacement did not replay exactly",
        ));
    }
    Ok(())
}

fn read_index(path: &Path) -> Result<PendingIndex> {
    crate::storage::reject_symlink_components(path, "automation pending index").map_err(
        |error| contract_error(format!("automation pending index path failed: {error}")),
    )?;
    let parent = path
        .parent()
        .ok_or_else(|| contract_error("automation pending index path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| contract_error("automation pending index path has no filename"))?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority()).map_err(|error| {
        contract_error(format!(
            "automation pending index directory open failed: {error}"
        ))
    })?;
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match directory.open_with(name, &options) {
        Ok(file) => file.into_std(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingIndex {
                schema_version: INDEX_SCHEMA_VERSION,
                entries: Vec::new(),
                retirement_transitions: Vec::new(),
            });
        }
        Err(error) => {
            return Err(contract_error(format!(
                "automation pending index open failed: {error}"
            )));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        contract_error(format!(
            "automation pending index metadata read failed: {error}"
        ))
    })?;
    if !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES {
        return Err(contract_error(
            "automation pending index is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            contract_error(format!("automation pending index read failed: {error}"))
        })?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(contract_error(
            "automation pending index grew beyond its durable byte bound",
        ));
    }
    let index: PendingIndex = serde_json::from_slice(&bytes).map_err(contract_error)?;
    if index.schema_version != INDEX_SCHEMA_VERSION
        || index.entries.len() > MAX_PENDING_AUTOMATION_EFFECTS
        || index.retirement_transitions.len() > MAX_PENDING_AUTOMATION_EFFECTS
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
    for transition in &index.retirement_transitions {
        validate_journal_filename(&transition.journal_file)?;
        transition.project_id.validate().map_err(contract_error)?;
        transition.scope_digest.validate().map_err(contract_error)?;
        validate_sha256_digest(&transition.source_digest)?;
    }
    Ok(index)
}

fn validate_sha256_digest(digest: &str) -> Result<()> {
    let Some(body) = digest.strip_prefix("sha256:") else {
        return Err(contract_error(
            "automation retirement transition digest prefix is invalid",
        ));
    };
    if body.len() != 64
        || !body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(contract_error(
            "automation retirement transition digest is invalid",
        ));
    }
    Ok(())
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
    crate::storage::PrivateStoreIo::create_dir_all_durable(parent).map_err(|error| {
        contract_error(format!(
            "automation pending index directory creation failed: {error}"
        ))
    })?;
    let lock_path = crate::storage::append_lock_path(path);
    crate::storage::reject_symlink_components(&lock_path, "automation pending index lock")
        .map_err(|error| {
            contract_error(format!(
                "automation pending index lock path failed: {error}"
            ))
        })?;
    let lock_parent = lock_path
        .parent()
        .ok_or_else(|| contract_error("automation pending index lock has no parent"))?;
    let lock_name = lock_path
        .file_name()
        .ok_or_else(|| contract_error("automation pending index lock has no filename"))?;
    let lock_directory =
        Dir::open_ambient_dir(lock_parent, ambient_authority()).map_err(|error| {
            contract_error(format!(
                "automation pending index lock directory failed: {error}"
            ))
        })?;
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let lock = lock_directory
        .open_with(lock_name, &options)
        .map(cap_std::fs::File::into_std)
        .and_then(|file| {
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "automation pending index lock is not a regular file",
                ));
            }
            fs2::FileExt::lock_exclusive(&file)?;
            Ok(file)
        })
        .map_err(|error| {
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

    #[cfg(unix)]
    #[test]
    fn symlink_pending_index_lock_is_rejected_without_touching_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = automation_root(temp.path());
        std::fs::create_dir_all(&root).expect("automation root");
        let path = root.join(INDEX_FILENAME);
        let outside = temp.path().join("outside.lock");
        std::fs::write(&outside, b"outside").expect("outside lock");
        std::os::unix::fs::symlink(&outside, crate::storage::append_lock_path(&path))
            .expect("symlink lock");

        assert!(with_index_lock(&path, || Ok(())).is_err());
        assert_eq!(
            std::fs::read(&outside).expect("outside preserved"),
            b"outside"
        );
    }

    #[test]
    fn visible_index_replace_error_retries_add_and_remove_idempotently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let entry = PendingIndexEntry {
            journal_file: format!("{}.json", "a".repeat(64)),
            project_id: ProjectId::new("project.pending-index-uncertainty").expect("project"),
            scope_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("scope digest"),
        };
        let add_entry = entry.clone();
        let add_error = mutate_index_with_writer(
            temp.path(),
            move |index| {
                index.entries.push(add_entry);
                Ok(())
            },
            |path, bytes| {
                write_pending_index_with_publisher(path, bytes, |temporary, destination| {
                    super::journal::replace_automation_file_atomically(
                        temporary,
                        destination,
                        "automation pending recovery index",
                    )?;
                    Err(std::io::Error::other(
                        "injected error after visible pending-index replacement",
                    ))
                })
            },
        )
        .expect_err("visible add uncertainty must surface");
        assert!(add_error.to_string().contains("visible pending-index"));
        assert_eq!(
            read_index(&index_path(temp.path()))
                .expect("visible add")
                .entries,
            vec![entry.clone()]
        );

        let retry_entry = entry.clone();
        mutate_index(temp.path(), move |index| {
            if !index.entries.contains(&retry_entry) {
                index.entries.push(retry_entry);
            }
            Ok(())
        })
        .expect("idempotent add retry");
        assert_eq!(
            read_index(&index_path(temp.path()))
                .expect("durable add")
                .entries,
            vec![entry.clone()]
        );

        let remove_entry = entry.clone();
        let remove_error = mutate_index_with_writer(
            temp.path(),
            move |index| {
                index.entries.retain(|candidate| candidate != &remove_entry);
                Ok(())
            },
            |path, bytes| {
                write_pending_index_with_publisher(path, bytes, |temporary, destination| {
                    super::journal::replace_automation_file_atomically(
                        temporary,
                        destination,
                        "automation pending recovery index",
                    )?;
                    Err(std::io::Error::other(
                        "injected error after visible pending-index removal",
                    ))
                })
            },
        )
        .expect_err("visible removal uncertainty must surface");
        assert!(remove_error.to_string().contains("visible pending-index"));
        assert!(
            read_index(&index_path(temp.path()))
                .expect("visible removal")
                .entries
                .is_empty()
        );

        mutate_index(temp.path(), |index| {
            index.entries.retain(|candidate| candidate != &entry);
            Ok(())
        })
        .expect("idempotent removal retry");
        assert!(
            read_index(&index_path(temp.path()))
                .expect("durable removal")
                .entries
                .is_empty()
        );
    }

    fn pending_entry(label: char) -> PendingIndexEntry {
        let scope_label = match label {
            'a' => 'e',
            'b' => 'f',
            _ => label,
        };
        PendingIndexEntry {
            journal_file: format!("{}.json", label.to_string().repeat(64)),
            project_id: ProjectId::new(format!("project.retirement-{label}")).expect("project"),
            scope_digest: ManifestDigest::new(format!(
                "sha256:{}",
                scope_label.to_string().repeat(64)
            ))
            .expect("scope digest"),
        }
    }

    fn retirement_transition(
        entry: &PendingIndexEntry,
        digest_label: char,
    ) -> PendingRetirementTransition {
        PendingRetirementTransition {
            journal_file: entry.journal_file.clone(),
            project_id: entry.project_id.clone(),
            scope_digest: entry.scope_digest.clone(),
            source_digest: format!("sha256:{}", digest_label.to_string().repeat(64)),
            capture_expected: true,
        }
    }

    fn pending_index(
        entries: Vec<PendingIndexEntry>,
        retirement_transitions: Vec<PendingRetirementTransition>,
    ) -> PendingIndex {
        PendingIndex {
            schema_version: INDEX_SCHEMA_VERSION,
            entries,
            retirement_transitions,
        }
    }

    #[test]
    fn retirement_handoff_resolves_visible_write_uncertainty_and_preserves_other_transitions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = index_path(temp.path());
        std::fs::create_dir_all(path.parent().expect("pending index parent"))
            .expect("create automation root");
        let entry_a = pending_entry('a');
        let entry_b = pending_entry('b');
        let transition_a = retirement_transition(&entry_a, 'c');
        let transition_b = retirement_transition(&entry_b, 'd');
        let original = pending_index(
            vec![entry_a.clone(), entry_b.clone()],
            vec![transition_b.clone()],
        );
        write_pending_index(
            &path,
            &encode_pending_index(&original).expect("original bytes"),
        )
        .expect("original index");

        let protected = publish_retirement_transition_with_writer(
            &path,
            &original,
            &transition_a,
            &mut |path, bytes| {
                write_pending_index(path, bytes)?;
                Err(contract_error(
                    "injected error after visible transition publication",
                ))
            },
        )
        .expect("visible marker publication is resolved by exact readback");
        assert_eq!(
            protected.retirement_transitions,
            vec![transition_a.clone(), transition_b.clone()]
        );
        assert_eq!(
            indexed_retirement_transitions_blocking(temp.path())
                .expect("bounded transition inventory")
                .len(),
            2
        );

        remove_pending_after_transition_with_writer(
            &path,
            &protected,
            &entry_a,
            &mut |path, bytes| {
                write_pending_index(path, bytes)?;
                Err(contract_error(
                    "injected error after visible pending removal",
                ))
            },
        )
        .expect("visible pending removal is resolved by exact readback");
        let marker_only = read_index(&path).expect("marker-only index");
        assert_eq!(marker_only.entries, vec![entry_b]);
        assert_eq!(
            marker_only.retirement_transitions,
            vec![transition_a.clone(), transition_b.clone()]
        );

        finish_retirement_transition_with_writer(
            &path,
            &marker_only,
            &transition_a,
            &mut |path, bytes| {
                write_pending_index(path, bytes)?;
                Err(contract_error(
                    "injected error after visible transition removal",
                ))
            },
        )
        .expect("visible marker removal is resolved by exact readback");
        let completed = read_index(&path).expect("completed index");
        assert_eq!(completed.retirement_transitions, vec![transition_b]);
    }

    #[test]
    fn retirement_handoff_preserves_prepublication_state_and_rejects_mismatched_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = index_path(temp.path());
        std::fs::create_dir_all(path.parent().expect("pending index parent"))
            .expect("create automation root");
        let entry = pending_entry('a');
        let transition = retirement_transition(&entry, 'c');
        let original = pending_index(vec![entry.clone()], Vec::new());
        write_pending_index(
            &path,
            &encode_pending_index(&original).expect("original bytes"),
        )
        .expect("original index");

        let error = publish_retirement_transition_with_writer(
            &path,
            &original,
            &transition,
            &mut |_path, _bytes| Err(contract_error("injected publication failure")),
        )
        .expect_err("an invisible marker publication must not authorize pending removal");
        assert!(error.to_string().contains("before pending removal"));
        assert_eq!(read_index(&path).expect("unchanged index"), original);

        let conflicting = retirement_transition(&entry, 'd');
        let mismatched = pending_index(vec![entry], vec![conflicting]);
        let error = publish_retirement_transition_with_writer(
            &path,
            &mismatched,
            &transition,
            &mut |_path, _bytes| panic!("mismatched marker must not be overwritten"),
        )
        .expect_err("a marker with a foreign source digest must fail closed");
        assert!(
            error
                .to_string()
                .contains("conflicts with its durable journal binding")
        );
    }
}
