use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

use tempfile::TempDir;
use tracedecay_runtime_core::db::engine::{TestConnection, params};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

const WRITES: usize = 256;
const CONCURRENCY: usize = 8;

fn sample_blocking_workers(peak: &AtomicUsize) {
    #[cfg(tokio_unstable)]
    {
        let metrics = tokio::runtime::Handle::current().metrics();
        let active = metrics
            .num_blocking_threads()
            .saturating_sub(metrics.num_idle_blocking_threads());
        peak.fetch_max(active, Ordering::Relaxed);
    }
    #[cfg(not(tokio_unstable))]
    let _ = peak;
}

#[hotpath::measure(label = "exact_sql_workload.write_ack", future = true)]
async fn write_ack(connection: &TestConnection, id: usize, peak: &AtomicUsize) -> u64 {
    let started = Instant::now();
    assert_eq!(
        connection
            .execute(
                "INSERT INTO acknowledgement_workload(id) VALUES (?1)",
                params![id as i64]
            )
            .await
            .unwrap(),
        1
    );
    let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap();
    sample_blocking_workers(peak);
    elapsed
}

fn report(label: &str, mut samples: Vec<u64>, elapsed: std::time::Duration, peak: &AtomicUsize) {
    assert_eq!(samples.len(), WRITES);
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({
            "workload": label,
            "writes": WRITES,
            "concurrency": if label == "sequential" { 1 } else { CONCURRENCY },
            "elapsed_ns": elapsed.as_nanos(),
            "ack_p50_ns": samples[(samples.len() - 1) / 2],
            "ack_p95_ns": samples[(samples.len() * 95).div_ceil(100) - 1],
            "observed_active_blocking_workers_peak": if cfg!(tokio_unstable) { Some(peak.load(Ordering::Relaxed)) } else { None },
            "blocking_worker_sampling": "after_each_ack_lower_bound"
        })
    );
}

fn main() {
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("exact-sql-writer-workload")
        .sections_exclude(vec![hotpath::Section::FunctionsCpu])
        .build();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let directory = TempDir::new().unwrap();
        let connection = Arc::new(TestConnection::open(
            &directory.path().join("writer.sqlite3"),
        ));
        connection
            .execute(
                "CREATE TABLE acknowledgement_workload(id INTEGER PRIMARY KEY)",
                (),
            )
            .await
            .unwrap();
        // Warm the actor and blocking pool before collecting either phase.
        connection
            .execute("INSERT INTO acknowledgement_workload VALUES (-1)", ())
            .await
            .unwrap();
        let peak = AtomicUsize::new(0);
        let started = Instant::now();
        let mut sequential = Vec::with_capacity(WRITES);
        for id in 0..WRITES {
            sequential.push(write_ack(&connection, id, &peak).await);
        }
        report("sequential", sequential, started.elapsed(), &peak);

        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
        let started = Instant::now();
        let mut workers = tokio::task::JoinSet::new();
        for worker in 0..CONCURRENCY {
            let connection = Arc::clone(&connection);
            let peak = Arc::clone(&peak);
            let barrier = Arc::clone(&barrier);
            workers.spawn(async move {
                barrier.wait().await;
                let mut samples = Vec::with_capacity(WRITES / CONCURRENCY);
                for index in 0..WRITES / CONCURRENCY {
                    let id = WRITES + worker * (WRITES / CONCURRENCY) + index;
                    samples.push(write_ack(&connection, id, &peak).await);
                }
                samples
            });
        }
        let mut concurrent = Vec::with_capacity(WRITES);
        while let Some(result) = workers.join_next().await {
            concurrent.extend(result.unwrap());
        }
        report("concurrent", concurrent, started.elapsed(), &peak);
        let mut rows = connection
            .query("SELECT COUNT(*), SUM(id) FROM acknowledgement_workload", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), (2 * WRITES + 1) as i64);
        assert_eq!(
            row.get::<i64>(1).unwrap(),
            ((2 * WRITES - 1) * (2 * WRITES) / 2) as i64 - 1
        );
    });
}
