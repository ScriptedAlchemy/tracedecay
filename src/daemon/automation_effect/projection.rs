//! Projection from automation/store authority receipts into the application terminal.

use tracedecay_agent_hosts::automation::AutomationCommittedReceipt;
use tracedecay_application::retained_surfaces::{
    AutomationCommittedReceiptV1, AutomationExternalEffectReceiptV1, AutomationRunRequestV1,
    AutomationRunSummaryV1, AutomationSkipReasonV1, AutomationTaskRequestV1, AutomationTaskV1,
    FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1,
    MemoryAutomationCurationAddDispositionV1, MemoryAutomationCurationLinkDispositionV1,
    MemoryAutomationCurationMergeV1, MemoryAutomationCurationOperationEffectV1,
    MemoryAutomationCurationReceiptV1, MemoryAutomationCurationRelationKindV1,
    MemoryAutomationCurationRelationProvenanceV1, MemoryAutomationCurationRelationV1,
    MemoryAutomationCurationRemoveDispositionV1, MemoryAutomationCurationResultV1,
    MemoryAutomationFactDispositionV1, MemoryAutomationFactEffectV1,
    MemoryAutomationFactEvidenceV1, MemoryAutomationFactInputDigestV1,
    MemoryAutomationFactReceiptV1, MemoryAutomationFactRequestV1, MemoryAutomationFactStateV1,
    MemoryAutomationFactTargetV1,
};
use tracedecay_domain::{FactCategoryV1, FactOwnerV1, FactRelationKindV1, RunId, canonical_sha256};
use tracedecay_store::{
    ProjectMemoryAutomaticFactApplyDispositionV1, ProjectMemoryAutomaticFactApplyResultV1,
    ProjectMemoryAutomaticFactStateV1, ProjectMemoryAutomationRunReceiptsV1,
    ProjectMemoryFactAddDispositionV1, ProjectMemoryFactCurationLinkDispositionV1,
    ProjectMemoryFactCurationOperationEffectV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactCurationRemoveDispositionV1,
};

use super::contract_error;
use crate::errors::Result;

pub(super) fn project_run_summary(
    task: AutomationTaskV1,
    receipts: &[AutomationCommittedReceiptV1],
) -> Result<AutomationRunSummaryV1> {
    let (accepted_count, rejected_count) = match task {
        AutomationTaskV1::MemoryCurator => {
            let mut accepted = 0_u64;
            for receipt in receipts {
                let AutomationCommittedReceiptV1::Curation(receipt) = receipt else {
                    return Err(contract_error(
                        "memory curator summary received a non-curation receipt",
                    ));
                };
                accepted = accepted
                    .checked_add(receipt.receipt.accepted_operations)
                    .ok_or_else(|| contract_error("memory curator summary overflowed"))?;
            }
            (accepted, 0)
        }
        AutomationTaskV1::SessionReflector => {
            let mut accepted = 0_u64;
            let mut rejected = 0_u64;
            for receipt in receipts {
                let AutomationCommittedReceiptV1::AutomaticFact(receipt) = receipt else {
                    return Err(contract_error(
                        "session reflector summary received a non-fact receipt",
                    ));
                };
                match receipt.state {
                    MemoryAutomationFactStateV1::Applied => accepted += 1,
                    MemoryAutomationFactStateV1::Quarantined => rejected += 1,
                }
            }
            (accepted, rejected)
        }
        AutomationTaskV1::SkillWriter | AutomationTaskV1::UserJob => {
            if receipts.len() > 1 {
                return Err(contract_error(
                    "external automation terminal carried more than one committed effect",
                ));
            }
            let accepted = u64::from(!receipts.is_empty());
            (accepted, 0)
        }
        AutomationTaskV1::CombinedReview => {
            let (accepted, rejected) =
                receipts
                    .iter()
                    .try_fold((0_u64, 0_u64), |(accepted, rejected), receipt| {
                        let (accepted_delta, rejected_delta) = match receipt {
                            AutomationCommittedReceiptV1::AutomaticFact(receipt) => {
                                match receipt.state {
                                    MemoryAutomationFactStateV1::Applied => (1, 0),
                                    MemoryAutomationFactStateV1::Quarantined => (0, 1),
                                }
                            }
                            AutomationCommittedReceiptV1::SkillWriting(_) => (1, 0),
                            _ => {
                                return Err(contract_error(
                                    "combined automation terminal carried an unrelated receipt",
                                ));
                            }
                        };
                        Ok((
                            accepted.checked_add(accepted_delta).ok_or_else(|| {
                                contract_error("combined automation accepted summary overflowed")
                            })?,
                            rejected.checked_add(rejected_delta).ok_or_else(|| {
                                contract_error("combined automation rejected summary overflowed")
                            })?,
                        ))
                    })?;
            (accepted, rejected)
        }
    };
    Ok(AutomationRunSummaryV1 {
        reviewed_count: accepted_count
            .checked_add(rejected_count)
            .ok_or_else(|| contract_error("memory automation summary overflowed"))?,
        accepted_count,
        rejected_count,
        skipped_count: 0,
    })
}

pub(super) fn project_skip_reason(reason: &str) -> Result<AutomationSkipReasonV1> {
    AutomationSkipReasonV1::from_ledger_reason(reason).ok_or_else(|| {
        contract_error(format!(
            "automation skip reason is not registered: {reason}"
        ))
    })
}

pub(super) fn project_committed_receipts(
    request: &AutomationRunRequestV1,
    committed: &AutomationCommittedReceipt,
) -> Result<Vec<AutomationCommittedReceiptV1>> {
    let outer_run_id = &request.run_id;
    match committed {
        AutomationCommittedReceipt::MemoryCuration(receipt) => {
            if receipt.operation_effects().is_empty() {
                return Err(contract_error(
                    "memory curation authority receipt has no committed effects",
                ));
            }
            if receipt.automation_run_id() != Some(outer_run_id) {
                return Err(contract_error(
                    "memory curation receipt is not bound to its admitted outer run",
                ));
            }
            let public_receipt = project_curation_receipt(receipt, receipt.replayed())?;
            let canonical_digest = canonical_sha256(&(
                "tracedecay.automation-run.curation-receipt.v1",
                &public_receipt,
            ))
            .map_err(contract_error)?;
            let receipt = MemoryAutomationCurationReceiptV1 {
                receipt: public_receipt,
                canonical_digest,
            };
            Ok(vec![AutomationCommittedReceiptV1::Curation(receipt)])
        }
        AutomationCommittedReceipt::AutomaticFacts(receipts) => receipts
            .iter()
            .map(|result| {
                if result.receipt().automation_run_id() != Some(outer_run_id.as_str()) {
                    return Err(contract_error(
                        "automatic fact receipt is not bound to its admitted outer run",
                    ));
                }
                project_automatic_fact_receipt(result)
                    .map(AutomationCommittedReceiptV1::AutomaticFact)
            })
            .collect(),
        AutomationCommittedReceipt::UserJobDelivery(receipt) => {
            Ok(vec![AutomationCommittedReceiptV1::UserJobDelivery(
                project_external_receipt(request, receipt)?,
            )])
        }
        AutomationCommittedReceipt::SkillWriting(receipt) => {
            Ok(vec![AutomationCommittedReceiptV1::SkillWriting(
                project_external_receipt(request, receipt)?,
            )])
        }
    }
}

pub(super) fn project_recovered_committed_receipts(
    request: &AutomationRunRequestV1,
    recovered: &ProjectMemoryAutomationRunReceiptsV1,
) -> Result<Vec<AutomationCommittedReceiptV1>> {
    let outer_run_id = &request.run_id;
    if recovered.run_id() != outer_run_id {
        return Err(contract_error(
            "recovered memory receipts are not bound to the admitted outer run",
        ));
    }
    match (
        recovered.curation_receipt(),
        recovered.automatic_fact_receipts().is_empty(),
    ) {
        (Some(receipt), true) => {
            if receipt.automation_run_id() != Some(outer_run_id) {
                return Err(contract_error(
                    "recovered curation receipt is not bound to its admitted outer run",
                ));
            }
            let public_receipt = project_curation_receipt(receipt, receipt.replayed())?;
            let canonical_digest = canonical_sha256(&(
                "tracedecay.automation-run.curation-receipt.v1",
                &public_receipt,
            ))
            .map_err(contract_error)?;
            Ok(vec![AutomationCommittedReceiptV1::Curation(
                MemoryAutomationCurationReceiptV1 {
                    receipt: public_receipt,
                    canonical_digest,
                },
            )])
        }
        (None, false) => {
            let results = recovered.automatic_fact_results().map_err(contract_error)?;
            let receipts =
                tracedecay_agent_hosts::automation::NonEmptyAutomaticFactReceipts::from_vec(
                    results,
                )
                .ok_or_else(|| contract_error("recovered automatic-fact receipt set is empty"))?;
            project_committed_receipts(
                request,
                &AutomationCommittedReceipt::AutomaticFacts(receipts),
            )
        }
        (None, true) => Ok(Vec::new()),
        (Some(_), false) => Err(contract_error(
            "one automation admission cannot recover mixed curation and reflector receipts",
        )),
    }
}

fn project_external_receipt(
    request: &AutomationRunRequestV1,
    receipt: &tracedecay_agent_hosts::automation::ExternalAutomationEffectReceipt,
) -> Result<AutomationExternalEffectReceiptV1> {
    let (expected_run_id, task_key) = match &request.task {
        AutomationTaskRequestV1::SkillWriter(_) => (
            request.run_id.as_str().to_owned(),
            "skill_writer".to_owned(),
        ),
        AutomationTaskRequestV1::CombinedReview(_) => (
            format!("{}_skills", request.run_id.as_str()),
            "skill_writer".to_owned(),
        ),
        AutomationTaskRequestV1::UserJob(options) => (
            request.run_id.as_str().to_owned(),
            format!("user_job:{}", options.job_id),
        ),
        _ => {
            return Err(contract_error(
                "memory automation admission cannot carry an external receipt",
            ));
        }
    };
    if receipt.run_id() != expected_run_id || receipt.task_key() != task_key {
        return Err(contract_error(
            "external automation receipt is not bound to its admitted run and task",
        ));
    }
    let manifest_digest =
        tracedecay_domain::ManifestDigest::new(receipt.manifest_digest().to_owned())
            .map_err(contract_error)?;
    let run_id = request.run_id.clone();
    AutomationExternalEffectReceiptV1::new(run_id, task_key, manifest_digest)
        .map_err(contract_error)
}

fn project_curation_receipt(
    receipt: &ProjectMemoryFactCurationReceiptV1,
    replayed: bool,
) -> Result<MemoryAutomationCurationResultV1> {
    Ok(MemoryAutomationCurationResultV1 {
        owner: public_owner(receipt.owner()),
        operation_id: receipt.operation_id().clone(),
        input_digest: receipt.input_digest().to_owned(),
        automation_run_id: receipt
            .automation_run_id()
            .cloned()
            .ok_or_else(|| contract_error("memory curation receipt has no outer run identity"))?,
        operation_effects: receipt
            .operation_effects()
            .iter()
            .map(|effect| match effect {
                ProjectMemoryFactCurationOperationEffectV1::Add {
                    fact,
                    disposition,
                    closest_fact,
                    similarity_millionths,
                    commit,
                } => Ok(MemoryAutomationCurationOperationEffectV1::Add {
                    fact_id: fact.fact_id().clone(),
                    disposition: public_add_disposition(*disposition),
                    closest_fact_id: closest_fact.as_ref().map(|fact| fact.fact_id().clone()),
                    similarity_millionths: *similarity_millionths,
                    commit: commit
                        .as_ref()
                        .map(|commit| public_commit_receipt(commit, replayed)),
                }),
                ProjectMemoryFactCurationOperationEffectV1::Update {
                    fact,
                    trust_delta_millionths,
                    commit,
                } => Ok(MemoryAutomationCurationOperationEffectV1::Update {
                    fact_id: fact.fact_id().clone(),
                    trust_delta_millionths: *trust_delta_millionths,
                    commit: public_commit_receipt(commit, replayed),
                }),
                ProjectMemoryFactCurationOperationEffectV1::Merge { outcome } => {
                    Ok(MemoryAutomationCurationOperationEffectV1::Merge {
                        outcome: MemoryAutomationCurationMergeV1 {
                            operation_id: outcome.operation_id().clone(),
                            input_digest: outcome.input_digest().to_owned(),
                            winner_fact_id: outcome.winner().fact_id().clone(),
                            content_updated: outcome.content_updated(),
                            deleted_loser_fact_ids: outcome
                                .deleted_losers()
                                .iter()
                                .map(|fact| fact.fact_id().clone())
                                .collect(),
                            commit_receipts: outcome
                                .commit_receipts()
                                .iter()
                                .map(|commit| public_commit_receipt(commit, replayed))
                                .collect(),
                        },
                    })
                }
                ProjectMemoryFactCurationOperationEffectV1::Remove {
                    target,
                    disposition,
                    remaining_fact_count,
                    commit,
                } => Ok(MemoryAutomationCurationOperationEffectV1::Remove {
                    target_fact_id: target.fact_id().clone(),
                    disposition: public_remove_disposition(*disposition),
                    remaining_fact_count: *remaining_fact_count,
                    commit: commit
                        .as_ref()
                        .map(|commit| public_commit_receipt(commit, replayed)),
                }),
                ProjectMemoryFactCurationOperationEffectV1::NormalizeTags { fact, commit } => {
                    Ok(MemoryAutomationCurationOperationEffectV1::NormalizeTags {
                        fact_id: fact.fact_id().clone(),
                        commit: public_commit_receipt(commit, replayed),
                    })
                }
                ProjectMemoryFactCurationOperationEffectV1::LinkFacts {
                    relation,
                    disposition,
                    commit,
                } => Ok(MemoryAutomationCurationOperationEffectV1::LinkFacts {
                    source_fact_id: relation.source_fact_id().clone(),
                    target_fact_id: relation.target_fact_id().clone(),
                    relation: MemoryAutomationCurationRelationV1 {
                        kind: public_relation(relation.relation()),
                        evidence_fact_ids: relation.evidence_fact_ids().to_vec(),
                        confidence_millionths: (relation.confidence().as_f64() * 1_000_000.0)
                            .round() as u32,
                        provenance: MemoryAutomationCurationRelationProvenanceV1 {
                            source_label: relation.source_label().to_owned(),
                            sanitization_receipt: relation
                                .sanitization_receipt_value()
                                .map_err(contract_error)?,
                        },
                    },
                    disposition: match disposition {
                        ProjectMemoryFactCurationLinkDispositionV1::Linked => {
                            MemoryAutomationCurationLinkDispositionV1::Linked
                        }
                        ProjectMemoryFactCurationLinkDispositionV1::AlreadyLinked => {
                            MemoryAutomationCurationLinkDispositionV1::AlreadyLinked
                        }
                    },
                    commit: commit
                        .as_ref()
                        .map(|commit| public_commit_receipt(commit, replayed)),
                }),
            })
            .collect::<Result<Vec<_>>>()?,
        replay_fact_id: receipt.replay_fact_id().cloned(),
        replay_event_id: receipt.replay_event_id().cloned(),
        changed_fact_ids: receipt
            .changed_facts()
            .iter()
            .map(|target| target.fact_id().clone())
            .collect(),
        accepted_operations: receipt.accepted_operations(),
        facts_added: receipt.facts_added(),
        facts_updated: receipt.facts_updated(),
        facts_merged: receipt.facts_merged(),
        facts_removed: receipt.facts_removed(),
        normalized_tags: receipt.normalized_tags(),
        facts_linked: receipt.facts_linked(),
    })
}

const fn public_add_disposition(
    disposition: ProjectMemoryFactAddDispositionV1,
) -> MemoryAutomationCurationAddDispositionV1 {
    match disposition {
        ProjectMemoryFactAddDispositionV1::Added => MemoryAutomationCurationAddDispositionV1::Added,
        ProjectMemoryFactAddDispositionV1::NearDuplicate => {
            MemoryAutomationCurationAddDispositionV1::NearDuplicate
        }
        ProjectMemoryFactAddDispositionV1::PossibleConflict => {
            MemoryAutomationCurationAddDispositionV1::PossibleConflict
        }
    }
}

const fn public_remove_disposition(
    disposition: ProjectMemoryFactCurationRemoveDispositionV1,
) -> MemoryAutomationCurationRemoveDispositionV1 {
    match disposition {
        ProjectMemoryFactCurationRemoveDispositionV1::Removed => {
            MemoryAutomationCurationRemoveDispositionV1::Removed
        }
        ProjectMemoryFactCurationRemoveDispositionV1::AlreadyRemoved => {
            MemoryAutomationCurationRemoveDispositionV1::AlreadyRemoved
        }
        ProjectMemoryFactCurationRemoveDispositionV1::NotFound => {
            MemoryAutomationCurationRemoveDispositionV1::NotFound
        }
    }
}

const fn public_relation(relation: FactRelationKindV1) -> MemoryAutomationCurationRelationKindV1 {
    match relation {
        FactRelationKindV1::Supports => MemoryAutomationCurationRelationKindV1::Supports,
        FactRelationKindV1::Contradicts => MemoryAutomationCurationRelationKindV1::Contradicts,
        FactRelationKindV1::Supersedes => MemoryAutomationCurationRelationKindV1::Supersedes,
        FactRelationKindV1::DerivedFrom => MemoryAutomationCurationRelationKindV1::DerivedFrom,
    }
}

fn project_automatic_fact_receipt(
    result: &ProjectMemoryAutomaticFactApplyResultV1,
) -> Result<MemoryAutomationFactReceiptV1> {
    let receipt = result.receipt();
    let request = receipt.request();
    let metadata = request
        .metadata()
        .as_object()
        .ok_or_else(|| contract_error("automatic fact canonical metadata is not a JSON object"))?;
    let state = match receipt.state() {
        ProjectMemoryAutomaticFactStateV1::Applied => MemoryAutomationFactStateV1::Applied,
        ProjectMemoryAutomaticFactStateV1::Quarantined => MemoryAutomationFactStateV1::Quarantined,
    };
    let disposition = match result.disposition() {
        ProjectMemoryAutomaticFactApplyDispositionV1::Applied => {
            MemoryAutomationFactDispositionV1::Applied
        }
        ProjectMemoryAutomaticFactApplyDispositionV1::AlreadyApplied => {
            MemoryAutomationFactDispositionV1::AlreadyApplied
        }
        ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined => {
            MemoryAutomationFactDispositionV1::Quarantined
        }
    };
    let effect = match receipt.state() {
        ProjectMemoryAutomaticFactStateV1::Applied => {
            let fact_id = receipt
                .applied_fact_id()
                .cloned()
                .ok_or_else(|| contract_error("applied automatic fact has no fact identity"))?;
            let target = receipt
                .applied_target()
                .ok_or_else(|| contract_error("applied automatic fact has no owned target"))?;
            MemoryAutomationFactEffectV1::Applied {
                fact_id,
                target: MemoryAutomationFactTargetV1 {
                    owner: public_owner(target.owner()),
                    fact_id: target.fact_id().clone(),
                },
                assertion_id: receipt.applied_assertion_id().cloned().ok_or_else(|| {
                    contract_error("applied automatic fact has no assertion identity")
                })?,
                event_id: receipt.applied_event_id().cloned().ok_or_else(|| {
                    contract_error("applied automatic fact has no event identity")
                })?,
            }
        }
        ProjectMemoryAutomaticFactStateV1::Quarantined => {
            MemoryAutomationFactEffectV1::Quarantined {
                reason: receipt
                    .quarantine_reason()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| contract_error("quarantined automatic fact has no reason"))?,
            }
        }
    };
    Ok(MemoryAutomationFactReceiptV1 {
        apply_id: receipt.apply_id().clone(),
        owner: public_owner(receipt.owner()),
        state,
        disposition,
        automation_run_id: RunId::new(
            receipt
                .automation_run_id()
                .ok_or_else(|| {
                    contract_error("automatic fact authority receipt has no outer run identity")
                })?
                .to_owned(),
        )
        .map_err(contract_error)?,
        request: MemoryAutomationFactRequestV1 {
            operation_id: request.operation_id().clone(),
            input_digest: MemoryAutomationFactInputDigestV1::new(request.input_digest())
                .map_err(contract_error)?,
            actor: request.actor().cloned(),
            sanitization_receipt: request.sanitization_receipt().clone(),
            content: request.content().to_owned(),
            category: public_category(request.category()),
            source_label: request.source_label().map(ToOwned::to_owned),
            tags: request.tags().to_vec(),
            entities: request.entities().to_vec(),
            default_trust_millionths: (request.default_trust().as_f64() * 1_000_000.0).round()
                as u32,
            metadata: metadata.clone().into_iter().collect(),
        },
        evidence: MemoryAutomationFactEvidenceV1 {
            evidence_hash: receipt.evidence().evidence_hash().map(ToOwned::to_owned),
            item: receipt
                .evidence()
                .item()
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(contract_error)?,
            validation: receipt
                .evidence()
                .validation()
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(contract_error)?,
        },
        effect,
        recorded_at_micros: receipt.recorded_at(),
        canonical_digest: result.canonical_digest().map_err(contract_error)?,
    })
}

fn public_owner(owner: &FactOwnerV1) -> FactCommitOwnerV1 {
    match owner {
        FactOwnerV1::Profile => FactCommitOwnerV1::Profile,
        FactOwnerV1::Project { project_id } => FactCommitOwnerV1::Project {
            project_id: project_id.clone(),
        },
    }
}

fn public_commit_receipt(
    receipt: &tracedecay_store::FactCommitReceipt,
    replayed: bool,
) -> FactCommitReceiptV1 {
    FactCommitReceiptV1 {
        disposition: if replayed {
            FactCommitDispositionV1::IdempotentReplay
        } else {
            FactCommitDispositionV1::Committed
        },
        fact_id: receipt.fact_id().clone(),
        owner: public_owner(receipt.owner()),
        committed_event_ids: receipt.committed_event_ids().to_vec(),
        last_event_id: receipt.last_event_id().clone(),
        active_assertion_id: receipt.active_assertion_id().cloned(),
    }
}

const fn public_category(
    category: FactCategoryV1,
) -> tracedecay_application::memory::FactCategoryV1 {
    match category {
        FactCategoryV1::General => tracedecay_application::memory::FactCategoryV1::General,
        FactCategoryV1::UserPref => tracedecay_application::memory::FactCategoryV1::UserPref,
        FactCategoryV1::Project => tracedecay_application::memory::FactCategoryV1::Project,
        FactCategoryV1::Tool => tracedecay_application::memory::FactCategoryV1::Tool,
        FactCategoryV1::Decision => tracedecay_application::memory::FactCategoryV1::Decision,
        FactCategoryV1::CodeArea => tracedecay_application::memory::FactCategoryV1::CodeArea,
    }
}

#[cfg(test)]
mod tests;
