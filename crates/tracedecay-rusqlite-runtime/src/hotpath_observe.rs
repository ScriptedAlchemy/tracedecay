//! Opt-in rusqlite-runtime Hotpath gauges.
//!
//! These exist to answer one question the writer's own timings cannot: when
//! `begin_immediate` averages 10.65 ms against an 82 µs p95, a few transactions
//! are blocking for ~200 ms while nearly all are instant. `BEGIN IMMEDIATE`
//! takes SQLite's write lock, so something else was holding it — and the
//! maintenance paths (checkpoint, incremental vacuum, online backup) take that
//! lock outside the normal write queue.
//!
//! Function timings alone cannot separate those, because every write funnels
//! through the same `repository::execute` / `submit_authorized` frames whatever
//! submitted it. Counting the maintenance operations and the time they hold the
//! database makes the blocking attributable instead of merely suspected.
//!
//! Counts and durations only; never table contents, paths, or SQL text.

#[inline(always)]
fn add(name: &'static str, delta: u64) {
    #[cfg(feature = "hotpath")]
    {
        if delta == 0 {
            return;
        }
        hotpath::gauge!(name).inc(delta);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = (name, delta);
}

/// One WAL checkpoint, and how long it ran.
///
/// A checkpoint holds the database while it copies WAL frames back, so its
/// total duration is the budget every concurrent writer waits inside. Compare
/// this against `rusqlite.begin_immediate`'s total: if they track each other,
/// the writer tail is checkpointing, not write throughput.
#[inline(always)]
pub(crate) fn record_checkpoint(elapsed: std::time::Duration, complete: bool) {
    add("rusqlite.checkpoint.runs", 1);
    add(
        "rusqlite.checkpoint.micros",
        u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
    );
    if !complete {
        // A checkpoint that could not drain the WAL leaves the pressure that
        // caused it, so the next one runs sooner and holds the lock again.
        add("rusqlite.checkpoint.incomplete", 1);
    }
}

/// One writer submission, labelled by the priority it was admitted under.
///
/// Session ingestion and code indexing share every writer frame, so a run that
/// does both cannot attribute `submit_authorized` counts from timings alone —
/// which is exactly what made an earlier batches-per-frame ratio unquotable.
/// Priority is already on the operation metadata, so splitting on it costs
/// nothing and needs no new field threaded through the contract.
#[inline(always)]
pub(crate) fn record_writer_submit(priority: tracedecay_store::OperationPriorityV1) {
    add(
        match priority {
            tracedecay_store::OperationPriorityV1::Foreground => {
                "rusqlite.writer.submits.foreground"
            }
            tracedecay_store::OperationPriorityV1::Background => {
                "rusqlite.writer.submits.background"
            }
            tracedecay_store::OperationPriorityV1::Health => "rusqlite.writer.submits.health",
        },
        1,
    );
}
