# Draft: bulk-insert workload runs 1.4-1.6x slower than SQLite (not posted)

Turso rev `2829ee1662bd01d79f60c2c170af684a2b50af86`, opt build, Linux x86_64.

## Workload

TraceDecay's lexical artifact build on 265 source files: 79 write
transactions (multi-row and single-row `INSERT`s into rowid tables with
unique keys, then `ORDER BY` merges and a canonical copy into a fresh file),
one writer thread, `synchronous = FULL`, `cache_size = -65536`, WAL with the
automatic checkpoint off. Five interleaved runs per engine; counters are the
writer thread's own (`perf stat --no-inherit -t`), which are independent of
host load.

| engine | writer instructions:u | writer cycles:u | ingest median | ingest min |
|---|---|---|---|---|
| SQLite 3 (bundled) | 60.9 G | 38.7 G | 14.1 s | 9.8 s |
| Turso, unpatched | 99.3 G | 65.2 G | 19.8 s | 16.9 s |
| Turso + evictable-count fix | 97.9 G | 59.5 G | 21.1 s | 15.4 s |

Records: `benchmark_data/storage-concurrency/native-perf.jsonl`, tag
`perf2-writer`. Wall times were taken at load average 30-118 on a shared
96-core host; compare the counters.

## Findings

1. `PageCache::evictable_count` never rises when a commit cleans pages
   (`core/storage/wal.rs:4625` clears the flag, `core/storage/pager.rs:4752`
   never notifies the cache), so `needs_spill` runs its O(n)
   `count_evictable_pages` (`core/storage/page_cache.rs:496`) on every page
   allocation once the cache is 90% full. Patch:
   `scripts/bazel/patches/turso_core-evictable-count-on-commit.patch`
   (writer cycles -8.7%). Note: `evictable-count-never-rises-on-commit.md`.
2. `TempFile::new` (`core/io/mod.rs:302`) always uses the system temporary
   directory, so a confined `IO` must force `TempStore::Memory` and sorts
   cannot spill. Patch: `scripts/bazel/patches/turso_core-io-temp-dir.patch`.
   Note: `temp-files-ignore-the-io-backend.md`.
3. Per-row insert cost. Statement-level timings put plain inserts at 4-7x
   SQLite per row (`INSERT INTO clone_fingerprint_postings`: 7.2 us against
   1.36 us over 117,764 rows into a fresh file, where no spill runs). The
   remaining 37 G writer instructions (+61%) are spread through the VDBE and
   B-tree insert path with no single dominant frame: `Statement::_step`,
   `BTreeCursor::indexbtree_move_to_internal`, `indexbtree_seek_inner`,
   `compare_payload_generic`, `op_make_record`, `ProgramState::reset`, and
   glibc `malloc`/`free`/`malloc_consolidate` together at about 12% of the
   writer thread's on-CPU samples over the whole bench run, which also
   includes TraceDecay's own allocation (per-record and per-page temporary
   allocations; the 3 MiB buffer-pool
   arena, `core/storage/buffer_pool.rs:130`, covers 768 of the cache's 16384
   pages, and the rest fall back to `Buffer::new_temporary`).
4. Spilled pages are read back and written twice. A commit re-reads every
   spilled page that was evicted (`PageSource::Evicted`,
   `core/storage/pager.rs:4568`) to append it again as a commit frame;
   SQLite's commit reuses the spill frames. Raising the cache to 256 MiB left
   the pread count unchanged (14,943 against 14,947), so on this workload the
   reads come from cold source pages in the canonical copy, not from spills.
5. Vectored WAL appends of up to `IOV_MAX` = 1024 frames
   (`core/storage/wal.rs:2558`) allocate large page-cache folios; on a host
   under memory pressure 6.4% of the writer's samples sit in
   `__x64_sys_pwritev` -> `__alloc_pages_slowpath` (direct reclaim and
   compaction). SQLite's 4 KiB `pwrite`s stay on the order-0 fast path. This
   is host-dependent.

## Asks

- Take fixes 1 and 2 (or equivalents).
- A buffer-pool arena that grows with `cache_size`, or a per-connection page
  buffer free list, to take allocation off the insert path.
- Reuse spill frames at commit instead of re-reading evicted pages.
