use std::collections::VecDeque;

use super::{
    DaemonLspProtocolSession, DaemonLspSessionTransport, DiagnosticSnapshotPort, FeedbackCyclePort,
    FramePoll, FrameSend, Infallible, LspFrame, LspRequestFailure, LspRequestId,
    MAX_LSP_FRAME_BYTES, MAX_PUBLICATION_BYTES, MAX_QUEUED_OUTBOUND_BYTES,
    MAX_QUEUED_OUTBOUND_MESSAGES, PublicationAdmission, RpcFailure, SemanticProviderPort,
    SessionLifecycle, Value, error_response, request_id,
};

const MIN_CLIENT_FRAME_OUTBOUND_RESERVE: usize = MAX_PUBLICATION_BYTES;
const CAPABILITY_CONTROL_RESERVED_MESSAGES: usize = 3;
const CAPABILITY_CONTROL_RESERVED_BYTES: usize = 12 * 1024;
const MAX_CAPABILITY_CONTROL_FRAME_BYTES: usize = 4 * 1024;
const MAX_ORDINARY_OUTBOUND_MESSAGES: usize =
    MAX_QUEUED_OUTBOUND_MESSAGES - CAPABILITY_CONTROL_RESERVED_MESSAGES;
const MAX_ORDINARY_OUTBOUND_BYTES: usize =
    MAX_QUEUED_OUTBOUND_BYTES - CAPABILITY_CONTROL_RESERVED_BYTES;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PublicationTag {
    pub(super) uri: String,
    pub(super) version: i64,
    pub(super) generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct QueuedFrame {
    pub(super) payload: LspFrame,
    pub(super) publication: Option<PublicationTag>,
    pub(super) server_request: Option<LspRequestId>,
}

#[derive(Default)]
pub(super) struct OutboundController {
    pub(super) queue: VecDeque<QueuedFrame>,
    pub(super) in_flight: bool,
    pub(super) queued_bytes: usize,
}

/// Concrete bridge-facing adapter for one typed daemon session actor. It
/// parses each client payload through [`DaemonLspProtocolSession`] and exposes
/// only queued LSP frames back to the bridge; it cannot become a raw daemon
/// socket tunnel.
pub struct DaemonLspProtocolTransport<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    session: DaemonLspProtocolSession<P, S, D>,
    now_ms: u64,
}

impl<P, S, D> DaemonLspProtocolTransport<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub fn new(session: DaemonLspProtocolSession<P, S, D>) -> Self {
        Self { session, now_ms: 0 }
    }

    pub fn set_now_ms(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    pub fn session(&self) -> &DaemonLspProtocolSession<P, S, D> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut DaemonLspProtocolSession<P, S, D> {
        &mut self.session
    }

    pub fn into_inner(self) -> DaemonLspProtocolSession<P, S, D> {
        self.session
    }
}

impl<P, S, D> DaemonLspSessionTransport for DaemonLspProtocolTransport<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    type Error = Infallible;

    fn try_send_client_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
        if matches!(
            self.session.lifecycle(),
            SessionLifecycle::Exited | SessionLifecycle::Expired
        ) {
            return Ok(FrameSend::Closed);
        }
        // Do not consume a frame when the typed session cannot reserve any
        // response capacity. The bridge retains exactly one frame and retries
        // once the daemon-to-client direction makes progress.
        if !self.session.has_client_frame_outbound_capacity() {
            return Ok(FrameSend::Backpressured);
        }
        let dispatch = self.session.handle_payload(frame, self.now_ms);
        Ok(if dispatch.closed {
            FrameSend::Closed
        } else {
            FrameSend::Sent
        })
    }

    fn poll_daemon_frame(&mut self) -> Result<FramePoll, Self::Error> {
        if let Some(frame) = self.session.poll_outbound() {
            return Ok(FramePoll::Frame(frame.to_vec()));
        }
        if matches!(
            self.session.lifecycle(),
            SessionLifecycle::Exited | SessionLifecycle::Expired
        ) {
            Ok(FramePoll::Closed)
        } else {
            Ok(FramePoll::Pending)
        }
    }

    fn acknowledge_daemon_frame(&mut self) -> Result<(), Self::Error> {
        self.session.acknowledge_outbound();
        Ok(())
    }
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    /// The daemon-side typed transport polls exactly one already-serialized
    /// frame. It cannot fetch arbitrary daemon socket data.
    pub fn poll_outbound(&mut self) -> Option<&[u8]> {
        let frame = self.outbound.queue.front()?;
        self.outbound.in_flight = true;
        Some(frame.payload.as_slice())
    }

    /// Records that the bridge accepted the current outbound frame. Network
    /// delivery remains at-least-once across reconnects by design.
    pub fn acknowledge_outbound(&mut self) -> bool {
        if !self.outbound.in_flight {
            return false;
        }
        let Some(frame) = self.outbound.queue.pop_front() else {
            self.outbound.in_flight = false;
            return false;
        };
        self.outbound.in_flight = false;
        self.outbound.queued_bytes = self
            .outbound
            .queued_bytes
            .saturating_sub(frame.payload.len());
        if let Some(publication) = frame.publication {
            self.lifecycle.control.acknowledge_publication_version(
                &publication.uri,
                publication.version,
                publication.generation,
            );
        }
        if self.diagnostics.refresh_needed {
            self.queue_diagnostic_refresh();
        }
        true
    }

    /// Test and adapter convenience. Production transports use
    /// [`Self::poll_outbound`] and [`Self::acknowledge_outbound`] so delivery
    /// state is preserved across temporary backpressure.
    pub fn drain_outbound(&mut self) -> Vec<LspFrame> {
        self.outbound.in_flight = false;
        let mut frames = Vec::with_capacity(self.outbound.queue.len());
        while let Some(frame) = self.outbound.queue.pop_front() {
            self.outbound.queued_bytes = self
                .outbound
                .queued_bytes
                .saturating_sub(frame.payload.len());
            if let Some(publication) = frame.publication {
                self.lifecycle.control.acknowledge_publication_version(
                    &publication.uri,
                    publication.version,
                    publication.generation,
                );
            }
            frames.push(frame.payload);
        }
        if self.diagnostics.refresh_needed {
            self.queue_diagnostic_refresh();
            while let Some(frame) = self.outbound.queue.pop_front() {
                self.outbound.queued_bytes = self
                    .outbound
                    .queued_bytes
                    .saturating_sub(frame.payload.len());
                frames.push(frame.payload);
            }
        }
        frames
    }
    pub(crate) fn enqueue_value(&mut self, value: Value) -> bool {
        let server_request = value
            .get("method")
            .and_then(Value::as_str)
            .and_then(|_| value.get("id"))
            .and_then(request_id);
        let response_id = value
            .get("id")
            .cloned()
            .filter(|_| value.get("method").is_none());
        let Ok(payload) = serde_json::to_vec(&value) else {
            return false;
        };
        if payload.len() <= MAX_LSP_FRAME_BYTES && self.enqueue_frame(payload, None, server_request)
        {
            return true;
        }
        let Some(id) = response_id else {
            return false;
        };
        let Ok(fallback) = serde_json::to_vec(&error_response(
            id,
            RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                retrigger_request: true,
            }),
        )) else {
            return false;
        };
        self.enqueue_frame(fallback, None, None)
    }

    pub(super) fn enqueue_value_exact(&mut self, value: Value) -> bool {
        let server_request = value
            .get("method")
            .and_then(Value::as_str)
            .and_then(|_| value.get("id"))
            .and_then(request_id);
        let Ok(payload) = serde_json::to_vec(&value) else {
            return false;
        };
        payload.len() <= MAX_LSP_FRAME_BYTES && self.enqueue_frame(payload, None, server_request)
    }

    /// Capability registration, cancellation, and unregistration have a
    /// dedicated bounded reserve so ordinary responses/publications cannot
    /// indefinitely delay a fail-closed capability transition.
    pub(super) fn enqueue_capability_control_value(&mut self, value: Value) -> bool {
        let server_request = value
            .get("method")
            .and_then(Value::as_str)
            .and_then(|_| value.get("id"))
            .and_then(request_id);
        let Ok(payload) = serde_json::to_vec(&value) else {
            return false;
        };
        if payload.len() > MAX_CAPABILITY_CONTROL_FRAME_BYTES
            || self.outbound.queue.len() >= MAX_QUEUED_OUTBOUND_MESSAGES
            || self.outbound.queued_bytes.saturating_add(payload.len()) > MAX_QUEUED_OUTBOUND_BYTES
        {
            return false;
        }
        self.outbound.queued_bytes += payload.len();
        self.outbound.queue.push_back(QueuedFrame {
            payload,
            publication: None,
            server_request,
        });
        true
    }

    pub(super) fn enqueue_publication(&mut self, value: Value, tag: PublicationTag) -> bool {
        let Ok(payload) = serde_json::to_vec(&value) else {
            return false;
        };
        if payload.len() > MAX_PUBLICATION_BYTES {
            return false;
        }
        if self.publication_replacement(&payload, &tag).is_err() {
            return false;
        }
        match self.lifecycle.control.admit_payload_publication(
            tag.uri.clone(),
            tag.version,
            tag.generation,
            &payload,
        ) {
            PublicationAdmission::Accepted => {}
            PublicationAdmission::Duplicate | PublicationAdmission::Stale => return false,
            PublicationAdmission::TooLarge { .. } | PublicationAdmission::SessionUnavailable => {
                return false;
            }
        }
        self.enqueue_frame(payload, Some(tag), None)
    }

    pub(super) fn enqueue_frame(
        &mut self,
        payload: LspFrame,
        publication: Option<PublicationTag>,
        server_request: Option<LspRequestId>,
    ) -> bool {
        if payload.len() > MAX_LSP_FRAME_BYTES {
            return false;
        }
        let replacement = if let Some(tag) = &publication {
            let Ok(replacement) = self.publication_replacement(&payload, tag) else {
                return false;
            };
            replacement
        } else {
            if self.outbound.queue.len() >= MAX_ORDINARY_OUTBOUND_MESSAGES
                || self.outbound.queued_bytes.saturating_add(payload.len())
                    > MAX_ORDINARY_OUTBOUND_BYTES
            {
                return false;
            }
            None
        };
        if let Some(index) = replacement {
            let Some(existing) = self.outbound.queue.get(index) else {
                return false;
            };
            debug_assert!(existing.publication.is_some());
            let Some(replaced) = self.outbound.queue.remove(index) else {
                return false;
            };
            self.outbound.queued_bytes = self
                .outbound
                .queued_bytes
                .saturating_sub(replaced.payload.len());
        }
        if let Some(tag) = &publication {
            self.lifecycle.control.mark_publication_queued(&tag.uri);
        }
        self.outbound.queued_bytes += payload.len();
        self.outbound.queue.push_back(QueuedFrame {
            payload,
            publication,
            server_request,
        });
        true
    }

    pub(super) fn publication_replacement(
        &self,
        payload: &[u8],
        tag: &PublicationTag,
    ) -> Result<Option<usize>, ()> {
        let replacement = self
            .outbound
            .queue
            .iter()
            .enumerate()
            .find(|(index, frame)| {
                !(self.outbound.in_flight && *index == 0)
                    && frame
                        .publication
                        .as_ref()
                        .is_some_and(|existing| existing.uri == tag.uri)
            })
            .map(|(index, _)| index);
        let replaced_len = replacement
            .and_then(|index| self.outbound.queue.get(index))
            .map_or(0, |frame| frame.payload.len());
        if let Some(index) = replacement {
            let existing = self.outbound.queue[index].publication.as_ref().ok_or(())?;
            if (tag.version, tag.generation) < (existing.version, existing.generation)
                || ((tag.version, tag.generation) == (existing.version, existing.generation)
                    && self.outbound.queue[index].payload == payload)
            {
                return Err(());
            }
        }
        let projected_messages = self.outbound.queue.len() + usize::from(replacement.is_none());
        let projected_bytes = self
            .outbound
            .queued_bytes
            .saturating_sub(replaced_len)
            .saturating_add(payload.len());
        if projected_messages > MAX_ORDINARY_OUTBOUND_MESSAGES
            || projected_bytes > MAX_ORDINARY_OUTBOUND_BYTES
        {
            return Err(());
        }
        Ok(replacement)
    }

    pub(super) fn discard_document_publications(&mut self, uri: &str) {
        self.discard_document_context(uri);
        self.diagnostics.active_refreshes.remove(uri);
        let mut retained = VecDeque::with_capacity(self.outbound.queue.len());
        let mut index = 0_usize;
        while let Some(frame) = self.outbound.queue.pop_front() {
            let is_in_flight = self.outbound.in_flight && index == 0;
            index += 1;
            if !is_in_flight
                && frame
                    .publication
                    .as_ref()
                    .is_some_and(|publication| publication.uri == uri)
            {
                self.outbound.queued_bytes = self
                    .outbound
                    .queued_bytes
                    .saturating_sub(frame.payload.len());
            } else {
                retained.push_back(frame);
            }
        }
        self.outbound.queue = retained;
        self.lifecycle.control.remove_publication(uri);
        self.diagnostics.published.remove(uri);
    }

    pub(super) fn has_outbound_capacity(&self, reserve_bytes: usize) -> bool {
        self.outbound.queue.len() < MAX_ORDINARY_OUTBOUND_MESSAGES
            && self.outbound.queued_bytes.saturating_add(reserve_bytes)
                <= MAX_ORDINARY_OUTBOUND_BYTES
    }

    pub(super) fn has_client_frame_outbound_capacity(&self) -> bool {
        self.has_outbound_capacity(MIN_CLIENT_FRAME_OUTBOUND_RESERVE)
    }
}

#[cfg(test)]
mod controller_tests {
    use serde_json::json;

    #[test]
    fn queue_serializes_one_exact_json_rpc_frame() {
        let mut session = super::super::tests::session();
        assert!(session.enqueue_value(json!({
            "jsonrpc": "2.0",
            "id": "queue",
            "result": { "ok": true },
        })));

        assert_eq!(
            session.poll_outbound(),
            Some(br#"{"id":"queue","jsonrpc":"2.0","result":{"ok":true}}"#.as_slice())
        );
        assert!(session.acknowledge_outbound());
        assert!(session.poll_outbound().is_none());
    }
}
