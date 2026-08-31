//! Daemon-side Doctor signal gatherers for the read-only kernel source ports.
//!
//! The transport-neutral Doctor kernel
//! ([`tracedecay_application::doctor`]) owns the seven source-port adapters,
//! [`DaemonRuntimeHealthSignalV1`], and [`compose_doctor_report`]. This module
//! gathers live daemon signals (scheduler, diagnostic broker, registered
//! stores, host-bundle receipts) and maps daemon-owned types into those kernel
//! reads. Truthfulness is preserved end to end: a signal that cannot be
//! consulted maps to the kernel's typed
//! `Unsupported`/`Absent`/`Denied`/`Unknown` read — never a fabricated healthy
//! result — and partial coverage carries its real reason.
//!
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::PinnedRuntimeConfiguration;
use tracedecay_application::doctor::{
    AdvisoryFeedbackReadV1, CodeIndexMountReadV1, CodeIndexMountStateV1,
    ConfigurationAuthorityReadV1, ConfigurationDriftV1, DaemonRuntimeHealthSignalV1,
    DoctorCoverageCompletenessV1, DoctorKernelInputsV1, DoctorStorageFamilyReadV1,
    DoctorStorageIncompleteReasonV1, HostConformanceV1, HostIntegrationReadV1,
    IngestRefusalCensusReadV1, IngestRefusalCountV1, LanguageServerReadV1, LanguageServerStateV1,
    ObservabilityReadV1, ObservabilityStateV1, OperationalAuditReadV1, ProfileAuthorityReadV1,
    RemoteOperationalReadV1, advisory_feedback_read_from_publication, compose_doctor_report,
    merge_storage_reads, runtime_health_read, storage_family_read,
};
use tracedecay_application::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_application::{
    ApplicationContractError, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, now_micros,
};
use tracedecay_usecases::semantic_runtime::ProjectSemanticActivationExt;

use super::maintenance::GuardedStoreTelemetryPort;
use tracedecay_daemon_service::DaemonFeedbackRuntimeRegistrar;

const DOCTOR_REPORT_CAPABILITY: &str = "capability.application.doctor.report";
const DOCTOR_REPORT_USE_CASE: &str = "use-case.application.doctor.report";
const DOCTOR_CONTEXT_HORIZON_MICROS: i64 = 30_000_000;

// === Configuration authority (Configuration family) ==========================

/// Map a real pinned-configuration lookup outcome into a kernel read.
///
/// A pinned snapshot resolves in-sync (the cache invariant guarantees the pinned
/// configuration equals the value derived from its resolved snapshot, so within
/// the cache there is no unobserved drift). A cold cache — the fail-closed
/// accessor's `Err` — is a typed [`ConfigurationAuthorityReadV1::Absent`], never
/// a fabricated healthy result.
#[must_use]
pub fn configuration_read_from_pin<E>(
    resolved: &Result<PinnedRuntimeConfiguration, E>,
) -> ConfigurationAuthorityReadV1 {
    match resolved {
        Ok(_) => ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::InSync,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        Err(_) => ConfigurationAuthorityReadV1::Absent,
    }
}

/// Run the exhaustive observation-authority invariant pass over an already
/// acquired read snapshot of the registered profile authority.
///
/// This is the same pass the `tracedecay_runtime` producers run
/// ([`tracedecay_global_db::schema_stages::validate_observation_authority_connection`]):
/// read-only, so Doctor observes the invariant without owning any repair of it.
/// `true` means the audit ran and every invariant held; `false` means it ran and
/// an invariant failed. "Could not run" is not representable here — the caller
/// owns that distinction.
async fn observation_authority_audit_passed(
    snapshot: &impl tracedecay_runtime_core::db::engine::QueryExecutor,
) -> bool {
    tracedecay_global_db::schema_stages::validate_observation_authority_connection(snapshot)
        .await
        .is_ok()
}

/// Observe the storage authority audit signal the daemon-side Doctor reader
/// reports as [`DaemonRuntimeHealthSignalV1::authority_audit_ok`].
///
/// Tri-state, matching the vocabulary the `tracedecay_runtime` producers already
/// publish: `Some(true)` only when the audit ran and passed, `Some(false)` when
/// it ran and an invariant failed, and `None` when it could not run at all
/// because the registered authority would not yield a read snapshot. A not-run
/// audit weakens runtime coverage to partial rather than claiming health.
async fn observation_authority_audit_ok(
    registry: &tracedecay_global_db::RegisteredGlobalDb,
) -> Option<bool> {
    match registry.read_snapshot().await {
        Ok(snapshot) => Some(observation_authority_audit_passed(&snapshot).await),
        Err(_) => None,
    }
}

// === Host/agent integration conformance (Advisory family) ====================

fn host_integration_read_from_report(
    report: &crate::agents::host_bundle_v2::HostBundleDoctorReportV1,
) -> HostIntegrationReadV1 {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1;

    if report.native_edit_stop_conformance.is_empty() {
        return HostIntegrationReadV1::Unsupported;
    }
    if report.components.is_empty() {
        return HostIntegrationReadV1::Absent;
    }
    let conformance = if report.components.iter().any(|component| {
        matches!(
            component.state,
            HostBundleComponentDoctorStateV1::Corrupt
                | HostBundleComponentDoctorStateV1::OwnershipConflict
        )
    }) {
        HostConformanceV1::ProtocolDrift
    } else if report.components.iter().any(|component| {
        // `Drifted`, `OrphanedRegistration`, and `ActivationDeferred` are
        // repairable conformance, not protocol drift: the component's ownership
        // is intact and either the ordinary reinstall or the host's own
        // activation converges it, so none may escalate to `ProtocolDrift`.
        matches!(
            component.state,
            HostBundleComponentDoctorStateV1::Repairable
                | HostBundleComponentDoctorStateV1::Missing
                | HostBundleComponentDoctorStateV1::Drifted
                | HostBundleComponentDoctorStateV1::OrphanedRegistration
                | HostBundleComponentDoctorStateV1::ActivationDeferred
        )
    }) {
        HostConformanceV1::Drifted
    } else {
        HostConformanceV1::Conformant
    };
    HostIntegrationReadV1::Observed {
        conformance,
        coverage: DoctorCoverageCompletenessV1::Complete,
    }
}

// === Code/semantic index mount (SemanticIndex family) ========================

/// Read the real code-index mount state from the daemon scheduler registry.
///
/// An unmounted worktree reports `Unmounted`; a mounted worktree whose freshness
/// ladder has already proven a complete generation current reports `Mounted`;
/// a worktree whose background convergence is parked on a deterministic
/// contract violation reports `Parked` with the exact reason; stale,
/// restored-unverified, or busy schedulers report `Indexing` and schedule
/// background reconciliation. Doctor never performs code-index catch-up on its
/// request path.
#[hotpath::measure(label = "daemon.doctor.code_index", future = true)]
pub(in crate::daemon) async fn code_index_read_from_registry(
    registry: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> CodeIndexMountReadV1 {
    if !registry.is_worktree_mounted(project_root).await {
        return CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Unmounted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    if registry.latest_complete_ready(project_root).await.is_some() {
        return CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Mounted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    if let Some(parked) = registry.convergence_park(project_root).await {
        return CodeIndexMountReadV1::Parked {
            reason: format!("{}; {}", parked.reason, parked.remediation),
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    CodeIndexMountReadV1::Observed {
        state: CodeIndexMountStateV1::Indexing,
        coverage: DoctorCoverageCompletenessV1::Complete,
    }
}

// === Language server/analyzer (LanguageServer family) ========================

/// Map the daemon diagnostic broker's project-active engine statuses.
#[must_use]
pub fn language_server_read_from_engine_states(
    states: impl IntoIterator<Item = tracedecay_lsp::analyzer::broker::EngineState>,
) -> LanguageServerReadV1 {
    use tracedecay_lsp::analyzer::broker::EngineState;

    let states = states.into_iter().collect::<Vec<_>>();
    if states.is_empty() {
        return LanguageServerReadV1::Absent;
    }
    let state = if states.contains(&EngineState::Crashed) {
        LanguageServerStateV1::Crashed
    } else if states.contains(&EngineState::Unavailable) {
        LanguageServerStateV1::Unavailable
    } else if states.contains(&EngineState::Disabled) {
        LanguageServerStateV1::Disabled
    } else if states.contains(&EngineState::Refreshing) {
        LanguageServerStateV1::Refreshing
    } else if states.iter().all(|state| *state == EngineState::Ready) {
        LanguageServerStateV1::Ready
    } else {
        LanguageServerStateV1::Available
    };
    LanguageServerReadV1::Observed {
        state,
        coverage: DoctorCoverageCompletenessV1::Complete,
    }
}

/// Read live project-active analyzer state from the daemon diagnostic owner.
pub async fn language_server_read_from_broker(
    broker: &tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>,
) -> LanguageServerReadV1 {
    let statuses = broker.lock().await.project_engine_statuses();
    language_server_read_from_engine_states(statuses.into_iter().map(|status| status.state))
}

// === Canonical Plan-26 observations (Observability family) ===================

/// Map the canonical durable Plan-26 read model into a truthful Doctor read.
#[must_use]
pub fn observability_read_from_model(
    model: Result<
        tracedecay_usecases::feedback::observations::FeedbackObservationReadModelV1,
        tracedecay_usecases::feedback::concrete::FeedbackRuntimeError,
    >,
) -> ObservabilityReadV1 {
    match model {
        Ok(model)
            if model.total_count == 0
                && model.denominators.eligible == 0
                && model.denominators.incomplete_boots == 0
                && model.watermark.producer_boot_id.is_none() =>
        {
            ObservabilityReadV1::Absent
        }
        Ok(model) => {
            use tracedecay_application::feedback::observations::FeedbackCoverageV1;
            let (state, coverage) = match model.coverage {
                FeedbackCoverageV1::Known => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Complete,
                ),
                FeedbackCoverageV1::Stale => (
                    ObservabilityStateV1::Stale,
                    DoctorCoverageCompletenessV1::Partial,
                ),
                FeedbackCoverageV1::Partial
                | FeedbackCoverageV1::Sampled
                | FeedbackCoverageV1::Capped => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Partial,
                ),
                FeedbackCoverageV1::Unknown => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Unknown,
                ),
            };
            ObservabilityReadV1::Observed {
                state,
                total_count: model.total_count,
                last_observed_at_micros: model.watermark.observed_through.map(|value| value.0),
                coverage,
            }
        }
        Err(_) => ObservabilityReadV1::Unknown,
    }
}

/// Map the durable cursor-advance refusal censuses of the consulted sessions
/// stores into one truthful kernel read. Counts merge by provider/reason; a
/// single unavailable store makes the whole census `Unknown` rather than a
/// silently partial healthy claim.
#[must_use]
pub fn ingest_refusal_read_from_censuses(
    censuses: &[tracedecay_global_db::observation::ObservationRefusalCensusV1],
) -> IngestRefusalCensusReadV1 {
    let mut merged: std::collections::BTreeMap<(String, String), u64> =
        std::collections::BTreeMap::new();
    for census in censuses {
        match census {
            tracedecay_global_db::observation::ObservationRefusalCensusV1::Observed {
                refusals,
            } => {
                for refusal in refusals {
                    let key = (refusal.provider.clone(), refusal.reason.clone());
                    let entry = merged.entry(key).or_insert(0);
                    *entry = entry.saturating_add(refusal.count);
                }
            }
            tracedecay_global_db::observation::ObservationRefusalCensusV1::Unavailable => {
                return IngestRefusalCensusReadV1::Unknown;
            }
        }
    }
    IngestRefusalCensusReadV1::Observed {
        refusals: merged
            .into_iter()
            .map(|((provider, reason), count)| IngestRefusalCountV1 {
                provider,
                reason,
                count,
            })
            .collect(),
    }
}

// === Storage retention/size (Storage family) =================================

fn orphan_store_findings_from_census(
    census: &[tracedecay_maintenance::retention::orphan_stores::StoreCensusEntry],
    retention_secs: i64,
    now: i64,
) -> DoctorStorageFamilyReadV1 {
    let classified = tracedecay_maintenance::retention::orphan_stores::classify_stores(census, now);
    let plan = tracedecay_maintenance::retention::orphan_stores::plan_collection(
        classified,
        retention_secs,
    );
    storage_family_read(
        plan.collect
            .iter()
            .chain(plan.retained_immature.iter())
            .chain(plan.relink.iter())
            .filter_map(crate::doctor::registry_drift::orphan_store_doctor_finding)
            .collect(),
    )
}

/// Collect the daemon's read-only unregistered-store-directory Doctor
/// findings for a profile (plan 38 §2's disjoint on-disk-only audit class —
/// a store directory with no `code_projects` row at all, invisible to the
/// registry-driven census performs). Runs the bottom-up sweep in
/// classification-only mode (no collection).
#[hotpath::measure(label = "daemon.doctor.unregistered_stores", future = true)]
pub async fn collect_unregistered_store_findings(
    global_db: &tracedecay_global_db::RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
) -> DoctorStorageFamilyReadV1 {
    let report = tracedecay_maintenance::retention::orphan_stores::sweep_unregistered_stores(
        global_db,
        profile_root,
        retention_secs,
        now,
        false,
    )
    .await;
    let Ok(report) = report else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    hotpath::gauge!("daemon.doctor.unregistered_stores_total")
        .inc((report.plan.collect.len() + report.plan.retained_immature.len()) as u64);
    storage_family_read(
        report
            .plan
            .collect
            .iter()
            .chain(report.plan.retained_immature.iter())
            .filter_map(crate::doctor::registry_drift::unregistered_store_doctor_finding)
            .collect(),
    )
}

/// Evaluate every owner-configured soft budget against the daemon's retained
/// project, registry, and session stores. A configured key that is not mounted
/// is emitted as typed unknown telemetry rather than silently omitted.
struct CollectedStoreTelemetryV1 {
    findings: DoctorStorageFamilyReadV1,
    table_growth_evidence: Vec<tracedecay_application::storage::TableGrowthDoctorEvidenceV1>,
}

const MAX_SYNCHRONOUS_TABLE_GROWTH_STORE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_ENTRIES: usize = 4_096;
/// Entry ceiling for the code-index generation census.
///
/// The census is metadata-only — a `stat` and a bounded manifest prefix per
/// generation — so its cost scales with the number of directory entries, not
/// with their size. Gating it on bytes instead (the previous
/// `MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES` budget) compared a 64 MiB ceiling
/// against generation files that are routinely ~1 GiB each, so the gate failed
/// on every real profile and the finding this kernel exists to produce was
/// structurally unreachable.
const MAX_SYNCHRONOUS_GENERATION_CENSUS_ENTRIES: usize = 4_096;

fn permits_synchronous_exhaustive_scan(root: &Path) -> bool {
    let mut pending = vec![root.to_path_buf()];
    let mut observed_bytes = 0_u64;
    let mut observed_entries = 0_usize;
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            observed_entries = observed_entries.saturating_add(1);
            if observed_entries > MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_ENTRIES {
                return false;
            }
            let Ok(file_type) = entry.file_type() else {
                return false;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                return false;
            }
            let Ok(metadata) = entry.metadata() else {
                return false;
            };
            observed_bytes = observed_bytes.saturating_add(metadata.len());
            if observed_bytes > MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES {
                return false;
            }
        }
    }
    true
}

/// Whether the sealed-generation directory is small enough (in *entries*) for a
/// synchronous metadata census. Byte size is deliberately not consulted.
fn permits_synchronous_generation_census(generations_root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(generations_root) else {
        return false;
    };
    let mut observed_entries = 0_usize;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_symlink() {
            continue;
        }
        observed_entries = observed_entries.saturating_add(1);
        if observed_entries > MAX_SYNCHRONOUS_GENERATION_CENSUS_ENTRIES {
            return false;
        }
    }
    true
}

fn permits_synchronous_session_retention_backlog(database_path: &Path) -> bool {
    ["", "-wal", "-shm"]
        .into_iter()
        .try_fold(0_u64, |total, suffix| {
            let mut path = database_path.as_os_str().to_os_string();
            path.push(suffix);
            match std::fs::metadata(PathBuf::from(path)) {
                Ok(metadata) => total
                    .checked_add(metadata.len())
                    .filter(|size| *size <= MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(total),
                Err(_) => None,
            }
        })
        .is_some()
}

fn permits_synchronous_table_growth(
    read: &tracedecay_application::storage::StorageTelemetryReadV1,
) -> bool {
    matches!(
        read,
        tracedecay_application::storage::StorageTelemetryReadV1::Observed { sample }
            if sample.total_bytes().get() <= MAX_SYNCHRONOUS_TABLE_GROWTH_STORE_BYTES
    )
}

#[hotpath::measure(label = "daemon.doctor.over_budget", future = true)]
async fn collect_over_budget_store_findings(
    context: &RequestContext,
    telemetry_ports: &[(
        tracedecay_application::storage::StoreKeyV1,
        GuardedStoreTelemetryPort,
    )],
    retention: &crate::config::RetentionConfig,
) -> CollectedStoreTelemetryV1 {
    use std::collections::BTreeMap;
    use tracedecay_application::storage::{
        StorageTelemetryReadV1, StoreSizeTelemetryPort, TableGrowthTelemetryReadV1,
        over_budget_finding, table_growth_doctor_evidence, table_growth_finding,
    };

    // Items-processed for the over-budget sweep: how many mounted stores this
    // pass actually sampled, so the sweep span divides into per-store cost.
    hotpath::gauge!("daemon.doctor.telemetry_stores_total").inc(telemetry_ports.len() as u64);
    let mut reads = BTreeMap::new();
    let mut table_growth_evidence = Vec::new();
    for (store, port) in telemetry_ports {
        let read = port.store_size(context, store).await;
        let table_growth = if permits_synchronous_table_growth(&read) {
            port.preview_table_growth(context, store).await
        } else {
            TableGrowthTelemetryReadV1::Unknown {
                store: store.clone(),
            }
        };
        if let TableGrowthTelemetryReadV1::Observed { samples, .. } = &table_growth {
            for sample in samples {
                tracing::info!(
                    target: "tracedecay::storage_telemetry",
                    store = sample.store.as_str(),
                    table = sample.table.as_str(),
                    previous_bytes = sample.previous_bytes.0,
                    current_bytes = sample.current_bytes.0,
                    growth_bytes = sample.growth_bytes().0,
                    previous_observed_at = sample.previous_observed_at.0,
                    current_observed_at = sample.current_observed_at.0,
                    "observed SQLite table payload growth"
                );
            }
        }
        table_growth_evidence.extend(table_growth_doctor_evidence(&table_growth));
        reads.entry(store.as_str().to_owned()).or_insert(read);
    }

    let mut findings = Vec::new();
    for evidence in &table_growth_evidence {
        let Ok(finding) = table_growth_finding(evidence) else {
            return CollectedStoreTelemetryV1 {
                findings: DoctorStorageFamilyReadV1::Unknown,
                table_growth_evidence,
            };
        };
        findings.push(finding);
    }
    for configured_store in retention.store_soft_budgets_bytes.keys() {
        let Ok(Some(budget)) = retention.store_soft_budget(configured_store) else {
            return CollectedStoreTelemetryV1 {
                findings: DoctorStorageFamilyReadV1::Unknown,
                table_growth_evidence,
            };
        };
        let read =
            reads
                .remove(configured_store)
                .unwrap_or_else(|| StorageTelemetryReadV1::Unknown {
                    store: budget.store.clone(),
                });
        let Ok(finding) =
            over_budget_finding(&budget, &read, DoctorCoverageCompletenessV1::Complete)
        else {
            return CollectedStoreTelemetryV1 {
                findings: DoctorStorageFamilyReadV1::Unknown,
                table_growth_evidence,
            };
        };
        findings.push(finding);
    }
    CollectedStoreTelemetryV1 {
        findings: storage_family_read(findings),
        table_growth_evidence,
    }
}

fn incident_debris_findings_from_census(
    census: &[tracedecay_maintenance::retention::orphan_stores::StoreCensusEntry],
    profile_root: &Path,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    let mut findings = Vec::new();
    for entry in census {
        let Ok(scan) = tracedecay_maintenance::retention::incident_debris::scan_incident_debris(
            entry,
            profile_root,
            observed_at_secs,
        ) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        let Ok(finding) = tracedecay_application::storage::incident_debris_finding(&scan) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
}

/// Read the configured session-retention backlog from the retained session
/// store. This mirrors the retention SQL in read-only form and emits clean
/// zero-byte records when a configured window has no eligible rows.
#[hotpath::measure(label = "daemon.doctor.retention_backlog", future = true)]
pub async fn collect_retention_backlog_findings(
    profile_sessions: &tracedecay_global_db::RegisteredGlobalDb,
    retention: &crate::config::RetentionConfig,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    if !permits_synchronous_session_retention_backlog(profile_sessions.db_path()) {
        return DoctorStorageFamilyReadV1::Unknown;
    }
    let Some(file_name) = profile_sessions
        .db_path()
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(store) = tracedecay_application::storage::StoreKeyV1::new(file_name.to_owned()) else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(snapshot) = profile_sessions.read_snapshot().await else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(records) = tracedecay_lcm::retention::read_session_retention_backlog(
        &snapshot,
        store,
        &retention.session_lcm,
        observed_at_secs,
    )
    .await
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    hotpath::gauge!("daemon.doctor.retention_backlog_records_total").inc(records.len() as u64);
    let mut findings = Vec::new();
    for record in records {
        let Ok(finding) = tracedecay_application::storage::retention_backlog_finding(
            &record,
            DoctorCoverageCompletenessV1::Complete,
        ) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
}

/// Read the exact code-generation liveness plan and surface superseded,
/// collectable, and stranded-scope bytes through Doctor. These are ordinary
/// files, not `SQLite` tables, so dbstat/table attribution cannot observe them.
///
/// The census is metadata-only by construction: gating this family on a byte
/// budget made the finding unreachable on every profile that actually had
/// something to report, because one sealed generation alone exceeds any budget
/// small enough to be called cheap.
#[hotpath::measure(label = "daemon.doctor.code_generation_retention", future = true)]
pub(super) async fn collect_code_generation_retention_findings(
    schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    maintenance_observations: &super::maintenance::StoreTelemetrySamplingRegistry,
    configuration: Option<
        &tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    >,
    code_index_store_root: &Path,
    project_root: &Path,
) -> DoctorStorageFamilyReadV1 {
    use tracedecay_application::storage::{
        CodeGenerationRetentionRecordV1, SemanticVectorRetentionRecordV1, StorageByteSizeV1,
        StoreKeyV1, code_generation_retention_finding, semantic_vector_retention_finding,
    };
    use tracedecay_code_index_retention::code_index_generations::{
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        GenerationDigestVerificationV1, ScopeRootRetentionPlanV1,
        plan_code_generation_retention_with_verification, plan_scope_root_retention,
    };

    if !code_index_store_root
        .join("active-code-generation-v1.json")
        .is_file()
    {
        return DoctorStorageFamilyReadV1::Absent;
    }
    let Some(configuration) = configuration else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let super::maintenance::SemanticVectorRetentionReadV1::Observed {
        receipt: semantic_census,
    } = maintenance_observations.semantic_vector_retention_read(project_root)
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    // Published vectors live in the mounted code graph; without it the
    // protection set cannot be proven and the census reads as Unknown rather
    // than "nothing is pinned".
    let vector_readable_sources =
        match tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::project_vector_readable_sources(
            schedulers,
            project_root,
            configuration,
            semantic_census.revision,
        )
        .await
        {
            tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Ready {
                sources,
                configured_root_receipt,
                ..
            } => (sources, configured_root_receipt.root_count()),
            // Each of these is a NAMED vector-authority degradation carrying the
            // reason the authority reported. Collapsing them into `Unknown`
            // would claim the state could not be determined when it was in fact
            // determined and explained, so each keeps its name and its reason.
            tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Unavailable(
                detail,
            ) => return DoctorStorageFamilyReadV1::Unavailable { detail },
            tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::ResetRequired(
                detail,
            ) => return DoctorStorageFamilyReadV1::ResetRequired { detail },
            tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Corrupt(
                detail,
            ) => return DoctorStorageFamilyReadV1::Corrupt { detail },
            tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Denied(
                _,
            ) => return DoctorStorageFamilyReadV1::Denied,
        };
    let (vector_readable_sources, retained_vector_root_count) = vector_readable_sources;
    let semantic_backlog =
        super::maintenance::SemanticVectorRetentionBacklogV1::from_receipt(&semantic_census);
    if semantic_backlog.published < retained_vector_root_count {
        return DoctorStorageFamilyReadV1::Unknown;
    }
    let Ok(semantic_store) = StoreKeyV1::new("semantic-vector-graph") else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let semantic_record = SemanticVectorRetentionRecordV1 {
        store: semantic_store,
        pending_generation_count: semantic_backlog.pending,
        ready_generation_count: semantic_backlog.ready,
        observed_non_configured_published_generation_count: semantic_backlog
            .published
            .saturating_sub(retained_vector_root_count),
        cancelled_generation_count: semantic_backlog.cancelled,
    };
    let semantic_completeness = DoctorCoverageCompletenessV1::Complete;
    let Ok(semantic_finding) =
        semantic_vector_retention_finding(&semantic_record, semantic_completeness)
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let vector_liveness_incomplete = semantic_record.has_backlog()
        || semantic_record.has_in_flight_generations()
        || semantic_record.observed_non_configured_published_generation_count > 0;
    let semantic_only_unknown = || DoctorStorageFamilyReadV1::ObservedIncomplete {
        findings: vec![semantic_finding.clone()],
        reason: DoctorStorageIncompleteReasonV1::Unknown,
    };
    if !permits_synchronous_generation_census(&code_index_store_root.join("code-generations-v1")) {
        return semantic_only_unknown();
    }
    let root = code_index_store_root.to_path_buf();
    // The shared parent that holds every scope root for this repository. A
    // stranded sibling scope is invisible to the scope-local census above, so
    // it is measured here or it is not measured anywhere.
    let scope_store_root = code_index_store_root.parent().map(Path::to_path_buf);
    let project_root = project_root.to_path_buf();
    let now = now_secs();
    let Ok(census) = tokio::task::spawn_blocking(move || {
        let plan = plan_code_generation_retention_with_verification(
            &root,
            &vector_readable_sources,
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            GenerationDigestVerificationV1::MetadataOnly,
        );
        // Zeros are only ever published together with `Partial`: a live-root set
        // that could not be proven must never read as "nothing is stranded".
        let scopes = scope_store_root.and_then(|scope_store_root| {
            let live_roots =
                super::store_maintenance::resolve_live_code_index_roots(&project_root).ok()?;
            plan_scope_root_retention(
                &scope_store_root,
                &live_roots,
                DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
                now,
            )
            .ok()
        });
        (plan, scopes)
    })
    .await
    else {
        return semantic_only_unknown();
    };
    let (plan, scopes) = census;
    let Ok(plan) = plan else {
        return semantic_only_unknown();
    };
    let Ok(store) = StoreKeyV1::new("code-index-v1") else {
        return semantic_only_unknown();
    };
    let completeness = if scopes.is_some() && !vector_liveness_incomplete {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    };
    let record = CodeGenerationRetentionRecordV1 {
        store,
        superseded_generation_count: plan.superseded_generations.len() as u64,
        superseded_generation_bytes: StorageByteSizeV1(plan.superseded_generation_bytes()),
        collectable_generation_count: if vector_liveness_incomplete {
            0
        } else {
            plan.collectable_generations.len() as u64
        },
        collectable_generation_bytes: if vector_liveness_incomplete {
            StorageByteSizeV1::ZERO
        } else {
            StorageByteSizeV1(plan.collectable_generation_bytes())
        },
        stranded_scope_count: if vector_liveness_incomplete {
            0
        } else {
            scopes
                .as_ref()
                .map_or(0, ScopeRootRetentionPlanV1::stranded_scope_count)
        },
        stranded_scope_bytes: if vector_liveness_incomplete {
            StorageByteSizeV1::ZERO
        } else {
            StorageByteSizeV1(
                scopes
                    .as_ref()
                    .map_or(0, ScopeRootRetentionPlanV1::stranded_scope_bytes),
            )
        },
    };
    let Ok(finding) = code_generation_retention_finding(&record, completeness) else {
        return semantic_only_unknown();
    };
    let findings = vec![semantic_finding, finding];
    if vector_liveness_incomplete {
        DoctorStorageFamilyReadV1::ObservedIncomplete {
            findings,
            reason: DoctorStorageIncompleteReasonV1::Unknown,
        }
    } else {
        storage_family_read(findings)
    }
}

/// Live provider of the Remote Brain operational read. Every Doctor read
/// re-observes the mounted remote authorities instead of freezing one value
/// at project-composition time.
pub(in crate::daemon) type RemoteOperationalReadProviderV1 =
    Arc<dyn Fn() -> RemoteOperationalReadV1 + Send + Sync>;

/// Build the daemon-owned live Doctor reader installed into a project MCP
/// server. Every read re-resolves exact project/worktree identity, observes the
/// current registered runtimes, and composes through the sole application
/// kernel. The dashboard receives no database handles or authority-bearing
/// inputs.
#[allow(clippy::too_many_arguments)]
pub(in crate::daemon) fn production_doctor_report_reader(
    project_root: PathBuf,
    project_id: tracedecay_domain::ProjectId,
    layout: tracedecay_runtime_core::storage::StoreLayout,
    graph: tracedecay_runtime_core::db::Database,
    registry: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    profile_sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    project_sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    profile_root: PathBuf,
    host_home: Option<PathBuf>,
    remote_operational: RemoteOperationalReadProviderV1,
    retention: crate::config::RetentionConfig,
    schedulers: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    diagnostic_broker: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    feedback_runtimes: DaemonFeedbackRuntimeRegistrar,
    store_telemetry_sampling: super::maintenance::StoreTelemetrySamplingRegistry,
    configuration_runtime: Arc<tracedecay_configuration::ProjectConfigurationRuntime>,
) -> tracedecay_dashboard_api::DoctorReportReader {
    Arc::new(move || {
        let project_root = project_root.clone();
        let project_id = project_id.clone();
        let layout = layout.clone();
        let graph = graph.clone();
        let registry = registry.clone();
        let profile_sessions = profile_sessions.clone();
        let project_sessions = project_sessions.clone();
        let profile_root = profile_root.clone();
        let host_home = host_home.clone();
        let remote_operational = Arc::clone(&remote_operational);
        let retention = retention.clone();
        let schedulers = schedulers.clone();
        let diagnostic_broker = Arc::clone(&diagnostic_broker);
        let feedback_runtimes = feedback_runtimes.clone();
        let store_telemetry_sampling = store_telemetry_sampling.clone();
        let configuration_runtime = Arc::clone(&configuration_runtime);
        Box::pin(async move {
            let scope = tracedecay_code_index_runtime::resolved_scope_for_project(
                &project_root,
                &project_id,
            )
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "daemon Doctor project scope",
            })?;
            let context = doctor_report_request_context(scope)?;
            let mut telemetry_ports = Vec::new();
            let mut telemetry_paths = BTreeSet::new();
            if telemetry_paths.insert(graph.database_path().to_path_buf())
                && let Some(port) =
                    store_telemetry_sampling.registered_port(graph.database_path(), context.scope())
            {
                telemetry_ports.push(port);
            }
            for database in [
                registry.as_ref(),
                profile_sessions.as_ref(),
                project_sessions.as_ref(),
            ] {
                if telemetry_paths.insert(database.db_path().to_path_buf())
                    && let Some(port) = store_telemetry_sampling
                        .registered_port(database.db_path(), context.scope())
                {
                    telemetry_ports.push(port);
                }
            }
            let pinned = crate::config::runtime_configuration_for_layout(&project_root, &layout);
            let graph_authority_current = graph.write_authority().is_ok_and(|authority| {
                authority
                    .require_active_write_scope("read dashboard Doctor graph authority")
                    .is_ok()
            });
            let registered_authority_current = registry.writer_connection().is_ok()
                && profile_sessions.writer_connection().is_ok();
            let retention_secs = retention
                .orphan_store_gc_days
                .and_then(|days| i64::try_from(days).ok())
                .and_then(|days| days.checked_mul(24 * 60 * 60))
                .unwrap_or(i64::MAX);
            let now = now_secs();
            let profile_scan_root = profile_root.join("projects");
            let profile_storage_reads = async {
                // The admission walk stats every store file under the profile;
                // its own wall span separates filesystem-walk cost from the
                // census reads it admits.
                let permitted = tokio::task::spawn_blocking(move || {
                    hotpath::measure_block!(
                        "daemon.doctor.profile_scan",
                        permits_synchronous_exhaustive_scan(&profile_scan_root)
                    )
                })
                .await
                .is_ok_and(|permitted| permitted);
                if !permitted {
                    return (None, DoctorStorageFamilyReadV1::Unknown);
                }
                let (registered_census, unregistered) = tokio::join!(
                    tracedecay_maintenance::retention::orphan_stores::build_store_census(
                        registry.as_ref(),
                        &profile_root,
                    ),
                    collect_unregistered_store_findings(
                        registry.as_ref(),
                        &profile_root,
                        retention_secs,
                        now,
                    ),
                );
                (registered_census.ok(), unregistered)
            };
            let code_index_store_root =
                tracedecay_code_index_runtime::code_index_scheduler::scoped_code_index_store_root(
                    &layout.data_root.join("code-index-v1"),
                    &project_root,
                );
            let advisory_feedback_read = async {
                let current_generation = schedulers
                    .latest_complete_ready(&project_root)
                    .await
                    .map(|latest| latest.generation().manifest().generation_id.clone());
                match feedback_runtimes.doctor_read_store(&project_root).await {
                    Some(store) => match store.doctor_latest_publication(&context).await {
                        Ok(publication) => advisory_feedback_read_from_publication(
                            publication.as_ref(),
                            current_generation.as_ref(),
                        ),
                        Err(_) => AdvisoryFeedbackReadV1::Unknown,
                    },
                    None => AdvisoryFeedbackReadV1::Absent,
                }
            };
            let host_project_root = project_root.clone();
            let host_components_root = profile_root.join("host-components");
            // Staleness comparison against the installed plugins' provenance
            // headers requires this binary's exact generator commit.
            let generator_commit = crate::product_runtime::product_runtime()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "registered product runtime source provenance",
                })?
                .source()
                .full_sha;
            let host_scan = tokio::task::spawn_blocking(move || {
                hotpath::measure_block!("daemon.doctor.host_scan", {
                    host_home
                        .as_ref()
                        .map_or(HostIntegrationReadV1::Unsupported, |home| {
                            let context = crate::agents::HealthcheckContext {
                                home: home.clone(),
                                project_path: host_project_root,
                            };
                            crate::agents::inspect_receipt_backed_host_components(
                                &context,
                                &host_components_root,
                                generator_commit,
                            )
                            .as_ref()
                            .map_or(
                                HostIntegrationReadV1::Unknown,
                                host_integration_read_from_report,
                            )
                        })
                })
            });
            let semantic_configuration_inventory =
                configuration_runtime.semantic_configuration_inventory_authority();
            let (
                quick_check,
                authority_audit_ok,
                temporal,
                (registered_census, unregistered),
                store_telemetry,
                profile_retention_backlog,
                project_retention_backlog,
                code_generation_retention,
                language_server,
                observability_read,
                (profile_refusal_census, project_refusal_census),
                advisory_feedback,
                host_read,
                code_index,
            ) =
                hotpath::future!(
                    async {
                        tokio::join!(
                    graph.quick_check_report(),
                    observation_authority_audit_ok(registry.as_ref()),
                    project_sessions.session_temporal_doctor_health(),
                    profile_storage_reads,
                    collect_over_budget_store_findings(&context, &telemetry_ports, &retention),
                    collect_retention_backlog_findings(profile_sessions.as_ref(), &retention, now),
                    collect_retention_backlog_findings(project_sessions.as_ref(), &retention, now),
                    collect_code_generation_retention_findings(
                        &schedulers,
                        &store_telemetry_sampling,
                        semantic_configuration_inventory.as_ref(),
                        &code_index_store_root,
                        &project_root,
                    ),
                    language_server_read_from_broker(&diagnostic_broker),
                    tracedecay_usecases::feedback::concrete::feedback_observation_read_model(
                        &graph,
                    ),
                    async {
                        tokio::join!(
                            profile_sessions.observation_refusal_census(),
                            project_sessions.observation_refusal_census(),
                        )
                    },
                    advisory_feedback_read,
                    host_scan,
                    code_index_read_from_registry(&schedulers, &project_root),
                )
                    },
                    label = "daemon.doctor.collect"
                )
                .await;
            let quick_check_ok = quick_check.ok().map(|problem| problem.is_none());
            let temporal_ok = match temporal.status() {
                tracedecay_session_temporal_store::SessionTemporalHealthStatus::Complete => {
                    Some(temporal.findings().is_empty())
                }
                tracedecay_session_temporal_store::SessionTemporalHealthStatus::Partial
                | tracedecay_session_temporal_store::SessionTemporalHealthStatus::Unavailable
                | tracedecay_session_temporal_store::SessionTemporalHealthStatus::Locked => None,
            };
            let (orphan, incident_debris) = registered_census.as_deref().map_or(
                (
                    DoctorStorageFamilyReadV1::Unknown,
                    DoctorStorageFamilyReadV1::Unknown,
                ),
                |census| {
                    (
                        orphan_store_findings_from_census(census, retention_secs, now),
                        incident_debris_findings_from_census(census, &profile_root, now),
                    )
                },
            );
            let storage = [
                orphan,
                unregistered,
                store_telemetry.findings,
                incident_debris,
                profile_retention_backlog,
                project_retention_backlog,
                code_generation_retention,
            ]
            .into_iter()
            .reduce(merge_storage_reads)
            .unwrap_or(DoctorStorageFamilyReadV1::Absent);
            let observability = observability_read_from_model(observability_read);
            let ingest_refusals = ingest_refusal_read_from_censuses(&[
                profile_refusal_census,
                project_refusal_census,
            ]);
            let host = match host_read {
                Ok(read) => read,
                Err(_) => HostIntegrationReadV1::Unknown,
            };
            let inputs = DoctorKernelInputsV1 {
                configuration: configuration_read_from_pin::<
                    tracedecay_domain::errors::TraceDecayError,
                >(&pinned),
                runtime: runtime_health_read(&DaemonRuntimeHealthSignalV1 {
                    serving: true,
                    startup_converged: graph_authority_current && registered_authority_current,
                    quick_check_ok,
                    // The exhaustive invariant pass
                    // (`validate_observation_authority_connection`) observed just
                    // above, never a boolean re-derived from schema and write-scope
                    // currency — that is a different question and is already
                    // reported through `startup_converged`. `None` here means the
                    // audit genuinely could not run and drops runtime coverage to
                    // partial, exactly as the coverage split intends.
                    authority_audit_ok,
                    temporal_ok,
                }),
                operational_audit: OperationalAuditReadV1 {
                    remote: remote_operational(),
                    profile_authority: ProfileAuthorityReadV1::Observed {
                        registry_attached: registry.writer_connection().is_ok(),
                        profile_sessions_attached: profile_sessions.writer_connection().is_ok(),
                        coverage: DoctorCoverageCompletenessV1::Complete,
                    },
                },
                host,
                advisory_feedback,
                language_server,
                code_index,
                observability,
                ingest_refusals,
                storage,
            };
            let report = compose_doctor_report(&context, &inputs).await?;
            Ok(
                tracedecay_dashboard_api::AdmittedDoctorReportV1::new(report)
                    .with_table_growth_evidence(store_telemetry.table_growth_evidence),
            )
        })
    })
}

pub(crate) fn doctor_report_request_context(
    scope: tracedecay_application::ResolvedScope,
) -> Result<RequestContext, ApplicationContractError> {
    let observed_at = now_micros();
    let expires_at =
        tracedecay_domain::UtcMicros(observed_at.0.saturating_add(DOCTOR_CONTEXT_HORIZON_MICROS));
    let request_id = mint_global_request_id(GlobalRequestSurface::DaemonDoctor).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "doctor report request identity",
        }
    })?;
    let suffix = request_id.as_str().to_owned();
    let actor = tracedecay_domain::ActorId::new("actor.tracedecay-daemon")?;
    let capability =
        tracedecay_tool_catalog::CapabilityId::new(DOCTOR_REPORT_CAPABILITY.to_owned())?;
    let use_case = tracedecay_tool_catalog::UseCaseId::new(DOCTOR_REPORT_USE_CASE.to_owned())?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.doctor.{suffix}"))?,
        1,
        tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon.doctor-report-grant.v1",
            &scope,
            &capability,
            &use_case,
            expires_at,
        ))?,
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Metadata,
    )?;
    RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at)?,
        CancellationContext::active(format!("cancel.daemon.doctor.{suffix}"))?,
    )
}

fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
    )
    .unwrap_or(i64::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
