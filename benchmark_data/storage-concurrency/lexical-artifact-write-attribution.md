# Lexical artifact writes per table (#3491, PERF5)

Corpus: 265 files (`/fast/tmp/tc-u12e-corpus`), opt builds, writer thread
only. Per-statement instructions:u come from a measurement-only build that
read a per-thread `perf_event_open` counter around every statement: native
spans cover bind, step and reset; SQLite spans run from the trace `STMT`
event to the `PROFILE` event, so a SQLite streaming `SELECT` also carries the
caller's row work between steps. Nested statements are charged exclusively.
The hooks are not part of the branch (commit `9d3d286017`, reverted by
`4a57a4fa37`). Rows inserted and bytes bound count both the staging write
and the canonical layout copy.

| table | rows inserted | bytes bound | native VM M | SQLite VM M | native G | SQLite G | native-minus-SQLite G | after: native G | after: SQLite G |
|---|---|---|---|---|---|---|---|---|---|
| clone_fingerprint_postings | 235528 | 10.57 MB | 47.67 | 10.47 | 12.34 | 5.86 | 6.49 | 8.57 | 3.33 |
| term_posting_runs | 269206 | 12.58 MB | 12.20 | 9.50 | 7.63 | 3.92 | 3.71 | 3.34 | 2.75 |
| term_postings | 155988 | 11.94 MB | 6.94 | 4.73 | 4.94 | 2.39 | 2.55 | 3.88 | 1.23 |
| ngram_postings | 81380 | 16.79 MB | 3.21 | 2.40 | 2.41 | 1.13 | 1.28 | 1.90 | 0.50 |
| row_chunks | 80140 | 1.60 MB | 2.40 | 1.92 | 1.90 | 0.94 | 0.97 | 1.65 | 0.55 |
| row_dictionary_pages | 39200 | 4.17 MB | 2.19 | 1.96 | 1.21 | 0.48 | 0.73 | 0.49 | 0.22 |
| exact_vocabulary | 42978 | 1.54 MB | 1.18 | 0.98 | 1.16 | 0.57 | 0.59 | 1.02 | 0.43 |
| exact_postings | 42978 | 1.00 MB | 1.35 | 1.07 | 1.06 | 0.50 | 0.57 | 0.79 | 0.23 |
| row_chunk_pages | 40070 | 1.60 MB | 1.86 | 1.61 | 0.79 | 0.26 | 0.53 | 0.79 | 0.26 |
| clone_occurrences | 12846 | 1.35 MB | 0.96 | 0.82 | 0.65 | 0.31 | 0.34 | 0.63 | 0.26 |
| clone_body_payloads | 11802 | 6.86 MB | 0.56 | 0.45 | 0.56 | 0.27 | 0.29 | 0.54 | 0.22 |
| row_dictionary | 72388 | 3.98 MB | 3.31 | 2.81 | 1.22 | 0.96 | 0.26 | 0.98 | 0.71 |
| exact_posting_runs | 27159 | 0.81 MB | 1.23 | 0.96 | 0.65 | 0.40 | 0.25 | 0.29 | 0.19 |
| all statements | | | | | 43.29 | 25.70 | 17.59 | 31.45 | 17.26 |

What the table showed:

- Index maintenance dominates the native gap. `NoConflict` seeks and
  `IdxInsert` (most of whose cost is `balance_non_root`) were 13.2 G of the
  86.2 G native writer in the PERF4 profile. The three page-ordered staging
  tables carried a composite primary key only for uniqueness: finalization
  reads them through a full sort, so the key's index bought nothing.
- Every statement execution costs about 4 to 5 K instructions on native
  before it inserts a row (open, admit, reset). The finalization merges and
  the canonical layout copy ran one `INSERT` per row.
- The fingerprint section walk resumed each of its 30 wakes with a row-value
  comparison, which the native engine evaluates as a filter over a full
  index scan: 34.2 M VM steps on native against 1.9 M on SQLite.

Changes, each with the logical artifact digest unchanged
(`sha256:a67fdd3e…`) and the same sealed file size per engine:

1. The staging tables `term_posting_runs`, `exact_posting_runs` and
   `row_dictionary_pages` are plain append tables. A repeated run fails its
   merge as overlapping postings; a page is unique through the
   `source_pages` key, and a page's dictionary entries through its map.
2. The term, exact, n-gram and fingerprint merges, the dictionary staging
   and the canonical layout copy insert 32 rows per statement, in the same
   order as before.
3. The fingerprint section resumes inside its family with an equality prefix
   and a range, and crosses to the next family with the row-value query
   only when the family ends.

Writer instructions:u, tag `perf5` in `native-perf.jsonl` (five interleaved
runs; every run of a binary gave the same count to 0.1 G):

| build | writer instructions:u | ingest median | ingest min |
|---|---|---|---|
| SQLite, base | 61.0 G | 15535 ms | 13024 ms |
| native, base | 86.2 G | 17392 ms | 16463 ms |
| SQLite, PERF5 | 57.4 G | 14658 ms | 11439 ms |
| native, PERF5 | 75.4 G | 14708 ms | 10997 ms |

Native is 1.24x the SQLite base and 1.31x SQLite with the same changes. The
remaining native excess is in index inserts on the sealed serving tables:
`clone_fingerprint_postings` (text-led four-column key), `term_postings`,
`ngram_postings` and `row_chunks`, each written twice (finalization and
layout copy).

Serving p50 per query class (µs), medians, tags `perf5-serving` and
`perf5-serving-native`. The sealed artifacts keep their logical digest
(`sha256:5d6d4ac3…` in the search bench) and file size, and no reader code
changed.

| build | runs | exact_identifier | fuzzy_typo | high_cardinality | path | phrase | short_token |
|---|---|---|---|---|---|---|---|
| SQLite, base | 3 | 4405 | 5086 | 66548 | 4991 | 118968 | 33848 |
| SQLite, PERF5 | 3 | 4356 | 5126 | 50088 | 5258 | 103061 | 27001 |
| native, base | 7 | 3795 | 4582 | 53300 | 4204 | 105514 | 29770 |
| native, PERF5 | 7 | 4174 | 5075 | 53752 | 4816 | 103741 | 29061 |

Native exact_identifier, fuzzy_typo and path read 10 to 15 % slower at the
median. The PERF5 native runs started at a higher host load (median load1 82
against 69), and the cause is not settled: whether the native layout copy
splits pages differently with 32-row statements has not been checked byte
for byte.
