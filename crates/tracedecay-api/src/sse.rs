use std::future;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt, stream};
use serde::Serialize;
use tracedecay_application::{RequestId, StreamEvent, StreamFrontier};

use crate::{HttpAdapterError, HttpSseEvent};

const SSE_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Frame a canonical application stream with Axum's SSE implementation.
///
/// Ordering, resume authorization, and cancellation are application/runtime
/// responsibilities. This adapter prepends the canonical open frontier,
/// publishes sequence IDs as SSE resume cursors, and closes after the first
/// terminal event so stale producer callbacks cannot escape.
pub fn sse_response<S, T>(
    correlation_id: RequestId,
    frontier: StreamFrontier,
    source: S,
) -> Sse<impl Stream<Item = Result<Event, HttpAdapterError>>>
where
    S: Stream<Item = StreamEvent<T>> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let open = stream::once(future::ready(encode_event(HttpSseEvent::<T>::Open {
        correlation_id,
        frontier,
    })));
    Sse::new(open.chain(encode_events(source))).keep_alive(
        KeepAlive::new()
            .interval(SSE_KEEP_ALIVE_INTERVAL)
            .text("heartbeat"),
    )
}

fn encode_events<S, T>(source: S) -> impl Stream<Item = Result<Event, HttpAdapterError>>
where
    S: Stream<Item = StreamEvent<T>>,
    T: Serialize,
{
    source.scan(false, |terminal_seen, event| {
        if *terminal_seen {
            return future::ready(None);
        }
        let event = HttpSseEvent::from(event);
        *terminal_seen = event.is_terminal();
        future::ready(Some(encode_event(event)))
    })
}

fn encode_event<T>(event: HttpSseEvent<T>) -> Result<Event, HttpAdapterError>
where
    T: Serialize,
{
    let name = event.event_name();
    let sequence = event.sequence();
    let mut encoded = Event::default().event(name);
    if let Some(sequence) = sequence {
        encoded = encoded.id(sequence.to_string());
    }
    encoded
        .json_data(event)
        .map_err(|_| HttpAdapterError::EventEncoding)
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll};

    use futures_util::Stream;
    use futures_util::task::noop_waker_ref;
    use serde_json::json;
    use tracedecay_application::{StreamEvent, StreamTermination};

    use super::{encode_events, stream};

    #[test]
    fn terminal_event_closes_before_stale_source_callbacks() {
        let terminal: StreamTermination = serde_json::from_value(json!({
            "termination": "completed",
            "receipt": {
                "started_at": 1,
                "ended_at": 2,
                "effective_deadline": {"expires_at": 3},
                "cancellation": null,
                "budget": {
                    "units_consumed": 1,
                    "bytes_consumed": 0,
                    "elapsed_micros": 1
                },
                "termination": "completed"
            }
        }))
        .expect("terminal fixture");
        let terminal = StreamEvent::<&str>::terminal(0, terminal).expect("terminal event");
        let stale = StreamEvent::item(1, "stale").expect("stale item");
        let mut encoded = Box::pin(encode_events(stream::iter([terminal, stale])));
        let mut context = Context::from_waker(noop_waker_ref());

        assert!(matches!(
            encoded.as_mut().poll_next(&mut context),
            Poll::Ready(Some(Ok(_)))
        ));
        assert!(matches!(
            encoded.as_mut().poll_next(&mut context),
            Poll::Ready(None)
        ));
    }
}
