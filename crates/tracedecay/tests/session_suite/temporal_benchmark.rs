use std::sync::{Arc, Barrier};
use std::thread;

use tracedecay::session_temporal_benchmark::IsolatedBenchmarkEnv;

/// Benchmark environments hand their home and data directory to the harness
/// explicitly, so two of them run at the same time without sharing either.
#[test]
fn concurrent_benchmark_environments_stay_disjoint() {
    let both_entered = Arc::new(Barrier::new(2));
    let enter = |prefix: &'static str| {
        let both_entered = Arc::clone(&both_entered);
        thread::spawn(move || {
            let isolated = IsolatedBenchmarkEnv::enter(prefix).unwrap();
            both_entered.wait();
            (
                isolated.home().to_path_buf(),
                isolated.data_dir().to_path_buf(),
                isolated.home().is_dir() && isolated.data_dir().is_dir(),
            )
        })
    };
    let first = enter("session-temporal-env-first-");
    let second = enter("session-temporal-env-second-");
    let (first_home, first_data, first_ready) = first.join().unwrap();
    let (second_home, second_data, second_ready) = second.join().unwrap();

    assert!(first_ready && second_ready);
    for (left, right) in [
        (&first_home, &second_home),
        (&first_data, &second_data),
        (&first_home, &second_data),
        (&first_data, &second_home),
    ] {
        assert!(
            !left.starts_with(right) && !right.starts_with(left),
            "concurrent benchmark environments share {} and {}",
            left.display(),
            right.display()
        );
    }
}
