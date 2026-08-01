/// Env var that, when set to `1`, switches every `TraceDecay` `SQLite`
/// connection to `journal_mode=MEMORY` + `synchronous=OFF` on all platforms.
///
/// **For tests/CI only — must never be set in production.** It trades away
/// crash durability entirely: a process or OS crash mid-transaction can
/// corrupt the database. CI test runs don't care (every DB is a throwaway
/// fixture), and on Windows this avoids the per-transaction rollback-journal
/// file create/write/fsync/delete cost of the `DELETE`+`FULL` pairing. An
/// in-memory journal also never enters WAL mode, so it sidesteps the Windows
/// WAL close-time teardown crash the same way `DELETE` does.
pub const SQLITE_UNSAFE_FAST_ENV: &str = "TRACEDECAY_SQLITE_UNSAFE_FAST";

/// Computes adaptive `(cache_size_kb, mmap_size)` based on the DB file size.
///
/// - **`cache_size`**: 25% of DB size, clamped to \[2 MB, 64 MB\] (in KiB).
/// - **`mmap_size`**: 2× DB size, clamped to \[0, 256 MB\].
///
/// This avoids the fixed 320 MB memory baseline for small/medium projects.
#[cfg(test)]
pub fn adaptive_cache_sizes(db_file_size: u64) -> (u64, u64) {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;

    // cache_size: 25% of DB, clamped [2 MB .. 64 MB], expressed in KiB
    let cache_bytes = (db_file_size / 4).clamp(2 * MB, 64 * MB);
    let cache_kb = cache_bytes / KB;

    // mmap_size: 2× DB, clamped [0 .. 256 MB]
    let mmap = db_file_size.saturating_mul(2).min(256 * MB);

    (cache_kb, mmap)
}

/// Returns the `mmap_size` that is actually safe to apply on the current
/// platform.
///
/// Graph-store mmap is disabled on every platform. Long-lived daemon handles
/// and short-lived peers previously retained divergent mapped page views
/// across WAL checkpoints; ordinary file I/O keeps `SQLite`'s locking and WAL
/// coherence mechanisms authoritative.
#[cfg(test)]
pub fn platform_safe_mmap_size(_mmap: u64) -> u64 {
    0
}

#[cfg(test)]
fn sqlite_unsafe_fast_enabled() -> bool {
    std::env::var(SQLITE_UNSAFE_FAST_ENV).as_deref() == Ok("1")
}

/// Returns the `journal_mode` safe for the current platform.
///
/// Windows `SQLite` local databases can intermittently fault while closing
/// WAL-mode databases under nextest's per-test process isolation. Disabling
/// mmap removed one unsafe teardown path, but master CI still aborts in
/// unrelated tests as different short-lived databases close. Use rollback
/// journaling on Windows and keep WAL everywhere else.
///
/// When [`SQLITE_UNSAFE_FAST_ENV`] is `1` (tests/CI only — never set it in
/// production) this returns `MEMORY` on every platform, skipping journal file
/// I/O entirely at the cost of crash durability.
#[cfg(test)]
pub fn platform_safe_journal_mode() -> &'static str {
    if sqlite_unsafe_fast_enabled() {
        "MEMORY"
    } else if cfg!(windows) {
        "DELETE"
    } else {
        "WAL"
    }
}

/// Returns the `synchronous` level paired with the current platform journal.
///
/// `NORMAL` is consistency-safe for WAL because WAL can recover from a missing
/// final fsync, but rollback journals need `FULL` to avoid corruption after an
/// OS crash or power loss. Keep the faster WAL+NORMAL pairing on non-Windows
/// and use DELETE+FULL on Windows.
///
/// When [`SQLITE_UNSAFE_FAST_ENV`] is `1` (tests/CI only — never set it in
/// production) this returns `OFF` on every platform, skipping fsyncs entirely
/// at the cost of crash durability.
#[cfg(test)]
pub fn platform_safe_synchronous_mode() -> &'static str {
    if sqlite_unsafe_fast_enabled() {
        "OFF"
    } else if cfg!(windows) {
        "FULL"
    } else {
        "NORMAL"
    }
}
