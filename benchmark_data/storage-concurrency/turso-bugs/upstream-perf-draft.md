# Draft: bulk-insert workload runs 1.3-1.5x slower than SQLite (not posted)

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
| + temp-dir fix (base of the next rows) | 97.9 G | 61.2 G | 19.1 s | 17.0 s |
| + unique-index seek reuse | 89.2 G | 61.4 G | 19.9 s | 16.0 s |
| + one-copy parameter bind (TraceDecay driver) | 88.8 G | 65.9 G | 24.5 s | 19.6 s |

Records: `benchmark_data/storage-concurrency/native-perf.jsonl`, tags
`perf2-writer`, `perf3-baseline`, `perf3-seek` and `perf3-bind`. Wall times
and cycles were taken at load average 30-118 on a shared 96-core host and
move by 20-30% between runs of one binary; compare the instruction counts.

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
3. A unique index insert seeks the index twice: `emit_commit_phase`
   (`core/translate/insert.rs:1382`) emits `IdxInsert` without `USE_SEEK`
   after the preflight `NoConflict` probe already positioned the cursor, so
   `op_idx_insert` (`core/vdbe/execute.rs:13156`) descends from the root
   again. That second seek was 7.5% of the writer's instructions:u. Patch:
   `scripts/bazel/patches/turso_core-insert-use-seek-result.patch` (writer
   instructions 97.9 G to 89.2 G, byte-identical artifact). Note:
   `unique-index-insert-seeks-twice.md`.
4. Per-row cost that remains. A frame-pointer build's instruction profile
   of the writer (2 M-instruction samples, 49,813 of them) puts 47.7% of the
   writer inside `Statement::_step` and 41% in TraceDecay itself. With the
   double seek gone, the remaining Turso cost is spread over the insert path:
   `NoConflict` probes 6.7% (`indexbtree_move_to_internal`,
   `indexbtree_seek_inner` and the `compare_payload_*` family, about 11 k
   instructions per probe), `BTreeCursor::insert` 9.4% (balance 4.0%,
   `Pager::add_dirty` 1.7% with `subjournal_page_if_required` 1.0% and the
   dirty-page `RoaringBitmap` insert 0.4% even for an already dirty page),
   per-execution setup and teardown about 7% (`ProgramState::reset` 1.8%,
   `op_open_write` 1.9% boxing a fresh `BTreeCursor` and `Arc<IndexInfo>`
   per execution, `op_transaction` 1.1% re-reading the header page for the
   schema cookie, `op_halt`, `Program::abort`), `op_variable` 0.9% cloning
   each text or blob parameter into a register that the next reset drops,
   and the sorter 2.5%.
5. Allocation. A `malloc` size histogram of the writer thread (bpftrace
   uprobe) counts 27.8 M calls for Turso against 12.1 M for SQLite, and only
   7,082 of Turso's fall in the 4-4.5 KiB page-buffer range, so the
   buffer-pool arena (`core/storage/buffer_pool.rs:130`) is not the
   allocation cost; the thread-local temporary buffer cache absorbs page
   churn. The extra 15.7 M are small (4 to 136 bytes): cursor boxes, record
   value vectors (`try_from_result_iter`), parameter and register clones.
6. Spilled pages are read back and written twice at commit
   (`PageSource::Evicted`, `core/storage/pager.rs:4568`). On this workload it
   costs nothing measurable: a 256 MiB cache, where nothing spills, left the
   pread count unchanged (14,943 against 14,947).
7. Vectored WAL appends of up to `IOV_MAX` = 1024 frames
   (`core/storage/wal.rs:2558`) can push the kernel into direct reclaim on a
   host under memory pressure. Capping them at 64 frames moved neither the
   writer's instructions (89.2 G against 89.3 G) nor its minimum CPU time
   (18.9 s against 19.6 s), so it is not carried.

## Asks

- Take fixes 1, 2 and 3 (or equivalents).
- Cut per-execution overhead for prepared single-row statements: reuse the
  cursor allocation and `IndexInfo` across executions, cache the schema
  cookie for the duration of a write transaction, and skip the dirty-set
  insert for a page that is already dirty.
- Cheaper index key comparison on seeks (decode the probe key once, compare
  serialized fields without building `ValueRef` vectors).
