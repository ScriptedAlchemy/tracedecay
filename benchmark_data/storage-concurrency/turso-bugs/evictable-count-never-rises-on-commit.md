# The page cache's evictable count never rises when a commit cleans pages

Turso rev `2829ee1662bd01d79f60c2c170af684a2b50af86`.

`PageCache` keeps `evictable_count` so `needs_spill`
(`core/storage/page_cache.rs:461`) can answer without walking the cache. The
count falls when a page turns dirty (`notify_page_dirty`, `:544`) and rises on
insert of a clean page or on `notify_page_spilled`. Nothing raises it when a
commit marks its pages clean: `WalFile::finalize_committed_pages`
(`core/storage/wal.rs:4625`) clears the dirty flag of every committed page,
and `Pager::commit_wal_inner` (`core/storage/pager.rs:4752`,
`CommitState::WalCommitDone`) never tells the cache. Eviction of those clean
pages then decrements the count again, so after the first commit of a
transaction larger than the cache it stays near zero.

Effect: once the cache passes its 90% spill threshold, every page allocation
fails the fast path in `needs_spill` and runs `count_evictable_pages`
(`:496`), an O(n) walk of the whole map, which finds the clean pages and
answers "no spill". With a 64 MiB cache that is a 16384-entry hash-map walk
per allocated page.

Repro: any connection with `cache_size` below the transaction working set
that commits more than once.

```rust
conn.execute("PRAGMA cache_size = -65536")?;  // 16384 pages
for _ in 0..80 {
    // one transaction writing ~64 MiB of new rows across several tables
    conn.execute("BEGIN")?; /* inserts */ conn.execute("COMMIT")?;
}
// perf: <hashbrown RawIterRange<(PageCacheKey, *mut PageCacheEntry)>>::fold_impl
// is the hottest turso frame of the writer thread.
```

Measured on TraceDecay's lexical artifact build (265 files, 79 commits),
writer thread: `fold_impl` was 3.9% of the writer's on-CPU samples and 5% of
its wall-clock stack samples (10 of 202), half of all samples inside
`BTreeCursor::insert`.

Fix carried in `scripts/bazel/patches/turso_core-evictable-count-on-commit.patch`:
`PageCache::notify_page_clean`, called for every committed page in
`CommitState::WalCommitDone` before `finalize_committed_pages`, raises the
count for a cached dirty, unspilled page. Measured over five interleaved runs
(`benchmark_data/storage-concurrency/native-perf.jsonl`, tag `perf2-writer`,
engine `native-h2` against `native-base`), writer thread only: cycles:u
65.2 G to 59.5 G (-8.7%), instructions:u 99.3 G to 97.9 G, fastest ingest
16.9 s to 15.4 s.

Pinned by `spill_check_work_per_commit_stays_flat_as_the_store_grows` in
`crates/tracedecay-sql/src/native/tests.rs`. It counts the cache entries
`count_evictable_pages` walks, through the thread-local counter that
`scripts/bazel/patches/turso_core-page-cache-walk-counter.patch` adds. The
test runs six commits of 600 one-page rows each against a 200-page cache.
With the fix every commit walks about 1,200 entries. Without the
`notify_page_clean` call the walks per commit go 1,182, 20,800, 27,000,
28,000, 28,800 and 28,000.
