//! Server-to-client native-integration status notifications.
//!
//! One bounded flush forwards the daemon's application status projections to a
//! ready session as `tracedecay/nativeIntegrationStatus` notifications. The
//! session dedupes per transaction, so a port re-returning an unchanged status
//! never re-notifies. See [`crate::native_integration`] for the gateway
//! constraint: this path admits no client method.

use tracedecay_application::NativeIntegrationStatusProjectionV1;
use tracedecay_domain::{NativeIntegrationTransactionId, RepositoryId};

use super::{
    Arc, BTreeMap, DaemonLspProtocolSession, DiagnosticSnapshotPort, FeedbackCyclePort,
    SemanticProviderPort, SessionLifecycle, json,
};
use crate::native_integration::{
    MAX_NATIVE_INTEGRATION_STATUS_BYTES, MAX_NATIVE_INTEGRATION_STATUS_PER_POLL,
    NativeIntegrationStatusPort, TRACEDECAY_NATIVE_INTEGRATION_STATUS_METHOD,
};

/// Transactions each session remembers for dedupe. Terminal statuses stay
/// remembered so a port that keeps returning them cannot re-notify; the oldest
/// entry by `updated_at` is evicted beyond this bound.
const MAX_TRACKED_NATIVE_INTEGRATION_TRANSACTIONS: usize = 128;

#[derive(Default)]
pub(super) struct NativeIntegrationController {
    pub(super) port: Option<Arc<dyn NativeIntegrationStatusPort>>,
    pub(super) notified: BTreeMap<
        (RepositoryId, NativeIntegrationTransactionId),
        NativeIntegrationStatusProjectionV1,
    >,
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub(super) fn flush_native_integration_status(&mut self) {
        if self.lifecycle.control.lifecycle() != SessionLifecycle::Ready
            || !self.has_outbound_capacity(MAX_NATIVE_INTEGRATION_STATUS_BYTES)
        {
            return;
        }
        let Some(port) = self.native_integration.port.clone() else {
            return;
        };
        let mut emitted = 0_usize;
        for projection in port.poll_status(MAX_TRACKED_NATIVE_INTEGRATION_TRANSACTIONS) {
            let identity = (
                projection.repository_id.clone(),
                projection.transaction_id.clone(),
            );
            if self.native_integration.notified.get(&identity) == Some(&projection) {
                continue;
            }
            if emitted == MAX_NATIVE_INTEGRATION_STATUS_PER_POLL {
                break;
            }
            let Ok(params) = serde_json::to_value(&projection) else {
                continue;
            };
            let notification = json!({
                "jsonrpc": "2.0",
                "method": TRACEDECAY_NATIVE_INTEGRATION_STATUS_METHOD,
                "params": params,
            });
            if !self.enqueue_value(notification) {
                break;
            }
            self.native_integration
                .notified
                .insert(identity, projection);
            emitted = emitted.saturating_add(1);
            if self.native_integration.notified.len() > MAX_TRACKED_NATIVE_INTEGRATION_TRANSACTIONS
                && let Some(oldest) = self
                    .native_integration
                    .notified
                    .iter()
                    .min_by_key(|(_, status)| status.updated_at)
                    .map(|(identity, _)| identity.clone())
            {
                self.native_integration.notified.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tracedecay_application::NativeIntegrationStatusProjectionV1;
    use tracedecay_domain::{
        ManifestDigest, NativeIntegrationPhaseV1, NativeIntegrationPreviewId,
        NativeIntegrationTerminalOutcomeV1, NativeIntegrationTransactionId, RefId, RepositoryId,
        UtcMicros,
    };

    use super::super::tests::{initialize, session};
    use crate::native_integration::{
        MAX_NATIVE_INTEGRATION_STATUS_PER_POLL, NativeIntegrationStatusPort,
        TRACEDECAY_NATIVE_INTEGRATION_STATUS_METHOD,
    };

    struct ScriptedStatusPort {
        statuses: Mutex<Vec<NativeIntegrationStatusProjectionV1>>,
    }

    impl ScriptedStatusPort {
        fn holding(projection: NativeIntegrationStatusProjectionV1) -> Arc<Self> {
            Arc::new(Self {
                statuses: Mutex::new(vec![projection]),
            })
        }

        fn replace(&self, projection: NativeIntegrationStatusProjectionV1) {
            *self.statuses.lock().unwrap() = vec![projection];
        }

        fn replace_many(&self, projections: Vec<NativeIntegrationStatusProjectionV1>) {
            *self.statuses.lock().unwrap() = projections;
        }
    }

    impl NativeIntegrationStatusPort for ScriptedStatusPort {
        fn poll_status(&self, maximum: usize) -> Vec<NativeIntegrationStatusProjectionV1> {
            let mut statuses = self.statuses.lock().unwrap().clone();
            statuses.truncate(maximum);
            statuses
        }
    }

    fn projection(
        phase: NativeIntegrationPhaseV1,
        phase_revision: u64,
        terminal_outcome: Option<NativeIntegrationTerminalOutcomeV1>,
    ) -> NativeIntegrationStatusProjectionV1 {
        projection_for(
            "repository.lsp.notify",
            "transaction.lsp.notify",
            phase,
            phase_revision,
            terminal_outcome,
        )
    }

    fn projection_for(
        repository_id: &str,
        transaction_id: &str,
        phase: NativeIntegrationPhaseV1,
        phase_revision: u64,
        terminal_outcome: Option<NativeIntegrationTerminalOutcomeV1>,
    ) -> NativeIntegrationStatusProjectionV1 {
        NativeIntegrationStatusProjectionV1 {
            transaction_id: NativeIntegrationTransactionId::new(transaction_id).unwrap(),
            preview_id: NativeIntegrationPreviewId::new("preview.lsp.notify").unwrap(),
            preview_digest: ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            repository_id: RepositoryId::new(repository_id).unwrap(),
            destination_ref: RefId::new("refs/heads/main").unwrap(),
            phase,
            phase_revision,
            cancellation_requested: false,
            terminal_outcome,
            updated_at: UtcMicros(i64::from(u32::try_from(phase_revision).unwrap())),
        }
    }

    fn native_integration_notifications(frames: Vec<Vec<u8>>) -> Vec<Value> {
        frames
            .into_iter()
            .map(|frame| serde_json::from_slice::<Value>(&frame).unwrap())
            .filter(|message| message["method"] == TRACEDECAY_NATIVE_INTEGRATION_STATUS_METHOD)
            .collect()
    }

    #[test]
    fn ready_sessions_forward_each_status_change_exactly_once() {
        let port =
            ScriptedStatusPort::holding(projection(NativeIntegrationPhaseV1::Prepared, 1, None));
        let mut session =
            session().with_native_integration_status_port(Arc::clone(&port) as Arc<_>);
        initialize(&mut session);

        let first = native_integration_notifications(session.drain_outbound());
        assert_eq!(first.len(), 1, "one changed status must notify once");
        assert_eq!(first[0]["params"]["phase"], "prepared");
        assert_eq!(first[0]["params"]["phase_revision"], 1);

        // An unchanged status re-returned by the port never re-notifies.
        session.flush_due(2);
        assert!(native_integration_notifications(session.drain_outbound()).is_empty());

        // A durable phase advance notifies again with the terminal outcome.
        port.replace(projection(
            NativeIntegrationPhaseV1::Terminal,
            5,
            Some(NativeIntegrationTerminalOutcomeV1::Committed),
        ));
        session.flush_due(3);
        let terminal = native_integration_notifications(session.drain_outbound());
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0]["params"]["phase"], "terminal");
        assert_eq!(terminal[0]["params"]["terminal_outcome"], "committed");
    }

    #[test]
    fn sessions_before_initialization_receive_no_native_integration_notifications() {
        let port =
            ScriptedStatusPort::holding(projection(NativeIntegrationPhaseV1::Prepared, 1, None));
        let mut session = session().with_native_integration_status_port(port as Arc<_>);

        session.flush_due(1);

        assert!(native_integration_notifications(session.drain_outbound()).is_empty());
    }

    #[test]
    fn equal_transaction_ids_from_distinct_projects_dedupe_independently() {
        let shared_transaction = "transaction.shared";
        let port = ScriptedStatusPort::holding(projection_for(
            "repository.first",
            shared_transaction,
            NativeIntegrationPhaseV1::Prepared,
            1,
            None,
        ));
        port.replace_many(vec![
            projection_for(
                "repository.first",
                shared_transaction,
                NativeIntegrationPhaseV1::Prepared,
                1,
                None,
            ),
            projection_for(
                "repository.second",
                shared_transaction,
                NativeIntegrationPhaseV1::Prepared,
                1,
                None,
            ),
        ]);
        let mut session =
            session().with_native_integration_status_port(Arc::clone(&port) as Arc<_>);
        initialize(&mut session);
        assert_eq!(
            native_integration_notifications(session.drain_outbound()).len(),
            2,
            "both project-owned statuses notify once"
        );

        session.flush_due(2);

        assert!(
            native_integration_notifications(session.drain_outbound()).is_empty(),
            "unchanged statuses with colliding transaction ids must stay deduped"
        );
    }

    #[test]
    fn retained_statuses_beyond_one_flush_are_eventually_notified() {
        let statuses = (0..=MAX_NATIVE_INTEGRATION_STATUS_PER_POLL)
            .map(|index| {
                projection_for(
                    "repository.lsp.notify",
                    &format!("transaction.lsp.notify.{index}"),
                    NativeIntegrationPhaseV1::Prepared,
                    1,
                    None,
                )
            })
            .collect::<Vec<_>>();
        let port = ScriptedStatusPort::holding(statuses[0].clone());
        port.replace_many(statuses);
        let mut session =
            session().with_native_integration_status_port(Arc::clone(&port) as Arc<_>);

        initialize(&mut session);
        assert_eq!(
            native_integration_notifications(session.drain_outbound()).len(),
            MAX_NATIVE_INTEGRATION_STATUS_PER_POLL
        );

        session.flush_due(2);
        assert_eq!(
            native_integration_notifications(session.drain_outbound()).len(),
            1,
            "the next flush must advance beyond the first bounded notification page"
        );
    }
}
