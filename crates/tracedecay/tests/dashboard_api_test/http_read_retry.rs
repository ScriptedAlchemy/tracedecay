//! State reads can recover from a client timeout with a bounded retry.
//!
//! The dashboard's durable reads can wait behind a writer that holds the
//! store's lock across its own publish. A bounded retry lets a later sample
//! observe progress; it does not enforce the polling caller's deadline.
//! One timed-out sample used to abort the whole wait: `ureq` reports a phase
//! timeout as `Error::Timeout(..)` ("timeout: global"), which the connection-level
//! retry classification did not match.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use crate::common::{ReadRetry, get_json, http_agent_with_timeout, read_retry};

/// Only a later sample serves this response; the first connection stays open
/// without a response until fixture shutdown.
const BODY: &str = r#"{"served":"after the client timeout"}"#;

/// Keep the original client deadline; the server holds the first sample open
/// until the read finishes rather than racing a sleep against this timeout.
const CLIENT_TIMEOUT: Duration = Duration::from_millis(150);

#[test]
fn a_state_read_retries_when_the_server_outlasts_the_client_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind slow-read fixture");
    let address = listener.local_addr().expect("slow-read fixture address");
    let stopped = Arc::new(AtomicBool::new(false));
    let server_stopped = Arc::clone(&stopped);
    let (ready_tx, ready_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut served = 0;
        let mut slow_sample = None;
        ready_tx
            .send(())
            .expect("signal slow-read fixture readiness");
        loop {
            let (mut stream, _) = listener.accept().expect("accept fixture connection");
            if server_stopped.load(Ordering::Acquire) {
                break;
            }
            stream
                .set_read_timeout(Some(CLIENT_TIMEOUT))
                .expect("bound fixture request read");
            stream
                .set_write_timeout(Some(CLIENT_TIMEOUT))
                .expect("bound fixture response write");
            let mut request = [0_u8; 1024];
            assert!(stream.read(&mut request).expect("read fixture request") > 0);
            served += 1;
            if served == 1 {
                // Holding this connection open forces a client timeout without
                // serializing later requests behind a sleeping handler.
                slow_sample = Some(stream);
                continue;
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{BODY}",
                BODY.len()
            )
            .expect("write slow-read fixture response");
        }
        drop(slow_sample);
        served
    });

    // Catch failures so shutdown always wakes accept and joins the server
    // before propagating the assertion. There are no detached handler threads.
    let result = catch_unwind(AssertUnwindSafe(|| {
        ready_rx.recv().expect("slow-read fixture ready");
        let agent = http_agent_with_timeout(CLIENT_TIMEOUT);
        get_json(
            &agent,
            &format!("http://{address}/api/automation/runs?limit=50"),
        )
    }));
    stopped.store(true, Ordering::Release);
    let wake = TcpStream::connect_timeout(&address, CLIENT_TIMEOUT);
    let served = server.join().expect("join slow-read fixture");
    drop(wake.expect("wake slow-read fixture accept"));
    let (status, payload) = result.unwrap_or_else(|panic| resume_unwind(panic));

    assert_eq!(status, 200);
    assert_eq!(payload["served"], "after the client timeout");
    assert!(
        served > 1,
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
