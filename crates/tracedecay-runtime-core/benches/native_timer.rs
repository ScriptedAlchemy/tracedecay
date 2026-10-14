//! Per-call cost of native timers awaited from a Tokio worker.
//!
//! `native_runtime::sleep_until` runs each timer on its own native task so a
//! foreign executor never polls an Asupersync timer. These cases price that
//! spawn against Tokio's timer and a direct Asupersync timer, to decide which
//! loops may use it:
//!
//! ```sh
//! bazel test //crates/tracedecay-runtime-core:native_timer --test_arg=--bench
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use tracedecay_runtime_core::native_runtime;

const FAR: Duration = Duration::from_secs(3600);

fn per_call<F, Fut>(runtime: &tokio::runtime::Runtime, call: F) -> impl FnMut(u64) -> Duration
where
    F: Fn() -> Fut + Copy + Send + 'static,
    Fut: Future + Send,
    Fut::Output: Send,
{
    move |iterations| {
        runtime.block_on(async move {
            tokio::spawn(async move {
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(call().await);
                }
                start.elapsed()
            })
            .await
            .expect("benchmark task")
        })
    }
}

async fn ready_after_one_yield() -> u32 {
    tokio::task::yield_now().await;
    7
}

fn native_timers(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    native_runtime::process_runtime().expect("native runtime");
    let mut group = c.benchmark_group("timer_from_tokio");
    group.bench_function("native_timeout_ready_future", |b| {
        b.iter_custom(per_call(&runtime, || {
            native_runtime::timeout_at(Instant::now() + FAR, async { 7 })
        }));
    });
    group.bench_function("native_timeout_completes_before_deadline", |b| {
        b.iter_custom(per_call(&runtime, || {
            native_runtime::timeout_at(Instant::now() + FAR, ready_after_one_yield())
        }));
    });
    group.bench_function("tokio_timeout_completes_before_deadline", |b| {
        b.iter_custom(per_call(&runtime, || {
            tokio::time::timeout(FAR, ready_after_one_yield())
        }));
    });
    group.bench_function("asupersync_timeout_completes_before_deadline", |b| {
        b.iter_custom(per_call(&runtime, || {
            asupersync::time::timeout(asupersync::time::wall_now(), FAR, ready_after_one_yield())
        }));
    });
    group.bench_function("native_sleep_until_past_deadline", |b| {
        b.iter_custom(per_call(&runtime, || {
            native_runtime::sleep_until(Instant::now())
        }));
    });
    group.bench_function("native_timeout_elapses_past_deadline", |b| {
        b.iter_custom(per_call(&runtime, || {
            native_runtime::timeout_at(Instant::now(), std::future::pending::<()>())
        }));
    });
    group.bench_function("tokio_sleep_until_past_deadline", |b| {
        b.iter_custom(per_call(&runtime, || {
            tokio::time::sleep_until(tokio::time::Instant::now())
        }));
    });
    group.bench_function("asupersync_sleep_zero", |b| {
        b.iter_custom(per_call(&runtime, || {
            asupersync::time::sleep(asupersync::time::wall_now(), Duration::ZERO)
        }));
    });
    group.finish();
}

criterion_group!(benches, native_timers);
criterion_main!(benches);
