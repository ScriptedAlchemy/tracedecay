//! Two tests, one process. The sibling reaches the probe callsite with no
//! subscriber. The capture test goes through the shared helper, then reaches
//! that same callsite on a thread that still has no subscriber, then records
//! it. That registration caches `Interest::never()` unless the helper installed
//! the keepalive.

use std::thread;

use tracedecay_runtime_core::logging::capture_formatted_tracing;

fn probe_event() {
    tracing::info!(target: "fleet.keepalive.probe", "fleet keepalive probe");
}

#[test]
fn sibling_reaches_the_probe_callsite_with_no_subscriber() {
    probe_event();
}

#[test]
fn shared_capture_records_the_probe_after_an_unsubscribed_hit() {
    let ((), captured) = capture_formatted_tracing(|| {
        thread::spawn(probe_event)
            .join()
            .expect("unsubscribed probe thread");
        probe_event();
    });
    assert_eq!(
        captured,
        " INFO fleet.keepalive.probe: fleet keepalive probe\n"
    );
}
