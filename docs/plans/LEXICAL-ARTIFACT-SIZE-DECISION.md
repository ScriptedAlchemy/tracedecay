# Lexical artifact size decision

Status: decided. Revision 25 replaces every earlier revision. Older
artifacts are refused as incompatible and rebuilt from the sealed
generation; there is no migration.

## Layout

- Every posting family is one delta-varint list per serving key, clustered by
  that key, with no secondary index: `term_postings` per `(term, field)` with
  frequencies and the document frequency, `exact_postings` per
  `(exact term, field)`, `ngram_postings` per `(kind, ngram)`. Dense n-gram
  lists are stored as a bitset over the range they span when that is smaller.
- Batches append page-ordered term and exact staging runs so appends stay at
  the tree tail; finalization merges each in one sorted pass and drops it.
  N-gram lists are never staged: finalization rebuilds them from the stored
  rows in key order, spilling to further passes only past a quarter of the
  builder's memory budget. Freed pages are released
  (`auto_vacuum = INCREMENTAL`).
- Annotation-use symbols (attributes such as `#[inline]`) mint no lexical
  document; their text stays searchable through the item they annotate and
  they remain graph symbols. Documents keep their source chunk ordinals.
- Row chunk text is raw deflate when that is smaller.
- `term_stats` and `ngram_statistics` are gone; the lists carry their
  document frequencies. `document_integrity` and `import_integrity` are gone;
  their digests are pure functions of stored rows and imports, and the
  per-page base-section receipts still attest them. `import_evidence` keeps
  only its canonical key, which is the evidence.
- `vocabulary` is keyed by term alone; term-id collisions are refused once at
  finalization. Rows store chunker-minted chunk ids as 32 digest bytes;
  symbol dictionary entries store canonical symbol ids as digest bytes and
  qualified names relative to the row's file path.
- Candidate sets are Roaring bitmaps (at most one bit per document); per-row
  term frequencies come from one ascending walk over the request's lists.

## Measurement

Cold index of `/fast/projects/tracedecay` (6,468 tracked files, ~140 MB) in an
isolated profile with the release daemon, September 23, 2026. The worktree
moved slightly between runs (409,398 → 407,739 chunk documents).

| Section | Revision 14 | Revision 17 |
| --- | ---: | ---: |
| `ngram_postings` (+ index, statistics) | 1,470,275,584 | 241,299,456 |
| `term_postings` (+ index, `term_stats`) | 1,220,493,312 | 71,905,280 |
| `rows` + `rows_by_chunk` | 291,549,184 | 247,988,224 |
| `row_dictionary` | 111,218,688 | 65,638,400 |
| `import_evidence` + `import_integrity` | 159,010,816 | 44,310,528 |
| `vocabulary` (+ unique index) | 78,381,056 | 38,686,720 |
| `exact_postings` (+ index) | 48,025,600 | 5,144,576 |
| `document_integrity` | 16,965,632 | 0 |
| everything else | 24,514,560 | 24,407,040 |
| **File** | **3,420,438,528** | **740,286,464** |

Per document the file fell from 8,355 to 1,816 bytes (−78%). The staging peak
during the build fell from ~3.4 GB to ~1.9 GB.

Revision 20 on the same journey (407,774 source chunks, 352,172 documents):

| Section | Revision 17 | Revision 20 |
| --- | ---: | ---: |
| `ngram_postings` | 241,299,456 | 233,578,496 |
| `rows` + `rows_by_chunk` | 247,988,224 | 140,324,864 |
| `term_postings` | 71,905,280 | 68,993,024 |
| `row_dictionary` | 65,638,400 | 59,482,112 |
| everything else | 112,549,888 | 111,857,664 |
| **File** | **740,286,464** | **614,989,824** |

Staging never exceeds the sealed size (peak 615 MB, down from ~1.9 GB), and
finalization takes ~70 s instead of spending ~2 min relocating freed pages.
The file is 18% of the revision-14 artifact.

Revision 23 on the same journey (348,769 documents; the worktree moved):

| Section | Revision 20 | Revision 23 |
| --- | ---: | ---: |
| `ngram_postings` | 233,578,496 | 231,690,240 |
| `rows` + `rows_by_chunk` → `row_blocks` + `row_chunks` | 140,324,864 | 84,905,984 |
| `term_postings` + `vocabulary` → `term_postings` | 107,196,416 | 78,487,552 |
| `row_dictionary` | 59,482,112 | 58,863,616 |
| everything else (lexical) | 74,407,936 | 73,986,048 |
| clone index (payloads, occurrences, exact, fingerprints) | not built | 270,721,024 |
| **File** | **614,989,824** | **798,654,464** |

- Rows are stored in deflated blocks of up to 32 consecutive documents (or
  64 KiB); a read inflates one block, and ascending visits reuse it. A
  signature chunk stores its text as the prefix length it shares with its
  body chunk in the same block (154,703 rows).
- `term_postings` is keyed by term text and carries every field's list and
  the fuzzy flag; the separate `vocabulary` tree and term ids are gone.
- The clone index seals with the lexical rows in one build, so there is no
  clone successor. Its payloads and occurrences are deflated canonical JSON
  in rowid tables (a WITHOUT ROWID row over ~1 KB spills to a mostly empty
  overflow page), and fingerprint postings are one delta-coded list of
  `(occurrence ordinal, position)` per fingerprint with its count. In the old
  layout the same clone rows took ~3.8 GB (payloads 2.35 GB, fingerprint rows
  1.1 GB, occurrences 311 MB) and their verification ran for over 20 minutes.
- Cold index to sealed artifact: 262 s with the clone index (215 s for
  revision 20 without it); finalization ~77 s.

Revision 24 makes the file a function of its content and shares it across
linked worktrees:

- Nothing route-specific is sealed. `artifact_state.metadata` holds only
  logical paths and retriever revisions; generation, repository, freshness,
  and each clone occurrence's project, worktree, generation, and snapshot
  come from the opener. The receipt drops the sealed source's state and
  chunk-chain digests (both hash the building generation into every chunk
  anchor), and `source_pages` keeps only content columns: the per-page
  cursors sit in `source_page_cursors`, dropped before the seal, and the
  finalization state table is dropped at the seal.
- Physical layout follows batch arrival order, so finalization `VACUUM`s the
  file before sealing and normalizes SQLite's commit counters; the same
  content staged through different batch sizes seals the same bytes.
- Completed artifacts live in the project's `code-text-artifacts-v1/` beside
  the shared generation segments; staging stays per scope in
  `code-text-artifact-staging-v1/`. Retention marks every scope's
  descriptors (including quarantined scopes) before collecting, publication
  holds the project lock shared from finding or placing the file until its
  descriptor is durable, and a scope's old `code-text-artifacts-v1/` is
  removed whole.
- Clone payloads are a deflated binary record without digests (re-derived
  and checked against the row's content address on decode), with each
  syntax kind stored once per payload and the rename stream stored as its
  difference from the conservative stream. Occurrences store only their
  eligibility beside the content columns.

Two linked worktrees of HEAD `37d94657c0` (354,853 documents) in one
isolated profile, built one after the other; the second build sealed the
same bytes and publication kept the first file:

| | Revision 23 | Revision 24 |
| --- | ---: | ---: |
| text artifacts stored | 2 per-scope files, 798,654,464 each (~1.60 GB) | 1 shared file, 672,530,432 |
| `clone_body_payloads` | 124,735,488 | 56,737,792 |
| `clone_occurrences` | 60,919,808 | 18,178,048 |
| clone index total (with fingerprints, exact postings, unique indexes) | 270,721,024 | 152,420,352 |
| `source_pages` | 14,761,984 | 9,973,760 |

The revision-23 row is the single-worktree measurement above (the tree
differs slightly); per-scope stamps made each worktree's file distinct. The
seal-time `VACUUM` also compacts the file (692.9 MB staged, 672.5 MB
sealed); it takes ~15 s of the 255 s cold build and briefly holds a
rollback journal and a temporary copy (~1.3 GB) beside the staging file.

Revision 25 interns the clone index's identities and skips redundant
builds:

- `clone_body_payloads` is keyed by an integer ordinal with its digest as 32
  bytes; `clone_occurrences` stores its symbol id as 32 digest bytes and
  names its payload by ordinal; `clone_exact_postings` is
  `(class, revision, digest bytes, occurrence ordinal)`. Exact pages page by
  occurrence ordinal.
- Before building, a worktree derives the artifact's content key from the
  sealed source (the format and every file segment's key, occurrence,
  content address, and symbol identities) and the projection's content
  metadata. A descriptor any scope published under that key is adopted once
  its file opens against its content address and this projection; a failed
  verification builds, and publication replaces a shared file that no longer
  hashes to its name.

Same journey, HEAD `37d94657c0`:

| | Revision 24 | Revision 25 |
| --- | ---: | ---: |
| `clone_body_payloads` (+ unique index) | 62,369,792 | 56,389,632 |
| `clone_occurrences` (+ unique index) | 25,071,616 | 11,730,944 |
| `clone_exact_postings` | 16,777,216 | 3,010,560 |
| clone index total | 152,420,352 | 119,332,864 |
| **File** | **672,530,432** | **639,401,984** |
| second worktree, same tree | builds (~255 s), publication dedupes | adopts after its generation seals, no text build |

## Remaining levers

- `ngram_postings` (234 MB) is now over a third of the file.
- `clone_fingerprint_postings` (48 MB) and `clone_body_payloads` (54 MB)
  are now the bulk of the clone index.
