# Lexical artifact size decision

Status: options only; no design decision is made here.

## Measurement

The September 13, 2026 cold run used the `c91b8e3c1d` hotpath binary, an
isolated profile, and `/fast/projects/tracedecay` as the source checkout. The
checkout contained 6,456 tracked files and 224,585,869 tracked bytes; code
index admission retained 5,295 files and 92,735,119 sanitized source bytes.
The finalized revision-14 lexical SQLite artifact was 2,741,256,192 bytes:
29.56 times the admitted source.

`dbstat` accounts for every 4 KiB page:

| Section | Bytes | Artifact | Source multiple |
| --- | ---: | ---: | ---: |
| `ngram_postings` | 859,471,872 | 31.35% | 9.27x |
| `ngram_postings_by_ngram` | 473,128,960 | 17.26% | 5.10x |
| `term_postings` | 375,259,136 | 13.69% | 4.05x |
| `term_postings_by_term` | 329,756,672 | 12.03% | 3.56x |
| `rows` | 258,244,608 | 9.42% | 2.78x |
| Import evidence and integrity | 162,938,880 | 5.94% | 1.76x |
| Row dictionary and row lookup index | 84,926,464 | 3.10% | 0.92x |
| Vocabulary and its unique index | 86,421,504 | 3.15% | 0.93x |
| Exact postings and lookup index | 49,246,208 | 1.80% | 0.53x |
| Statistics, receipts, integrity, metadata, and free page | 61,861,888 | 2.26% | 0.67x |

The two posting tables and their serving indexes hold 74.33% of the file.
The 32,039,149 n-gram rows contain 248,947,156 bytes of bitmap payload; the
remaining 610,524,716 bytes in that table are SQLite keys, records, and page
slack. The row table contains 190,591,951 payload bytes, and the row
dictionary contains 42,012,311 payload bytes.

## Duplication boundaries

The artifact is a derived serving projection, not source authority.

- `rows` re-encodes sanitized chunks already authenticated by the sealed code
  generation. The lexical copy is needed by current reads, but it is duplicate
  content rather than new evidence.
- `row_dictionary`, `document_integrity`, and `source_pages` repeat identities,
  digests, and page receipts derivable from the sealed generation.
- `import_evidence` and `import_integrity` repeat parser-attested import records
  retained by the code generation and represented in the graph projection.
- Term, exact, and n-gram postings are lexical-only derived data. Their base
  tables do not duplicate graph rows, but the three secondary serving indexes
  duplicate their keys and row locators inside this artifact.
- The graph's 1,637,478,400-byte `generation.grafeo` is separate. Removing
  lexical duplicates does not remove graph-store storage.

## Bounded options

### Keep revision 14

Cost on this corpus: 2,741,256,192 bytes (29.56x source). This preserves
resume-in-place construction and current query plans. It requires no format
cutover, but leaves storage proportional to SQLite row overhead as well as
posting payload.

### Separate build order from serving order

Build resumable staging tables in page order, then publish final posting tables
clustered by serving key so the term, n-gram, exact, row, and vocabulary
secondary indexes are unnecessary. Those indexes occupy 906,018,816 bytes.
The measured upper estimate is therefore 1,835,237,376 bytes (19.79x source)
before accounting for final-table rewrite scratch space. Publication would
temporarily need approximately one old artifact plus one 1.84 GB successor;
the final write must remain atomic and resumable.

### Keep only derived lexical data

Hydrate chunk rows and imports through the sealed-generation authority instead
of storing lexical copies, while retaining compact document locators and all
postings needed for bounded query latency. Removing the measured import tables
from the clustered estimate yields 1,672,298,496 bytes (18.03x source);
removing row copies needs a replacement locator format and cannot be estimated
as a simple subtraction because postings currently address row document IDs.

### Replace row-per-page n-grams with a compact immutable index

The n-gram table stores 248,947,156 bitmap bytes in 859,471,872 bytes of SQLite
pages, plus a 473,128,960-byte serving index. A bounded immutable layout could
store one ordered key directory and paged bitmap payload, with checksummed
blocks and a fixed query-read budget. The measured payload floor is 249 MB.
Adding current row, dictionary, and document-digest payloads gives a 495 MB
known floor before term/exact postings, directories, checksums, and alignment.
An 8x-source envelope would allow 742 MB total, leaving about 247 MB for those
unmeasured requirements. This option needs a prototype before that envelope
can be accepted or rejected.

## Decision still required

Choose only after measuring query p95/p99, cold build time, peak scratch and
resident memory, crash-resume behavior, and byte-stable rebuilds for a
prototype. This memo deliberately does not select an option or assign a new
format revision.
