//! Retention over one registered session store.

use tracedecay_global_db::observation::retention::{
    ObservationRetentionConfig, ObservationRetentionReport, RetentionMode,
};
use tracedecay_global_db::{ObservabilityRetentionReceiptV1, RegisteredGlobalDb};
use tracedecay_lcm::{
    LcmGcConfig, LcmGcReport, LcmRetentionConfig, LcmRetentionReport,
    RetentionMode as LcmRetentionMode,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisteredStoreRetentionErrorV1 {
    Observability { diagnostic: String },
    PayloadGc { diagnostic: String },
}

impl RegisteredStoreRetentionErrorV1 {
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        match self {
            Self::Observability { diagnostic } | Self::PayloadGc { diagnostic } => diagnostic,
        }
    }
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
    }
}

/// Runs every registered-store retention kernel without owning cadence,
/// writer admission, or process logging.
#[hotpath::measure(label = "maintenance.registered_store.retention", future = true)]
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
                .run_observation_retention(None, observations, RetentionMode::Apply, now)
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
    RegisteredStoreRetentionReportV1 {
        session_lcm,
        observations,
        observability,
        payload_gc,
    }
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

    #[tokio::test]
    async fn registered_retention_always_prunes_observability_analytics() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
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

    #[tokio::test]
    async fn registered_retention_reaps_stale_orphan_payloads_in_place() {
        const NOW: i64 = 40 * 86_400;
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
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

    #[tokio::test]
    async fn observability_retention_failure_preserves_typed_owner_diagnostic() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
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
}
