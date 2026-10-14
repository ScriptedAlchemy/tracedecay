//! The MCP server session loop over one broker connection.
//!
//! Every request runs inside the connection future, never on a task of its
//! own: a scope the daemon installs around the connection (admission parking)
//! covers every handler, and no executor is named here. The protocol policy
//! mirrors `rmcp` 3.5's server (`service/server.rs`, `handler/server.rs`), with
//! `rmcp` kept only for its wire model types.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;

use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, StreamExt};
use rmcp::model::{
    ClientJsonRpcMessage, ClientNotification, ClientRequest, EmptyResult, ErrorData, GetMeta,
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, ProtocolVersion, RequestId,
    RequestMetaObject, ServerJsonRpcMessage, ServerResult,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::cancellation::CancellationToken;

use crate::broker_stream_transport::{BrokerResponseWriter, BrokerStreamTransport};

use super::{McpConnectionContext, RmcpConnectionAdapter};

/// The refusal for an `initialize` whose params do not decode.
const MALFORMED_INITIALIZE_MESSAGE: &str = "initialize params are missing or malformed: \
     protocolVersion, capabilities and clientInfo are required";

/// Where a connection stands in the MCP lifecycle.
#[derive(Clone)]
pub(crate) enum SessionState {
    /// Only pings, refused openers, and `server/discover` probes have arrived.
    Opening,
    /// The first request past any discover probe carried SEP-2575 per-request
    /// client context instead of an `initialize`; every later request must
    /// carry it too.
    Inline,
    /// `initialize` agreed on this protocol version.
    Initialized(ProtocolVersion),
}

/// What a request handler knows about its request beyond the params.
pub(crate) struct McpRequestContext {
    pub(crate) id: RequestId,
    pub(crate) cancellation: CancellationToken,
    /// The wire `params._meta`, lifted out of the params.
    pub(crate) meta: RequestMetaObject,
    pub(crate) session: SessionState,
}

impl McpRequestContext {
    /// The request's own `_meta` version, else the session's negotiated one.
    pub(crate) fn protocol_version(&self) -> Option<ProtocolVersion> {
        self.meta
            .protocol_version()
            .or_else(|| match &self.session {
                SessionState::Initialized(version) => Some(version.clone()),
                SessionState::Opening | SessionState::Inline => None,
            })
    }
}

type InFlight<'a> = Pin<Box<dyn Future<Output = Option<RequestId>> + Send + 'a>>;

struct Session<'a, C: McpConnectionContext> {
    adapter: &'a RmcpConnectionAdapter<C>,
    responses: BrokerResponseWriter,
    state: SessionState,
    cancellations: HashMap<RequestId, CancellationToken>,
    in_flight: FuturesUnordered<InFlight<'a>>,
}

/// Serves one connection until the peer leaves or `shutdown` resolves.
///
/// Either ending cancels every in-flight request, then waits for each to
/// settle. The transport ends the message stream only once the peer fully
/// closed or every accepted request was answered, so nothing cancelled here
/// could still have been delivered.
pub(crate) async fn serve<C, S>(
    adapter: &RmcpConnectionAdapter<C>,
    transport: BrokerStreamTransport,
    shutdown: S,
) -> Result<()>
where
    C: McpConnectionContext,
    S: Future<Output = ()> + Send,
{
    let mut session = Session {
        adapter,
        responses: transport.response_writer(),
        state: SessionState::Opening,
        cancellations: HashMap::new(),
        in_flight: FuturesUnordered::new(),
    };
    // The stream owns the in-progress receive, so a lost race never drops a
    // half-read frame or a half-written refusal.
    let mut messages = std::pin::pin!(futures_util::stream::unfold(
        transport,
        |mut transport| async move {
            transport
                .receive()
                .await
                .map(|message| (message, transport))
        }
    ));
    let mut shutdown = std::pin::pin!(shutdown);
    let mut draining = false;
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown, if !draining => {
                draining = true;
                session.cancel_all();
                break;
            }
            Some(answered) = session.in_flight.next(), if !session.in_flight.is_empty() => {
                session.answered(answered);
            }
            message = messages.next() => match message {
                None => {
                    session.cancel_all();
                    break;
                }
                Some(message) => session.receive(message).await?,
            },
        }
    }
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown, if !draining => {
                draining = true;
                session.cancel_all();
            }
            answered = session.in_flight.next() => match answered {
                Some(answered) => session.answered(answered),
                None => return Ok(()),
            },
        }
    }
}

impl<C: McpConnectionContext> Session<'_, C> {
    async fn receive(&mut self, message: ClientJsonRpcMessage) -> Result<()> {
        if matches!(self.state, SessionState::Opening) {
            return self.open(message).await;
        }
        match message {
            JsonRpcMessage::Request(JsonRpcRequest { id, request, .. }) => {
                self.start_request(id, request);
            }
            JsonRpcMessage::Notification(JsonRpcNotification { notification, .. }) => {
                self.notify(notification);
            }
            // This server sends the peer no requests, so no answer is owed.
            JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => {}
        }
        Ok(())
    }

    /// The lifecycle before a request settles the session. Everything here is
    /// answered inline, in wire order, before the next frame is read.
    async fn open(&mut self, message: ClientJsonRpcMessage) -> Result<()> {
        // JSON-RPC forbids answering a notification, and nothing is in flight
        // yet, so a stray one (a pipelined `notifications/initialized` after a
        // refused initialize) is dropped rather than ending the connection.
        let JsonRpcMessage::Request(JsonRpcRequest {
            id, mut request, ..
        }) = message
        else {
            return Ok(());
        };
        if matches!(request, ClientRequest::PingRequest(_)) {
            return Ok(self
                .answer(id, Ok(ServerResult::EmptyResult(EmptyResult {})))
                .await?);
        }
        // An `initialize` whose params do not decode arrives as a custom
        // request. It gets a typed refusal and the connection stays open for
        // a corrected handshake.
        if request.method() == "initialize"
            && !matches!(request, ClientRequest::InitializeRequest(_))
        {
            let refusal = ErrorData::invalid_params(MALFORMED_INITIALIZE_MESSAGE, None);
            return Ok(self.answer(id, Err(refusal)).await?);
        }
        let meta = std::mem::take(request.get_meta_mut());
        let request = match request {
            ClientRequest::InitializeRequest(initialize) => initialize.params,
            request => {
                // Each refusal here leaves the session open, and a discover
                // probe chooses no lifecycle, so a client that probes first
                // can still fall back to `initialize`.
                let missing = meta.missing_required_keys(&ProtocolVersion::V_2026_07_28);
                let Some(requested) = meta.protocol_version().filter(|_| missing.is_empty()) else {
                    let refusal = missing_request_context_error(&missing);
                    return Ok(self.answer(id, Err(refusal)).await?);
                };
                if matches!(request, ClientRequest::DiscoverRequest(_)) {
                    let context = McpRequestContext {
                        id: id.clone(),
                        cancellation: CancellationToken::new(),
                        meta,
                        session: SessionState::Opening,
                    };
                    let discovered = self.adapter.handle_request(request, context).await;
                    return Ok(self.answer(id, discovered).await?);
                }
                if !ProtocolVersion::KNOWN_VERSIONS.contains(&requested) {
                    let refusal = ErrorData::unsupported_protocol_version(
                        requested,
                        ProtocolVersion::KNOWN_VERSIONS,
                    );
                    return Ok(self.answer(id, Err(refusal)).await?);
                }
                self.state = SessionState::Inline;
                self.start_request_with_meta(id, request, meta);
                return Ok(());
            }
        };
        let context = McpRequestContext {
            id: id.clone(),
            cancellation: CancellationToken::new(),
            meta,
            session: SessionState::Initialized(request.protocol_version.clone()),
        };
        match self.adapter.initialize(request, context).await {
            Ok(initialized) => {
                self.state = SessionState::Initialized(initialized.protocol_version.clone());
                Ok(self
                    .answer(id, Ok(ServerResult::InitializeResult(initialized)))
                    .await?)
            }
            Err(refusal) => {
                self.answer(id, Err(refusal.clone())).await?;
                Err(opening_failed(&refusal))
            }
        }
    }

    fn answer(
        &self,
        id: RequestId,
        result: std::result::Result<ServerResult, ErrorData>,
    ) -> impl Future<Output = std::io::Result<()>> + Send + 'static {
        self.responses.clone().send(response_message(id, result))
    }

    fn start_request(&mut self, id: RequestId, mut request: ClientRequest) {
        let meta = std::mem::take(request.get_meta_mut());
        self.start_request_with_meta(id, request, meta);
    }

    fn start_request_with_meta(
        &mut self,
        id: RequestId,
        request: ClientRequest,
        meta: RequestMetaObject,
    ) {
        let cancellation = CancellationToken::new();
        self.cancellations.insert(id.clone(), cancellation.clone());
        let context = McpRequestContext {
            id: id.clone(),
            cancellation,
            meta,
            session: self.state.clone(),
        };
        let adapter = self.adapter;
        let responses = self.responses.clone();
        self.in_flight.push(Box::pin(async move {
            // An unwind here would end the connection future and every other
            // request on the session, so a panicking handler fails alone.
            let result = AssertUnwindSafe(adapter.handle_request(request, context))
                .catch_unwind()
                .await
                .unwrap_or_else(|payload| {
                    Err(ErrorData::internal_error(
                        format!("request handler panicked: {}", panic_message(&*payload)),
                        None,
                    ))
                });
            if let Err(error) = responses.send(response_message(id.clone(), result)).await {
                tracing::debug!(%error, "MCP response was not delivered");
            }
            Some(id)
        }));
    }

    fn notify(&mut self, notification: ClientNotification) {
        match notification {
            ClientNotification::CancelledNotification(cancelled) => {
                let request_id = cancelled.params.request_id;
                if let Some(cancellation) = request_id
                    .as_ref()
                    .and_then(|id| self.cancellations.remove(id))
                {
                    cancellation.cancel();
                }
                self.adapter.cancel_request(request_id);
            }
            ClientNotification::CustomNotification(custom) => {
                let adapter = self.adapter;
                self.in_flight.push(Box::pin(async move {
                    let dispatched = AssertUnwindSafe(
                        adapter.dispatch_notification(custom.method, custom.params),
                    )
                    .catch_unwind()
                    .await;
                    if let Err(payload) = dispatched {
                        tracing::warn!(
                            panic = panic_message(&*payload),
                            "MCP notification handler panicked"
                        );
                    }
                    None
                }));
            }
            // Progress, `initialized`, and roots changes need no action here.
            _ => {}
        }
    }

    fn answered(&mut self, request: Option<RequestId>) {
        if let Some(id) = request {
            self.cancellations.remove(&id);
        }
    }

    fn cancel_all(&mut self) {
        let _span = tracing::trace_span!("daemon.engine.transport.cancel").entered();
        for (_, cancellation) in self.cancellations.drain() {
            cancellation.cancel();
        }
    }
}

fn response_message(
    id: RequestId,
    result: std::result::Result<ServerResult, ErrorData>,
) -> ServerJsonRpcMessage {
    match result {
        Ok(result) => ServerJsonRpcMessage::response(result, id),
        Err(error) => ServerJsonRpcMessage::error(error, Some(id)),
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

pub(crate) fn missing_request_context_error(missing: &[&str]) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "request _meta is missing or has malformed required fields: {}",
            missing.join(", ")
        ),
        None,
    )
}

fn opening_failed(refusal: &ErrorData) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("MCP session failed to open: {}", refusal.message),
    }
}
