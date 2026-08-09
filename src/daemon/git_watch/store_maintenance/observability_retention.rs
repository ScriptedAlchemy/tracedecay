use super::{log_daemon_event, now_secs_i64};
use tracedecay_runtime_core::cancellation::CancellationToken;

/// Prunes raw observability transport and derived rollups as one maintenance
/// capability. Cancellation aborts an in-flight database future; each store
/// transaction owns rollback, so a partial retention page cannot commit.
pub(in crate::daemon) async fn run_observability_analytics_retention(
    database: &crate::global_db::RegisteredGlobalDb,
    store: &'static str,
    cancellation: &CancellationToken,
) -> bool {
    if cancellation.is_cancelled() {
        return false;
    }
    let now = match now_secs_i64() {
        Ok(now) => now,
        Err(failure) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "observability_analytics".to_string()),
                    ("failure", failure.to_string()),
                ],
            );
            return false;
        }
    };
    let analytics_retention = tokio::select! {
        biased;
        () = cancellation.cancelled() => return false,
        result = database.prune_observability_events(now) => result,
    };
    match analytics_retention {
        Ok(analytics_receipt) => {
            if analytics_receipt.expired_detail > 0
                || analytics_receipt.expired_rollup > 0
                || analytics_receipt.expired_settled_outbox > 0
            {
                log_daemon_event(
                    "retention_observability_analytics",
                    &[
                        ("store", store.to_owned()),
                        (
                            "expired_detail",
                            analytics_receipt.expired_detail.to_string(),
                        ),
                        (
                            "expired_rollup",
                            analytics_receipt.expired_rollup.to_string(),
                        ),
                        (
                            "expired_settled_outbox",
                            analytics_receipt.expired_settled_outbox.to_string(),
                        ),
                    ],
                );
            }
            let rollup_retention = tokio::select! {
                biased;
                () = cancellation.cancelled() => return false,
                result = database.prune_observability_rollups(now) => result,
            };
            match rollup_retention {
                Ok(rollup_receipt) => {
                    if rollup_receipt.expired_generations > 0
                        || rollup_receipt.expired_journal_entries > 0
                        || rollup_receipt.expired_dirty_days > 0
                    {
                        log_daemon_event(
                            "retention_observability_rollups",
                            &[
                                ("store", store.to_owned()),
                                (
                                    "expired_generations",
                                    rollup_receipt.expired_generations.to_string(),
                                ),
                                (
                                    "expired_journal_entries",
                                    rollup_receipt.expired_journal_entries.to_string(),
                                ),
                                (
                                    "expired_dirty_days",
                                    rollup_receipt.expired_dirty_days.to_string(),
                                ),
                            ],
                        );
                    }
                    // A bounded analytics page remaining is an explicit
                    // request for the coordinator's short retry cadence.
                    !analytics_receipt.has_more
                }
                Err(_) => {
                    log_daemon_event(
                        "retention_degraded",
                        &[
                            ("pass", "observability_rollups".to_owned()),
                            ("failure", "retention_pass_failed".to_owned()),
                        ],
                    );
                    false
                }
            }
        }
        Err(_) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "observability_analytics".to_owned()),
                    ("failure", "retention_pass_failed".to_owned()),
                ],
            );
            false
        }
    }
}
