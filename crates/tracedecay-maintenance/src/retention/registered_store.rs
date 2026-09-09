//! Retention over one registered session store.

use tracedecay_global_db::observation::retention::{
    ObservationRetentionConfig, ObservationRetentionReport, RetentionMode,
};
use tracedecay_global_db::{ObservabilityRetentionReceiptV1, RegisteredGlobalDb};
use tracedecay_lcm::{LcmRetentionConfig, LcmRetentionReport, RetentionMode as LcmRetentionMode};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisteredStoreRetentionErrorV1 {
    Observability { diagnostic: String },
}

impl RegisteredStoreRetentionErrorV1 {
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        match self {
            Self::Observability { diagnostic } => diagnostic,
        }
    }
}

/// Typed outcomes from every independent retention authority consulted for a
/// registered store. Disabled policies never acquire their writer.
pub struct RegisteredStoreRetentionReportV1 {
    pub session_lcm: Option<tracedecay_domain::errors::Result<LcmRetentionReport>>,
    pub observations: Option<tracedecay_domain::errors::Result<ObservationRetentionReport>>,
    pub observability: Result<ObservabilityRetentionReceiptV1, RegisteredStoreRetentionErrorV1>,
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
        session_lcm_succeeded && observations_succeeded && self.observability.is_ok()
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
    RegisteredStoreRetentionReportV1 {
        session_lcm,
        observations,
        observability,
    }
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
