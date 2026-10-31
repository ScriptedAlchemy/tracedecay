//! A read that outlasts the client's global timeout is a slow sample, not a
//! failed request.
//!
//! The dashboard's durable reads can wait behind a writer that holds the
//! store's lock across its own publish, so a polling helper must let its
//! caller's completion deadline decide when a wait failed. One timed-out
//! sample used to abort the whole wait: `ureq` reports a phase timeout as
//! `Error::Timeout(..)` ("timeout: global"), which the connection-level retry
//! classification did not match.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::common::{ReadRetry, get_json, http_agent_with_timeout, read_retry};

/// The answer the slow sample eventually serves, so a retried read is
/// distinguishable from a fixture that answered the first attempt.
const BODY: &str = r#"{"served":"after the client timeout"}"#;

/// The client gives up long before the first request is answered, and the
/// retry has the same margin to reach the immediate answer.
const CLIENT_TIMEOUT: Duration = Duration::from_millis(500);
const SLOW_SAMPLE: Duration = Duration::from_millis(1200);

#[test]
fn a_state_read_retries_when_the_server_outlasts_the_client_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind slow-read fixture");
    let address = listener.local_addr().expect("slow-read fixture address");
    let served = Arc::new(AtomicUsize::new(0));
    let handler_served = Arc::clone(&served);
    // Each connection is answered on its own thread: the first request sleeps
    // past the client's timeout, so the retry must not queue behind it.
    thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else {
                return;
            };
            let ordinal = handler_served.fetch_add(1, Ordering::SeqCst);
            thread::spawn(move || {
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                if ordinal == 0 {
                    // Still working when the client gives up on this sample.
                    thread::sleep(SLOW_SAMPLE);
                    return;
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{BODY}",
                    BODY.len()
                )
                .expect("write slow-read fixture response");
            });
        }
    });

    let agent = http_agent_with_timeout(CLIENT_TIMEOUT);
    let (status, payload) = get_json(
        &agent,
        &format!("http://{address}/api/automation/runs?limit=50"),
    );

    assert_eq!(status, 200);
    assert_eq!(payload["served"], "after the client timeout");
    assert!(
        served.load(Ordering::SeqCst) > 1,
        "the read must have retried the timed-out sample"
    );
}

/// The timeout budget is its own. A read still gets the fixture's full
/// connection-error budget — a freshly started server racing its readiness —
/// no matter how many slow samples it already retried.
#[test]
fn a_state_read_keeps_the_full_connection_retry_budget() {
    let refused = ureq::Error::ConnectionFailed;
    let mut connection_failures = 0;
    loop {
        match read_retry(&refused, connection_failures, 0) {
            ReadRetry::Retry(delay) => {
                assert_eq!(
                    delay,
                    Duration::from_millis(25 * (connection_failures as u64 + 1)),
                    "connection backoff keeps the fixture's schedule"
                );
                connection_failures += 1;
            }
            ReadRetry::Exhausted => break,
            ReadRetry::Fatal => panic!("a connection-level failure is retryable"),
        }
    }
    assert_eq!(
        connection_failures + 1,
        12,
        "a read keeps the full connection-error attempt budget"
    );

    let timed_out = ureq::Error::Timeout(ureq::Timeout::Global);
    assert!(matches!(read_retry(&timed_out, 0, 2), ReadRetry::Retry(_)));
    assert_eq!(read_retry(&timed_out, 0, 3), ReadRetry::Exhausted);
    assert!(
        matches!(read_retry(&refused, 0, 3), ReadRetry::Retry(_)),
        "spent timeout retries never shorten the connection budget"
    );
}
