//! `tracedecay_admin_project`: the bookkeeping the project's owner maintains
//! for first-party commands.

use std::sync::Arc;

use tracedecay_automation_runtime::automation::AutomationRunControl;
use tracedecay_contracts::retrieval::{
    AdminProjectBenchV1, AdminProjectCounterResetV1, AdminProjectCounterV1, AdminProjectResultV1,
    AdminProjectStatusAccountingV1, AdminProjectSurfaceRequestV1, AutomaticFactAddRequestV1,
    AutomaticFactEvidenceV1, AutomaticFactReceiptAvailabilityV1, AutomaticFactReceiptListV1,
    AutomaticFactReceiptStateV1, AutomaticFactReceiptV1, AutomaticFactReceiptViewV1,
    AutomationReconcileScope, AutomationSchedulerReconcileOutcome,
    ProjectAutomationReconcileReport,
};
use tracedecay_contracts::{CancellationSignal, Deadline, now_micros};
use tracedecay_domain::ProvenanceId;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_project::project::TraceDecay;
use tracedecay_session_memory::fact_store::DatabaseFactStore;
use tracedecay_session_memory::memory::{MemoryApplication, MemoryApplicationError};
use tracedecay_store::{ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1};

fn project_memory_application<'a>(
    cg: &TraceDecay,
    db: &'a tracedecay_runtime_core::db::Database,
) -> Result<MemoryApplication<DatabaseFactStore<'a>>> {
    let owner = cg.project_memory_owner()?;
    MemoryApplication::new(owner, DatabaseFactStore::new(db))
        .map_err(|error| memory_application_error(&error))
}

fn memory_application_error(error: &MemoryApplicationError) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("project memory application failed: {error}"),
    }
}

fn admin_project_run_control(
    deadline: Deadline,
    cancellation: CancellationSignal,
) -> AutomationRunControl {
    AutomationRunControl::from_interrupted(Arc::new(move || {
        cancellation.is_cancelled() || deadline.is_elapsed_at(now_micros())
    }))
}

fn parse_automatic_fact_apply_id(value: String) -> Result<ProvenanceId> {
    ProvenanceId::new(value).map_err(|error| TraceDecayError::Config {
        message: format!("invalid automatic fact apply id: {error}"),
    })
}

const fn store_receipt_state(
    state: AutomaticFactReceiptStateV1,
) -> ProjectMemoryAutomaticFactStateV1 {
    match state {
        AutomaticFactReceiptStateV1::Applied => ProjectMemoryAutomaticFactStateV1::Applied,
        AutomaticFactReceiptStateV1::Quarantined => ProjectMemoryAutomaticFactStateV1::Quarantined,
    }
}

fn automatic_fact_receipt(receipt: &ProjectMemoryAutomaticFactReceiptV1) -> AutomaticFactReceiptV1 {
    let request = receipt.request();
    let evidence = receipt.evidence();
    AutomaticFactReceiptV1 {
        apply_id: receipt.apply_id().as_str().to_owned(),
        state: match receipt.state() {
            ProjectMemoryAutomaticFactStateV1::Applied => AutomaticFactReceiptStateV1::Applied,
            ProjectMemoryAutomaticFactStateV1::Quarantined => {
                AutomaticFactReceiptStateV1::Quarantined
            }
        },
        operation_id: request.operation_id().as_str().to_owned(),
        add_fact_request: AutomaticFactAddRequestV1 {
            content: request.content().to_owned(),
            category: request.category(),
            source_label: request.source_label().map(str::to_owned),
            tags: request.tags().to_vec(),
            entities: request.entities().to_vec(),
            trust: request.default_trust().as_f64(),
            metadata: request.metadata().clone(),
        },
        evidence: AutomaticFactEvidenceV1 {
            evidence_hash: evidence.evidence_hash().map(str::to_owned),
            item: evidence.item().cloned(),
            validation: evidence.validation().cloned(),
        },
        recorded_at_micros: receipt.recorded_at().0,
        applied_fact_id: receipt
            .applied_fact_id()
            .map(|fact_id| fact_id.as_str().to_owned()),
        quarantine_reason: receipt.quarantine_reason().map(str::to_owned),
    }
}

/// Serve one `tracedecay_admin_project` request for the project's owner.
/// The profile's automation reconcile is the daemon's profile owner's.
#[hotpath::measure(future = true, label = "mcp.admin.project.total")]
pub async fn compute_admin_project(
    cg: &TraceDecay,
    request: AdminProjectSurfaceRequestV1,
    global_db: Option<&RegisteredGlobalDb>,
    automation_scheduler_reconciler: Option<
        tracedecay_dashboard_api::AutomationSchedulerReconciler,
    >,
    application_deadline: Deadline,
    application_cancellation: CancellationSignal,
) -> Result<AdminProjectResultV1> {
    let run_control = admin_project_run_control(application_deadline, application_cancellation);
    Ok(match request {
        AdminProjectSurfaceRequestV1::CounterGet {} => {
            AdminProjectResultV1::Counter(AdminProjectCounterV1 {
                counter: cg.get_local_counter().await?,
            })
        }
        AdminProjectSurfaceRequestV1::CounterReset {} => {
            cg.reset_local_counter().await?;
            AdminProjectResultV1::CounterReset(AdminProjectCounterResetV1 { reset: true })
        }
        AdminProjectSurfaceRequestV1::AutomationReconcile { scope } => {
            if scope != AutomationReconcileScope::Project {
                return Err(TraceDecayError::Config {
                    message: "profile automation reconciliation is answered by the daemon's profile owner, not a project's".to_owned(),
                });
            }
            let outcome = match automation_scheduler_reconciler {
                Some(reconcile) => reconcile().await,
                None => AutomationSchedulerReconcileOutcome::OwnerUnavailable,
            };
            AdminProjectResultV1::ProjectAutomationReconcile(ProjectAutomationReconcileReport {
                scope,
                outcome,
            })
        }
        AdminProjectSurfaceRequestV1::StatusAccounting {} => {
            let global_db = global_db.ok_or_else(|| TraceDecayError::Config {
                message: "daemon global database is unavailable".to_string(),
            })?;
            let tokens_saved = cg.get_tokens_saved().await?;
            // An explicit accounting status action fails closed: a registry
            // it cannot write or read is an error, not a null total.
            global_db
                .try_upsert_project_tokens(cg.project_root(), tokens_saved)
                .await?;
            let global_tokens_saved = global_db
                .try_global_tokens_saved()
                .await
                .map_err(|message| TraceDecayError::Config { message })
                .map(|total| total.saturating_sub(tokens_saved))
                .map(|total| (total > 0).then_some(total))?;
            AdminProjectResultV1::StatusAccounting(AdminProjectStatusAccountingV1 {
                tokens_saved,
                global_tokens_saved,
            })
        }
        AdminProjectSurfaceRequestV1::Bench {
            queries_toml,
            json,
            max_nodes,
        } => {
            let report = crate::bench::run_bench_with_toml(
                cg,
                queries_toml
                    .as_deref()
                    .unwrap_or(crate::bench::DEFAULT_QUERIES_TOML),
                crate::bench::BenchOptions {
                    format: crate::bench::OutputFormat::Json,
                    max_nodes,
                },
            )?;
            AdminProjectResultV1::Bench(AdminProjectBenchV1 {
                output: if json {
                    crate::bench::format_report_json(&report)
                } else {
                    crate::bench::format_report_console(&report)
                },
            })
        }
        AdminProjectSurfaceRequestV1::AutomaticFactReceiptList { state, limit } => {
            let db = cg.open_project_store_db()?;
            let memory = project_memory_application(cg, &db)?;
            let page = memory
                .list_project_memory_automatic_fact_receipts(
                    state.map(store_receipt_state),
                    None,
                    limit,
                    run_control.read_control(),
                )
                .await
                .map_err(|error| memory_application_error(&error))?;
            let receipts = page
                .receipts()
                .iter()
                .map(automatic_fact_receipt)
                .collect::<Vec<_>>();
            AdminProjectResultV1::AutomaticFactReceiptList(AutomaticFactReceiptListV1 {
                availability: AutomaticFactReceiptAvailabilityV1::Available,
                count: receipts.len(),
                receipts,
                next_after_apply_id: page
                    .next_after_apply_id()
                    .map(|apply_id| apply_id.as_str().to_owned()),
            })
        }
        AdminProjectSurfaceRequestV1::AutomaticFactReceiptView { id } => {
            let apply_id = parse_automatic_fact_apply_id(id)?;
            let db = cg.open_project_store_db()?;
            let memory = project_memory_application(cg, &db)?;
            let receipt = memory
                .get_project_memory_automatic_fact_receipt(apply_id, run_control.read_control())
                .await
                .map_err(|error| memory_application_error(&error))?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "automatic fact receipt not found".to_string(),
                })?;
            AdminProjectResultV1::AutomaticFactReceiptView(Box::new(AutomaticFactReceiptViewV1 {
                receipt: automatic_fact_receipt(&receipt),
            }))
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tracedecay_session_memory::memory::ProjectMemoryFactAddRequest;

    use super::*;

    fn test_application_control() -> (Deadline, CancellationSignal) {
        (
            Deadline::new(tracedecay_domain::UtcMicros(i64::MAX)).unwrap(),
            CancellationSignal::active("cancel.admin-project-test").unwrap(),
        )
    }

    async fn seed_automatic_fact_receipt(
        cg: &TraceDecay,
        apply_id: &str,
        content: &str,
    ) -> ProjectMemoryAutomaticFactReceiptV1 {
        use tracedecay_domain::{ActorId, Confidence, FactCategoryV1};

        let owner = cg.project_memory_owner().unwrap();
        let db = cg.open_project_store_db().unwrap();
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
        let actor = ActorId::new("automation.session-reflector".to_owned()).unwrap();
        let request = tracedecay_session_memory::memory::automatic_fact_add_command(
            owner,
            ProjectMemoryFactAddRequest {
                content: content.to_owned(),
                category: FactCategoryV1::Decision,
                source_label: Some("admin-project-test".to_owned()),
                tags: Vec::new(),
                entities: Vec::new(),
                trust: Some(Confidence::new(0.9).unwrap()),
                metadata: json!({}),
            },
            "run.admin-project-test",
            apply_id,
            Some(actor),
        )
        .unwrap();
        let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
        let write_control = run_control.write_control();
        memory
            .apply_project_memory_automatic_fact(
                ProvenanceId::new(apply_id.to_owned()).unwrap(),
                request,
                tracedecay_store::ProjectMemoryAutomaticFactEvidenceV1::default(),
                &write_control,
            )
            .await
            .unwrap()
            .receipt()
            .clone()
    }

    async fn admin_project(cg: &TraceDecay, request: AdminProjectSurfaceRequestV1) -> Value {
        let (deadline, cancellation) = test_application_control();
        serde_json::to_value(
            compute_admin_project(cg, request, None, None, deadline, cancellation)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn admin_project_reads_terminal_automatic_fact_receipts_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&profile_root).unwrap();
        let project_root = std::fs::canonicalize(project_root).unwrap();
        let profile_root = std::fs::canonicalize(profile_root).unwrap();
        let cg = TraceDecay::init_with_options_for_test(
            &project_root,
            tracedecay_project::project::TraceDecayOpenOptions {
                global_db_path: Some(profile_root.join("global.db")),
                profile_root: Some(profile_root),
            },
        )
        .await
        .unwrap();

        let apply_id = "automatic-fact.rpc.read-only";
        let seeded = seed_automatic_fact_receipt(
            &cg,
            apply_id,
            "Admin project RPC reads this terminal automatic fact receipt",
        )
        .await;
        let owner_before =
            tracedecay_runtime_core::db::probe_writer_owner(&cg.store_layout().graph_db_path)
                .unwrap();
        let receipt = json!({
            "apply_id": apply_id,
            "state": "applied",
            "operation_id": seeded.request().operation_id().as_str(),
            "add_fact_request": {
                "content": "Admin project RPC reads this terminal automatic fact receipt",
                "category": "decision",
                "source_label": "admin-project-test",
                "tags": [],
                "entities": [],
                "trust": 0.9,
                "metadata": {},
            },
            "evidence": {},
            "recorded_at_micros": seeded.recorded_at().0,
            "applied_fact_id": seeded.applied_fact_id().unwrap().as_str(),
        });

        assert_eq!(
            admin_project(
                &cg,
                AdminProjectSurfaceRequestV1::AutomaticFactReceiptList {
                    state: Some(AutomaticFactReceiptStateV1::Applied),
                    limit: 50,
                },
            )
            .await,
            json!({
                "availability": { "state": "available" },
                "count": 1,
                "receipts": [receipt.clone()],
                "next_after_apply_id": null,
            })
        );
        assert_eq!(
            admin_project(
                &cg,
                AdminProjectSurfaceRequestV1::AutomaticFactReceiptList {
                    state: Some(AutomaticFactReceiptStateV1::Quarantined),
                    limit: 50,
                },
            )
            .await,
            json!({
                "availability": { "state": "available" },
                "count": 0,
                "receipts": [],
                "next_after_apply_id": null,
            })
        );
        assert_eq!(
            admin_project(
                &cg,
                AdminProjectSurfaceRequestV1::AutomaticFactReceiptView {
                    id: apply_id.to_owned(),
                },
            )
            .await,
            json!({ "receipt": receipt })
        );

        let owner_after =
            tracedecay_runtime_core::db::probe_writer_owner(&cg.store_layout().graph_db_path)
                .unwrap();
        assert_eq!(owner_after, owner_before);
    }
}
