use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tracedecay::session_temporal_benchmark::IsolatedBenchmarkEnv;

#[test]
fn concurrent_benchmark_environments_do_not_overlap() {
    let (first_entered_tx, first_entered_rx) = mpsc::channel();
    let (first_release_tx, first_release_rx) = mpsc::channel();
    let first = thread::spawn(move || {
        let _isolated = IsolatedBenchmarkEnv::enter("session-temporal-env-first-").unwrap();
        first_entered_tx.send(()).unwrap();
        first_release_rx.recv().unwrap();
    });
    first_entered_rx.recv().unwrap();

    let (second_entered_tx, second_entered_rx) = mpsc::channel();
    let (second_release_tx, second_release_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        let _isolated = IsolatedBenchmarkEnv::enter("session-temporal-env-second-").unwrap();
        second_entered_tx.send(()).unwrap();
        second_release_rx.recv().unwrap();
    });

    let overlapped = second_entered_rx
        .recv_timeout(Duration::from_secs(1))
        .is_ok();
    if overlapped {
        second_release_tx.send(()).unwrap();
        second.join().unwrap();
        first_release_tx.send(()).unwrap();
        first.join().unwrap();
    } else {
        first_release_tx.send(()).unwrap();
        first.join().unwrap();
        second_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second environment guard should enter after the first is released");
        second_release_tx.send(()).unwrap();
        second.join().unwrap();
    }

    assert!(
        !overlapped,
        "process-global environment guards must not overlap"
    );
}
