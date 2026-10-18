use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::native_runtime::RuntimeBuilder;

const HANG_GUARD: Duration = Duration::from_secs(10);
const SPIN: Duration = Duration::from_millis(300);
const WAKE_BOUND: Duration = Duration::from_millis(40);

/// On two workers, task B waits on a channel. Task A sends to it, then spins
/// for [`SPIN`] in the same poll. The idle worker must run B while A spins:
/// a wake raised by the executing task may not wait for that task's poll.
#[test]
fn a_peer_woken_by_a_long_poll_runs_during_that_poll() {
    let runtime = RuntimeBuilder::multi_thread()
        .worker_threads(2)
        .build()
        .expect("test runtime");
    let (tx, rx) = async_channel::bounded::<Instant>(1);
    let (woke, wakes) = mpsc::channel::<Duration>();
    let _b = runtime
        .handle()
        .try_spawn(async move {
            let sent = rx.recv().await.expect("a sends");
            let _ = woke.send(sent.elapsed());
        })
        .expect("spawn b");
    // B parks on the channel and both workers go idle.
    std::thread::sleep(Duration::from_millis(100));

    let _a = runtime
        .handle()
        .try_spawn(async move {
            tx.try_send(Instant::now()).expect("wake b");
            let spun = Instant::now();
            while spun.elapsed() < SPIN {
                std::hint::spin_loop();
            }
        })
        .expect("spawn a");

    let latency = wakes.recv_timeout(HANG_GUARD).expect("b wakes");
    assert!(
        latency < WAKE_BOUND,
        "b ran {latency:?} after a woke it, behind a's {SPIN:?} poll"
    );
}
