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

use crate::common::{get_json, http_agent_with_timeout};

/// The answer the slow sample eventually serves, so a retried read is
/// distinguishable from a fixture that answered the first attempt.
const BODY: &str = r#"{"served":"after the client timeout"}"#;

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
                    thread::sleep(Duration::from_millis(400));
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

    let agent = http_agent_with_timeout(Duration::from_millis(150));
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
