//! Live-compaction outcome → operator receipt.

use crate::retention::live_compaction::LiveStoreCompactionOutcomeV1;
use tracedecay_runtime_core::logging::log_daemon_event;

/// Record one live-compaction outcome and return whether the store is healthy.
#[must_use]
pub fn record_live_compaction_outcome(
    store_name: &'static str,
    outcome: LiveStoreCompactionOutcomeV1,
) -> bool {
    match outcome {
        LiveStoreCompactionOutcomeV1::NotScheduled => true,
        LiveStoreCompactionOutcomeV1::Compacted {
            freelist_before,
            freelist_after,
        } => {
            log_daemon_event(
                "retention_compaction",
                &[
                    ("store", store_name.to_owned()),
                    (
                        "freed_pages",
                        freelist_before.saturating_sub(freelist_after).to_string(),
                    ),
                ],
            );
            true
        }
        LiveStoreCompactionOutcomeV1::Failed(failure) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "compaction".to_owned()),
                    ("failure", failure.as_str().to_owned()),
                ],
            );
            false
        }
    }
}
