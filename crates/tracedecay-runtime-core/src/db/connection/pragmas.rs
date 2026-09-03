/// Computes adaptive `(cache_size_kb, mmap_size)` based on the DB file size.
///
/// - **`cache_size`**: 25% of DB size, clamped to \[2 MB, 64 MB\] (in KiB).
/// - **`mmap_size`**: 2× DB size, clamped to \[0, 256 MB\].
///
/// This avoids the fixed 320 MB memory baseline for small/medium projects.
#[cfg(test)]
pub(crate) fn adaptive_cache_sizes(db_file_size: u64) -> (u64, u64) {
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
pub(crate) fn platform_safe_mmap_size(_mmap: u64) -> u64 {
    0
}
