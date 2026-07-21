//! Thin HTTP/SSE adapter contracts over `tracedecay-application`.
//!
//! The executable owns `CanonicalInvocation`; this crate receives the resolved
//! binding and its application result after dispatch, then encodes that result
//! for HTTP. It owns no store, query, policy, or LSP tunnel authority.
//!
#![forbid(unsafe_code)]

use serde::Serialize;
use thiserror::Error;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationProblemEnvelope, ApplicationResult, OperationTermination,
    RequestId, SafeDiagnostic, StreamEvent, StreamEventKind, StreamGap, StreamTermination,
};
use tracedecay_tool_catalog::BindingId;

/// Initial revision for the HTTP adapter's outbound DTOs.
pub const HTTP_API_REVISION: u32 = 1;

/// A resolved canonical invocation result ready for HTTP presentation.
pub struct CanonicalInvocationResult<T> {
    pub binding_id: BindingId,
    pub result: ApplicationResult<T>,
}

impl<T> CanonicalInvocationResult<T> {
    pub fn new(binding_id: BindingId, result: ApplicationResult<T>) -> Self {
        Self { binding_id, result }
    }

    pub fn into_http_json(self) -> HttpJsonEnvelope<T> {
        match self.result {
            Ok(application) => HttpJsonEnvelope::Success(Box::new(HttpSuccessEnvelope {
                binding_id: self.binding_id,
                application,
            })),
            Err(application) => HttpJsonEnvelope::Problem(Box::new(HttpProblemEnvelope {
                binding_id: self.binding_id,
                application,
            })),
        }
    }
}

/// Outbound HTTP JSON is either an admitted application result or a
/// pre-admission application problem.
#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HttpJsonEnvelope<T> {
    Success(Box<HttpSuccessEnvelope<T>>),
    Problem(Box<HttpProblemEnvelope>),
}

/// HTTP success preserves the application contract, request identity, scope,
/// and outcome without reimplementing application semantics.
#[derive(Serialize)]
pub struct HttpSuccessEnvelope<T> {
    pub binding_id: BindingId,
    #[serde(flatten)]
    pub application: ApplicationEnvelope<T>,
}

/// HTTP problem preserves the application's safe problem record verbatim.
#[derive(Serialize)]
pub struct HttpProblemEnvelope {
    pub binding_id: BindingId,
    #[serde(flatten)]
    pub application: ApplicationProblemEnvelope,
}

/// SSE presentation of canonical stream events. A concrete server supplies
/// framing, resume, heartbeat scheduling, and backpressure only.
#[derive(Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum HttpSseEvent<T> {
    Open {
        correlation_id: RequestId,
        next_sequence: u64,
    },
    Item {
        sequence: u64,
        item: T,
    },
    Progress {
        sequence: u64,
        completed: u64,
        total: Option<u64>,
    },
    Gap {
        sequence: u64,
        gap: StreamGap,
    },
    Heartbeat {
        sequence: u64,
    },
    Warning {
        sequence: u64,
        warning: SafeDiagnostic,
    },
    Completed {
        sequence: u64,
        terminal: StreamTermination,
    },
    Cancelled {
        sequence: u64,
        terminal: StreamTermination,
    },
    TimedOut {
        sequence: u64,
        terminal: StreamTermination,
    },
    Failed {
        sequence: u64,
        terminal: StreamTermination,
    },
    Partial {
        sequence: u64,
        terminal: StreamTermination,
    },
    EffectUnknown {
        sequence: u64,
        terminal: StreamTermination,
    },
}

impl<T> From<StreamEvent<T>> for HttpSseEvent<T> {
    fn from(event: StreamEvent<T>) -> Self {
        let sequence = event.sequence;
        match event.kind {
            StreamEventKind::Item(item) => Self::Item { sequence, item },
            StreamEventKind::Progress { completed, total } => Self::Progress {
                sequence,
                completed,
                total,
            },
            StreamEventKind::Gap(gap) => Self::Gap { sequence, gap },
            StreamEventKind::Terminal(terminal) => match terminal.termination {
                OperationTermination::Completed => Self::Completed { sequence, terminal },
                OperationTermination::Cancelled => Self::Cancelled { sequence, terminal },
                OperationTermination::TimedOut => Self::TimedOut { sequence, terminal },
                OperationTermination::Failed => Self::Failed { sequence, terminal },
                OperationTermination::Partial => Self::Partial { sequence, terminal },
                OperationTermination::EffectUnknown => Self::EffectUnknown { sequence, terminal },
            },
        }
    }
}

/// Placeholder for a transport-owned SSE source.
pub trait HttpSseStream {
    type Item;

    fn next_event(&mut self) -> Result<Option<HttpSseEvent<Self::Item>>, HttpAdapterError>;
}

/// Transport wiring failures that reveal no application state.
#[derive(Debug, Error)]
pub enum HttpAdapterError {
    #[error("HTTP/SSE transport wiring is not installed")]
    NotImplemented,
}

#[cfg(test)]
mod tests {
    use super::HttpSseEvent;
    use tracedecay_application::{StreamEvent, StreamEventKind};

    #[test]
    fn sse_preserves_canonical_item_and_progress_events() {
        let item = HttpSseEvent::from(StreamEvent::item(7, "value").expect("item"));
        assert_eq!(
            serde_json::to_value(item).expect("serialize item"),
            serde_json::json!({
                "event": "item",
                "data": {"sequence": 7, "item": "value"}
            })
        );

        let progress = HttpSseEvent::<()>::from(StreamEvent {
            sequence: 8,
            kind: StreamEventKind::Progress {
                completed: 2,
                total: Some(5),
            },
        });
        assert_eq!(
            serde_json::to_value(progress).expect("serialize progress"),
            serde_json::json!({
                "event": "progress",
                "data": {"sequence": 8, "completed": 2, "total": 5}
            })
        );
    }
}
