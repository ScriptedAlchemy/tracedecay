//! Process-wide native engine timings, dumped when `TRACEDECAY_SQL_PROFILE=1`.
//! Counters stay cheap atomics so a profiled ingest can say whether time is
//! commit/fsync, progress ticks, or statement steps.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

const ENV: &str = "TRACEDECAY_SQL_PROFILE";

static COMMITS: AtomicU64 = AtomicU64::new(0);
static COMMIT_NS: AtomicU64 = AtomicU64::new(0);
static SYNCS: AtomicU64 = AtomicU64::new(0);
static SYNC_NS: AtomicU64 = AtomicU64::new(0);
static PROGRESS: AtomicU64 = AtomicU64::new(0);
static PROGRESS_NS: AtomicU64 = AtomicU64::new(0);
static IDENTITY: AtomicU64 = AtomicU64::new(0);
static IDENTITY_NS: AtomicU64 = AtomicU64::new(0);
static DRIVES: AtomicU64 = AtomicU64::new(0);
static DRIVE_NS: AtomicU64 = AtomicU64::new(0);
static COMPILES: AtomicU64 = AtomicU64::new(0);
static COMPILE_NS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn enabled() -> bool {
    std::env::var_os(ENV).is_some_and(|value| value == "1")
}

pub(crate) fn install() {
    if !enabled() {
        return;
    }
    // `Dump` prints once the process is leaving, after the last connection
    // has closed and the last sync has been counted.
    static DUMP: std::sync::OnceLock<Dump> = std::sync::OnceLock::new();
    DUMP.get_or_init(Dump::new);
}

pub(crate) fn commit<T>(op: impl FnOnce() -> T) -> T {
    if !enabled() {
        return op();
    }
    let started = Instant::now();
    let result = op();
    COMMITS.fetch_add(1, Ordering::Relaxed);
    add_ns(&COMMIT_NS, started);
    result
}

pub(crate) fn sync<T>(op: impl FnOnce() -> T) -> T {
    if !enabled() {
        return op();
    }
    let started = Instant::now();
    let result = op();
    SYNCS.fetch_add(1, Ordering::Relaxed);
    add_ns(&SYNC_NS, started);
    result
}

pub(crate) fn progress<T>(op: impl FnOnce() -> T) -> T {
    if !enabled() {
        return op();
    }
    let started = Instant::now();
    let result = op();
    PROGRESS.fetch_add(1, Ordering::Relaxed);
    add_ns(&PROGRESS_NS, started);
    result
}

pub(crate) fn identity<T>(op: impl FnOnce() -> T) -> T {
    if !enabled() {
        return op();
    }
    let started = Instant::now();
    let result = op();
    IDENTITY.fetch_add(1, Ordering::Relaxed);
    add_ns(&IDENTITY_NS, started);
    result
}

pub(crate) fn drive<T>(op: impl FnOnce() -> T) -> T {
    if !enabled() {
        return op();
    }
    let started = Instant::now();
    let result = op();
    DRIVES.fetch_add(1, Ordering::Relaxed);
    add_ns(&DRIVE_NS, started);
    result
}

pub(crate) fn compile<T>(op: impl FnOnce() -> T) -> T {
    if !enabled() {
        return op();
    }
    let started = Instant::now();
    let result = op();
    COMPILES.fetch_add(1, Ordering::Relaxed);
    add_ns(&COMPILE_NS, started);
    result
}

fn add_ns(target: &AtomicU64, started: Instant) {
    let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    target.fetch_add(nanos, Ordering::Relaxed);
}

pub(crate) fn dump() {
    if !enabled() {
        return;
    }
    let ms = |nanos: u64| nanos / 1_000_000;
    eprintln!(
        "TRACEDECAY_SQL_PROFILE commits={} commit_ms={} syncs={} sync_ms={} drives={} drive_ms={} compiles={} compile_ms={} progress={} progress_ms={} identity={} identity_ms={}",
        COMMITS.load(Ordering::Relaxed),
        ms(COMMIT_NS.load(Ordering::Relaxed)),
        SYNCS.load(Ordering::Relaxed),
        ms(SYNC_NS.load(Ordering::Relaxed)),
        DRIVES.load(Ordering::Relaxed),
        ms(DRIVE_NS.load(Ordering::Relaxed)),
        COMPILES.load(Ordering::Relaxed),
        ms(COMPILE_NS.load(Ordering::Relaxed)),
        PROGRESS.load(Ordering::Relaxed),
        ms(PROGRESS_NS.load(Ordering::Relaxed)),
        IDENTITY.load(Ordering::Relaxed),
        ms(IDENTITY_NS.load(Ordering::Relaxed)),
    );
}

struct Dump;

impl Dump {
    fn new() -> Self {
        Self
    }
}

impl Drop for Dump {
    fn drop(&mut self) {
        dump();
    }
}
