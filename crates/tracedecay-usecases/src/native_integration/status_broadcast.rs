//! In-memory fan-out of observed native-integration transaction statuses.
//!
//! The daemon invocation handler publishes the same bounded application
//! status projection every surface answers with; LSP sessions read the most
//! recent projections and forward changed ones as notifications. This is a
//! read-only observation channel: nothing here can start, approve, apply, or
//! cancel a transaction.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tracedecay_application::NativeIntegrationStatusProjectionV1;
use tracedecay_domain::NativeIntegrationTransactionId;
use tracedecay_lsp::NativeIntegrationStatusPort;

/// Latest-per-transaction retention. Beyond this bound the oldest projection
/// by `updated_at` is evicted; consumers dedupe on content, so eviction can
/// only cost a redundant re-notification, never a fabricated status.
const MAX_BROADCAST_TRANSACTIONS: usize = 64;

#[derive(Default)]
pub struct NativeIntegrationStatusBroadcastV1 {
    statuses: Mutex<BTreeMap<NativeIntegrationTransactionId, NativeIntegrationStatusProjectionV1>>,
}

impl NativeIntegrationStatusBroadcastV1 {
    /// Records the latest observed projection for its transaction. A stale
    /// publication (older `phase_revision` for the same transaction) never
    /// overwrites newer durable evidence.
    pub fn publish(&self, projection: NativeIntegrationStatusProjectionV1) {
        let mut statuses = self
            .statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match statuses.get(&projection.transaction_id) {
            Some(current) if current.phase_revision > projection.phase_revision => return,
            _ => {}
        }
        statuses.insert(projection.transaction_id.clone(), projection);
        if statuses.len() > MAX_BROADCAST_TRANSACTIONS
            && let Some(oldest) = statuses
                .iter()
                .min_by_key(|(_, status)| status.updated_at)
                .map(|(transaction_id, _)| transaction_id.clone())
        {
            statuses.remove(&oldest);
        }
    }
}

impl NativeIntegrationStatusPort for NativeIntegrationStatusBroadcastV1 {
    /// The most recently updated projections, newest first.
    fn poll_status(&self, maximum: usize) -> Vec<NativeIntegrationStatusProjectionV1> {
        let statuses = self
            .statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut recent = statuses.values().cloned().collect::<Vec<_>>();
        recent.sort_by_key(|status| std::cmp::Reverse(status.updated_at));
        recent.truncate(maximum);
        recent
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        ManifestDigest, NativeIntegrationPhaseV1, NativeIntegrationPreviewId,
        NativeIntegrationTerminalOutcomeV1, RefId, RepositoryId, UtcMicros,
    };

    use super::*;

    fn projection(
        transaction: &str,
        phase_revision: u64,
        updated_at: i64,
    ) -> NativeIntegrationStatusProjectionV1 {
        NativeIntegrationStatusProjectionV1 {
            transaction_id: NativeIntegrationTransactionId::new(transaction).expect("transaction"),
            preview_id: NativeIntegrationPreviewId::new("preview.broadcast").expect("preview"),
            preview_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("digest"),
            repository_id: RepositoryId::new("repository.broadcast").expect("repository"),
            destination_ref: RefId::new("refs/heads/main").expect("reference"),
            phase: NativeIntegrationPhaseV1::Terminal,
            phase_revision,
            cancellation_requested: false,
            terminal_outcome: Some(NativeIntegrationTerminalOutcomeV1::Committed),
            updated_at: UtcMicros(updated_at),
        }
    }

    #[test]
    fn newest_projection_per_transaction_wins_and_stale_revisions_never_regress() {
        let broadcast = NativeIntegrationStatusBroadcastV1::default();
        broadcast.publish(projection("transaction.broadcast.one", 3, 30));
        broadcast.publish(projection("transaction.broadcast.one", 2, 40));

        let recent = broadcast.poll_status(8);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].phase_revision, 3);
        assert_eq!(recent[0].updated_at, UtcMicros(30));
    }

    #[test]
    fn retention_evicts_the_oldest_projection_beyond_the_bound() {
        let broadcast = NativeIntegrationStatusBroadcastV1::default();
        for index in 0..=MAX_BROADCAST_TRANSACTIONS {
            broadcast.publish(projection(
                &format!("transaction.broadcast.{index}"),
                1,
                index as i64,
            ));
        }

        let recent = broadcast.poll_status(MAX_BROADCAST_TRANSACTIONS + 1);
        assert_eq!(recent.len(), MAX_BROADCAST_TRANSACTIONS);
        assert!(
            !recent
                .iter()
                .any(|status| status.updated_at == UtcMicros(0)),
            "the oldest projection must be evicted first"
        );
    }
}
