//! Retention over one registered session store.

use tracedecay_domain::UtcMicros;
use tracedecay_global_db::observation::retention::{
    ObservationRetentionConfig, ObservationRetentionReport, RetentionMode,
};
use tracedecay_global_db::observation::{CanonicalBodyGcReport, MAX_CANONICAL_BODY_GC_ROWS};
use tracedecay_global_db::{
    GITHUB_STACK_TERMINAL_RETENTION_DAYS_V1, ObservabilityRetentionReceiptV1, RegisteredGlobalDb,
};
use tracedecay_lcm::retention::{
    MAX_REPLAY_METADATA_COMPACTION_ROWS, ReplayMetadataCompactionReport,
};
use tracedecay_lcm::{
    LcmGcConfig, LcmGcReport, LcmRetentionConfig, LcmRetentionReport,
    RetentionMode as LcmRetentionMode,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisteredStoreRetentionErrorV1 {
    Observability { diagnostic: String },
    PayloadGc { diagnostic: String },
    CanonicalBodyGc { diagnostic: String },
    ReplayMetadataCompaction { diagnostic: String },
    GitHubStackTerminal { diagnostic: String },
}

impl RegisteredStoreRetentionErrorV1 {
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        match self {
            Self::Observability { diagnostic }
            | Self::PayloadGc { diagnostic }
            | Self::CanonicalBodyGc { diagnostic }
            | Self::ReplayMetadataCompaction { diagnostic }
            | Self::GitHubStackTerminal { diagnostic } => diagnostic,
        }
    }
}

/// Most bounded retirement transactions one retention pass runs per store.
/// Each is its own registered transaction, so the store's writer is released
/// between pages even though the daemon admits the pass once.
const MAX_GITHUB_STACK_RETIRE_PAGES_PER_PASS: usize = 64;

/// Outcome of the bounded terminal GitHub stack delivery pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitHubStackTerminalRetentionReportV1 {
    pub retired: usize,
    /// False when the page budget ended the pass first. The durable frontier
    /// is kept, so the next pass resumes instead of rescanning.
    pub exhausted: bool,
}

/// Typed outcomes from every independent retention authority consulted for a
/// registered store. Disabled policies never acquire their writer.
pub struct RegisteredStoreRetentionReportV1 {
    pub session_lcm: Option<tracedecay_domain::errors::Result<LcmRetentionReport>>,
    pub observations: Option<tracedecay_domain::errors::Result<ObservationRetentionReport>>,
    pub observability: Result<ObservabilityRetentionReceiptV1, RegisteredStoreRetentionErrorV1>,
    /// Applied external-payload GC. It runs after session retention so the
    /// payloads of rows that pass just dropped are reconciled by the same tick.
    pub payload_gc: Result<LcmGcReport, RegisteredStoreRetentionErrorV1>,
    /// Shared canonical bodies are collected only after both owner indexes
    /// and the initial candidate seed are complete.
    pub canonical_body_gc: Result<CanonicalBodyGcReport, RegisteredStoreRetentionErrorV1>,
    /// Lossless repair of old provider replay strings duplicated by raw bodies.
    pub replay_metadata: Result<ReplayMetadataCompactionReport, RegisteredStoreRetentionErrorV1>,
    /// Completed GitHub stack delivery payloads past their fixed inspection
    /// window. Their window is a product invariant, not an operator policy, so
    /// this runs for every store whenever store retention runs, independent of
    /// automation jobs.
    pub github_stack_terminal:
        Result<GitHubStackTerminalRetentionReportV1, RegisteredStoreRetentionErrorV1>,
}

impl RegisteredStoreRetentionReportV1 {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        let session_lcm_succeeded = self
            .session_lcm
            .as_ref()
            .is_none_or(|result| result.as_ref().is_ok_and(|report| report.errors.is_empty()));
        let observations_succeeded = self
            .observations
            .as_ref()
            .is_none_or(|result| result.as_ref().is_ok_and(|report| report.errors.is_empty()));
        let payload_gc_succeeded = self
            .payload_gc
            .as_ref()
            .is_ok_and(|report| report.errors.is_empty());
        session_lcm_succeeded
            && observations_succeeded
            && self.observability.is_ok()
            && payload_gc_succeeded
            && self.canonical_body_gc.is_ok()
            && self.replay_metadata.is_ok()
            && self
                .github_stack_terminal
                .as_ref()
                .is_ok_and(|report| report.exhausted)
    }
}

/// Runs every registered-store retention kernel without owning cadence,
/// writer admission, or process logging.
#[tracing::instrument(
    name = "maintenance.registered_store.retention",
    level = "trace",
    skip_all
)]
pub async fn run_registered_store_retention(
    database: &RegisteredGlobalDb,
    session_lcm: &LcmRetentionConfig,
    observations: &ObservationRetentionConfig,
    now: i64,
) -> RegisteredStoreRetentionReportV1 {
    let session_lcm = if session_lcm.enabled {
        Some(
            database
                .run_session_lcm_retention("all", None, session_lcm, LcmRetentionMode::Apply, now)
                .await,
        )
    } else {
        None
    };
    let observations = if observations.enabled {
        Some(
            database
                .run_observation_retention(
                    None,
                    observations,
                    RetentionMode::Apply,
                    UtcMicros::from_unix_seconds(now),
                )
                .await,
        )
    } else {
        None
    };
    let observability = database
        .prune_observability_events(now)
        .await
        .map_err(|diagnostic| RegisteredStoreRetentionErrorV1::Observability { diagnostic });
    let payload_gc = run_payload_gc(database, now).await;
    let canonical_body_gc = run_canonical_body_gc(database).await;
    let replay_metadata = run_replay_metadata_compaction(database).await;
    let github_stack_terminal = run_github_stack_terminal_retention(database, now).await;
    RegisteredStoreRetentionReportV1 {
        session_lcm,
        observations,
        observability,
        payload_gc,
        canonical_body_gc,
        replay_metadata,
        github_stack_terminal,
    }
}

/// Retire completed stack delivery payloads from one registered store through
/// bounded pages. A page examines and compacts a bounded number of payloads
/// and commits its durable scan frontier atomically, so a long pending prefix
/// is neither rescanned nor allowed to starve later terminal rows.
async fn run_github_stack_terminal_retention(
    database: &RegisteredGlobalDb,
    now: i64,
) -> Result<GitHubStackTerminalRetentionReportV1, RegisteredStoreRetentionErrorV1> {
    let cutoff_micros = now
        .saturating_sub(GITHUB_STACK_TERMINAL_RETENTION_DAYS_V1 * 86_400)
        .saturating_mul(1_000_000);
    let mut retired = 0usize;
    let mut exhausted = false;
    for _ in 0..MAX_GITHUB_STACK_RETIRE_PAGES_PER_PASS {
        let page = match database
            .retire_github_stack_terminal_payloads_before(cutoff_micros)
            .await
        {
            Ok(page) => page,
            Err(diagnostic) => {
                tracing::warn!(
                    event = "retention_degraded",
                    pass = "github_stack_terminal",
                    failure = %diagnostic,
                );
                return Err(RegisteredStoreRetentionErrorV1::GitHubStackTerminal { diagnostic });
            }
        };
        retired = retired.saturating_add(page.retired);
        if page.exhausted {
            exhausted = true;
            break;
        }
    }
    if retired > 0 {
        tracing::info!(
            event = "retention_prune",
            table = "github_stack_delivery_signals",
            signals = retired,
            window_days = GITHUB_STACK_TERMINAL_RETENTION_DAYS_V1,
        );
    }
    Ok(GitHubStackTerminalRetentionReportV1 { retired, exhausted })
}

/// Resume bounded, lossless storage work without repeating the raw-drop policy.
pub async fn run_session_storage_compaction(
    database: &RegisteredGlobalDb,
    config: &LcmRetentionConfig,
    now: i64,
) -> Option<tracedecay_domain::errors::Result<LcmRetentionReport>> {
    if !config.enabled || config.offload_after_days.is_none() {
        return None;
    }
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..*config
    };
    Some(
        database
            .run_session_lcm_retention("all", None, &config, LcmRetentionMode::Apply, now)
            .await,
    )
}

/// Continuation ticks advance the bounded body lifecycle, leaving destructive
/// age policies and profile-wide maintenance on their normal cadence.
pub async fn run_canonical_body_gc(
    database: &RegisteredGlobalDb,
) -> Result<CanonicalBodyGcReport, RegisteredStoreRetentionErrorV1> {
    database
        .run_canonical_body_gc(MAX_CANONICAL_BODY_GC_ROWS)
        .await
        .map_err(|error| RegisteredStoreRetentionErrorV1::CanonicalBodyGc {
            diagnostic: error.to_string(),
        })
}

/// Runs a bounded replay-metadata repair page without repeating age policies.
pub async fn run_replay_metadata_compaction(
    database: &RegisteredGlobalDb,
) -> Result<ReplayMetadataCompactionReport, RegisteredStoreRetentionErrorV1> {
    database
        .compact_replay_metadata(MAX_REPLAY_METADATA_COMPACTION_ROWS)
        .await
        .map_err(
            |error| RegisteredStoreRetentionErrorV1::ReplayMetadataCompaction {
                diagnostic: error.to_string(),
            },
        )
}

async fn run_payload_gc(
    database: &RegisteredGlobalDb,
    now: i64,
) -> Result<LcmGcReport, RegisteredStoreRetentionErrorV1> {
    let storage_root =
        database
            .db_path()
            .parent()
            .ok_or_else(|| RegisteredStoreRetentionErrorV1::PayloadGc {
                diagnostic: "registered sessions database has no storage root".to_owned(),
            })?;
    database
        .lcm_run_payload_gc_apply(storage_root, "all", None, &LcmGcConfig::default(), now)
        .await
        .map_err(|error| RegisteredStoreRetentionErrorV1::PayloadGc {
            diagnostic: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use tracedecay_global_db::{AnalyticsEventInsert, AnalyticsEventQuery};

    use super::*;

    #[asupersync::test]
    async fn registered_retention_always_prunes_observability_analytics() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "maintenance-registered-observability-retention",
        )
        .await;
        harness
            .registered
            .append_observability_event(&AnalyticsEventInsert {
                provider: "tracedecay-observability".to_owned(),
                project_id: "scope:retention".to_owned(),
                session_id: None,
                timestamp: 0,
                event_kind: "retrieval.query.completed.v1".to_owned(),
                hook_name: None,
                tool_name: None,
                tool_category: None,
                skill_name: None,
                hint_category: None,
                hint_id: Some("retention:event:1".to_owned()),
                outcome: Some("succeeded".to_owned()),
                metadata_json: Some(
                    serde_json::json!({
                        "retention_class": "optional_local_detail30d"
                    })
                    .to_string(),
                ),
            })
            .await
            .expect("append old observability detail");
        let session_lcm = LcmRetentionConfig {
            enabled: false,
            ..LcmRetentionConfig::default()
        };
        let observations = ObservationRetentionConfig {
            enabled: false,
            ..ObservationRetentionConfig::default()
        };

        let storage = run_session_storage_compaction(
            &harness.registered,
            &LcmRetentionConfig {
                drop_after_days: Some(0),
                ..LcmRetentionConfig::default()
            },
            31 * 86_400,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(storage.dropped.window_days, None);
        assert_eq!(storage.dropped.acted, 0);
        assert!(
            run_session_storage_compaction(&harness.registered, &session_lcm, 31 * 86_400)
                .await
                .is_none(),
            "disabling session retention also stops its storage continuation"
        );
        assert_eq!(
            harness
                .registered
                .query_analytics_events(&AnalyticsEventQuery {
                    provider: Some("tracedecay-observability".to_owned()),
                    project_id: Some("scope:retention".to_owned()),
                    limit: 10,
                    ..AnalyticsEventQuery::default()
                })
                .await
                .unwrap()
                .len(),
            1,
            "storage continuation must not run profile-wide age policies"
        );

        let report = run_registered_store_retention(
            &harness.registered,
            &session_lcm,
            &observations,
            31 * 86_400,
        )
        .await;

        assert!(report.succeeded());
        let rows = harness
            .registered
            .query_analytics_events(&AnalyticsEventQuery {
                provider: Some("tracedecay-observability".to_owned()),
                project_id: Some("scope:retention".to_owned()),
                limit: 10,
                ..AnalyticsEventQuery::default()
            })
            .await
            .expect("query retained observability detail");
        assert!(
            rows.is_empty(),
            "registered maintenance must invoke analytics retention"
        );
    }

    #[asupersync::test]
    async fn registered_retention_reaps_stale_orphan_payloads_in_place() {
        const NOW: i64 = 40 * 86_400;
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "maintenance-registered-payload-gc",
        )
        .await;
        let storage_root = harness.registered.db_path().parent().unwrap().to_path_buf();
        let orphan = |message_id: &str, mtime: i64| {
            let payload = tracedecay_lcm::payload::write_external_payload(
                &storage_root,
                "codex",
                "session-gc",
                message_id,
                "message",
                "externalized body with no remaining reference",
                None,
            )
            .unwrap();
            let path =
                tracedecay_lcm::payload::payload_dir(&storage_root).join(payload.payload_ref);
            filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(mtime, 0)).unwrap();
            path
        };
        let stale = orphan("message-stale", NOW - 86_400);
        let fresh = orphan("message-fresh", NOW - 60);
        let session_lcm = LcmRetentionConfig {
            enabled: false,
            ..LcmRetentionConfig::default()
        };
        let observations = ObservationRetentionConfig {
            enabled: false,
            ..ObservationRetentionConfig::default()
        };

        let report =
            run_registered_store_retention(&harness.registered, &session_lcm, &observations, NOW)
                .await;

        assert!(!stale.exists(), "an orphan past its grace window is reaped");
        assert!(fresh.exists(), "an orphan inside its grace window is kept");
        assert!(report.succeeded());
    }

    /// `lcm_status` reads the store the retention tick cleaned. A profile with
    /// orphaned payloads but no sessions still reports the tick's GC record
    /// and the payload directory it scanned.
    #[asupersync::test]
    async fn lcm_status_reports_the_payload_gc_the_retention_tick_recorded() {
        const NOW: i64 = 40 * 86_400;
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "maintenance-registered-payload-gc-status",
        )
        .await;
        let storage_root = harness.registered.db_path().parent().unwrap().to_path_buf();
        for (message_id, mtime) in [("message-stale", NOW - 86_400), ("message-fresh", NOW - 60)] {
            let payload = tracedecay_lcm::payload::write_external_payload(
                &storage_root,
                "codex",
                "session-gc",
                message_id,
                "message",
                "externalized body with no remaining reference",
                None,
            )
            .unwrap();
            let path =
                tracedecay_lcm::payload::payload_dir(&storage_root).join(payload.payload_ref);
            filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(mtime, 0)).unwrap();
        }
        let status = || async {
            let status = harness
                .registered
                .lcm_status_with_options("all", None, true, &LcmGcConfig::default())
                .await
                .unwrap();
            (
                status.payload_gc.last_gc_status,
                status.payload_gc.last_reaped_refs,
                status.payload.coverage.scanned_files,
            )
        };
        let (before_status, before_reaped, _) = status().await;
        assert_eq!((before_status, before_reaped), (None, None));

        let report = run_registered_store_retention(
            &harness.registered,
            &LcmRetentionConfig {
                enabled: false,
                ..LcmRetentionConfig::default()
            },
            &ObservationRetentionConfig {
                enabled: false,
                ..ObservationRetentionConfig::default()
            },
            NOW,
        )
        .await;

        assert!(report.succeeded());
        assert_eq!(status().await, (Some("ok".to_owned()), Some(1), 1));
    }

    #[asupersync::test]
    async fn observability_retention_failure_preserves_typed_owner_diagnostic() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "maintenance-registered-observability-retention-error",
        )
        .await;
        let session_lcm = LcmRetentionConfig {
            enabled: false,
            ..LcmRetentionConfig::default()
        };
        let observations = ObservationRetentionConfig {
            enabled: false,
            ..ObservationRetentionConfig::default()
        };

        let report =
            run_registered_store_retention(&harness.registered, &session_lcm, &observations, -1)
                .await;

        assert_eq!(
            report.observability,
            Err(RegisteredStoreRetentionErrorV1::Observability {
                diagnostic: "invalid observability retention time".to_owned(),
            })
        );
        assert!(!report.succeeded());
    }

    /// Stack deliveries live in each registered session store, so the
    /// store-wide pass must retire them there with every operator retention
    /// policy disabled. Pending history and a still-open window stay intact.
    #[asupersync::test]
    async fn registered_retention_retires_only_expired_settled_stack_payloads() {
        const NOW: i64 = 40 * 86_400;
        const PROJECT: &str = "project.stack-retention";
        const RECIPIENT: &str = "actor.stack-retention";
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "maintenance-registered-stack-terminal-retention",
        )
        .await;
        let record = |signal_id: &str, observed_at_micros: i64| {
            tracedecay_global_db::GitHubStackSignalRecordV1 {
                project_id: PROJECT.to_owned(),
                signal_id: signal_id.to_owned(),
                scope_digest: "sha256:stack-retention-scope".to_owned(),
                repository_id: "repository.stack-retention".to_owned(),
                watermark_id: "watermark.stack-retention".to_owned(),
                observed_at_micros,
                signal_json: format!(r#"{{"payload":"{signal_id}"}}"#),
            }
        };
        let in_window = (NOW - 86_400) * 1_000_000;
        for (signal_id, observed_at, settle) in [
            ("signal.expired-settled", 10, true),
            ("signal.expired-pending", 11, false),
            ("signal.in-window-settled", in_window, true),
        ] {
            harness
                .registered
                .append_github_stack_signal(
                    record(signal_id, observed_at),
                    vec![RECIPIENT.to_owned()],
                )
                .await
                .expect("persist stack signal");
            if settle {
                harness
                    .registered
                    .acknowledge_github_stack_host_delivery(PROJECT, signal_id, RECIPIENT)
                    .await
                    .expect("settle stack signal");
            }
        }
        let session_lcm = LcmRetentionConfig {
            enabled: false,
            ..LcmRetentionConfig::default()
        };
        let observations = ObservationRetentionConfig {
            enabled: false,
            ..ObservationRetentionConfig::default()
        };

        let report =
            run_registered_store_retention(&harness.registered, &session_lcm, &observations, NOW)
                .await;

        assert_eq!(
            report.github_stack_terminal,
            Ok(GitHubStackTerminalRetentionReportV1 {
                retired: 1,
                exhausted: true,
            })
        );
        assert!(report.succeeded());
        let lookup = |signal_id: &'static str| {
            let database = harness.registered.clone();
            async move {
                database
                    .github_stack_signal(PROJECT, signal_id)
                    .await
                    .expect("read stack signal")
            }
        };
        assert!(
            lookup("signal.expired-settled").await.is_none(),
            "an expired settled payload is compacted"
        );
        assert!(
            lookup("signal.expired-pending").await.is_some(),
            "an undelivered signal is never retired by age"
        );
        assert!(
            lookup("signal.in-window-settled").await.is_some(),
            "a settled signal inside the inspection window is kept"
        );
    }
}
