# Code generation resident size design

Status: proposed. No sealed-format revision is required. The sealed
generation format stays at `SEALED_GENERATION_FORMAT_REVISION_V1` 21.

## The cost is in the decoded form, not the sealed bytes

A cold build of this repository (6.1k files, 108 MB of source, 219k symbols,
458,883 chunks) holds a decoded `CodeIndexPublishedGenerationV1` whose
`retained_bytes` read 3.24 GB of a 4.11 GB process footprint. A 25.7k-file
repository read 4.32 GB of 5.49 GB. Each value is a lower bound because
`resident_bytes.rs` counts string and vector lengths, not allocator headers.

| Part | Bytes |
| --- | ---: |
| Chunk records: text, exact terms, subtokens, and per-chunk identity and revision strings | 1.15 GB |
| Per-file records: clone bodies, schema evidence, and identities | 1.08 GB |
| Projection request and receipts: every chunk listed as added, with digests | 0.49 GB |
| Symbol records | 0.18 GB |
| Edges (684k) | 0.16 GB |
| Unresolved calls | 0.11 GB |
| Imports | 0.04 GB |

The three largest rows look like candidates for sealed codec changes.
They are not. The sealed codec already writes each one compactly, and the
decoder expands each one back to its full domain shape:

- **Per-chunk revisions and identities.** A file segment
  (`PersistedFileChunksRefV2` in `sealed_codec.rs`) writes the generation
  and file occurrence once per file and replaces both with markers
  (`$tracedecay:g`, `$tracedecay:f`) before compression. It hoists the
  language descriptor, chunker, sanitizer, and sensitivity revisions into
  `chunk_defaults`. It omits a chunk id that the chunk-id recipe derives,
  and it omits a content digest that the stored text determines. It stores
  the source text the chunks cover once per file, and rows keep only spans.
  In memory, every `CodeSearchChunkV1` owns its own `String` for six of
  these values: `anchor.generation_id`, `anchor.file_occurrence_id`,
  `language_descriptor_revision`, `chunker_revision`, `sanitizer_revision`,
  and `sensitivity.policy_revision`. The domain newtypes from
  `validated_string_newtype!` wrap `String`, so a clone is a new heap
  allocation.
- **Projection request and receipts.** `projection_rows.rs` writes the
  request's added chunks as `(file key, first position, count)` runs. It
  writes only the receipt decisions that differ from what the request
  implies. In memory, the request's `added_or_changed` holds one
  `ChangedCodeChunkV1` per chunk. Each row repeats the chunk id and content
  digest that the chunk record already owns. Receipt rows are no longer held.
  `ProjectionPublicationHandoffV1` replays them from the request one row at a
  time, so the remaining projection cost is the request rows.
- **Per-file records.** `clone_rows.rs` interns each file's syntax kinds and
  token texts and writes token streams as integer codes. It omits payload
  digests and the occurrence fields that the file authority fixes. In
  memory, each `CodeIndexCloneBodyV1` holds expanded token streams, its
  digests, and a `CloneBodyOccurrenceV1` that repeats project, repository,
  generation, and path. Each file page also keeps `document.chunk_ids`,
  which repeats the id of every chunk the page already holds.

Shrinking the sealed bytes would not move the 3.24 GB. The work is in the
decoded representation.

## Cold construction no longer holds the decoded generation

The table above describes builds that kept the whole decoded generation
until publication. Cold construction now writes file segments to the store
preparation as each extraction window completes. It charges what it retains
to `ProcessResidentMemoryV1` under the `code_index.construction.v1`
component, through `ResidentMemoryOperationV1::reserve_process_shared`. A
refused build parks until its witnessed reconstruction demand fits, so a
concurrent second cold pass waits instead of stacking past the limit.
Serving uses the published generation's decode cache and recorded admission
demand.

The same isolated cold pass on the release daemon now measures as follows.
Each value is one run. `footprint` reports the physical footprint peak, and
`tracedecay status --json` reports `memory.resident_bytes` and
`memory.reserved_bytes`.

| Repository | Physical footprint peak | Peak occupancy | Peak construction charge |
| --- | ---: | ---: | ---: |
| This repository (6.1k files) | 1.58 GB | 1.58 GB | 0.92 GB |
| `t3code` (29k files) | 2.45 GB | 2.53 GB | 0.96 GB |

During construction, occupancy stays below the reservations. The peak in
both runs comes after publication, while the interactive graph catalog
warms. `PendingInteractiveCatalogWarmV1::run` takes no reservation, so
occupancy reached 2.53 GB on `t3code` while 268 MB was reserved. The catalog
then stays resident as the 833 MB `graph_catalog` owner. That warm is a
separate admission gap and is outside this design.

A decoded generation retains its full `retained_bytes`. When a decode peak
measurement exists, admission reserves the larger of
`decode_peak_growth_bytes` and `retained_bytes` under
`code-index-generation-decode-v1`. Without a peak measurement, admission uses
the retained bytes. A first active-generation decode with no recorded demand
currently takes no reservation. The `decoded_generation` owner reports
retained residency while the decode is held. The proposals below target both
retained residency and transient decode demand. They do not change these
admission rules or close the first-unmeasured-decode gap.

## Proposed decoded encoding

Apply the changes in this order. Measure after each one, and stop when the
remaining size no longer justifies the next change.

1. **Share per-generation identity and revision strings.** Change the
   identity and revision newtypes that a generation repeats per record to
   wrap `Arc<str>`. These are `CodeGenerationId`, `FileOccurrenceId`,
   `LanguageDescriptorRevision`, `ChunkerRevision`, `SanitizerRevision`, and
   `PolicyRevisionId`. The decoder and the builder intern each distinct value
   once per generation and clone the `Arc` into every record. Each newtype
   serializes through `as_str()`, so wire bytes and canonical digests do not
   change. Explicit string serialization requires no additional serde feature
   changes. The per-chunk cost of the six values drops from six
   heap allocations to six reference-count increments. Hoisting the fields
   out of `CodeSearchChunkV1` would save the 16-byte pointers too, but it
   changes a domain contract that every chunk consumer reads. Do that only if
   measurement after interning shows the pointers matter.
2. **Keep the projection request in its sealed shape.** Hold the request's
   added chunks in memory as the same `(file key, first position, count)`
   runs that `projection_rows.rs` writes. Keep explicit rows only for changed
   and deleted chunks. Resolve each run against the file's chunk roster when
   the projector, the digest, or receipt replay iterates the rows. Compute
   the request digest by serializing the resolved rows in the same order.
   `ProjectionPublicationHandoffV1::receipt_rows` already serializes receipt
   rows this way without collecting them, so the digest stays
   byte-identical. A cold build adds every chunk, so its
   request shrinks to one run per file.
3. **Derive the document chunk roster.** Drop `document.chunk_ids` from the
   decoded file page and derive the roster from the page's chunk records in
   their existing file-row order. The document roster must match those rows
   positionally. The chunker orders rows by source span and grain, with chunk
   ID only as a tie-breaker. The separate projection roster uses chunk-id
   order. The sealed segment already omits the document roster.
4. **Keep clone token streams interned.** Hold each file's interned string
   table and integer token codes in memory, as `clone_rows.rs` writes them.
   Rebuild the occurrence from the file authority when a caller asks for it.
   The readers of `conservative_tokens` and `rename_tokens` in `clones.rs`
   take a `CloneTokenStreamV1`, so they must read codes through the file's
   table. It is the largest change of the four. Measure the share
   of the 1.08 GB that clone bodies hold before you start it.

The issue measurement does not split the 1.15 GB and 1.08 GB rows by
field. Before any change lands, measure the split with the method in
[Measure the win](#measure-the-win). Then order the work by the bytes each
change removes.

## Keep shipped generations readable

Beta builds have shipped, so live profiles hold sealed generations at
revision 21. All four changes alter only the decoded representation. Each
change must keep the sealed bytes identical for the same input. Then a
shipped generation decodes into the new representation, and a new build
writes the same segment and manifest digests as an old build. Segment reuse
across the upgrade keeps working, so the format revision stays at 21 and
there is no migration.

Each change must include a test that seals one fixture and asserts the
segment and manifest digests equal those of the unchanged codec. The test
fails if the in-memory change leaks into the wire form.

If a later change must alter the sealed bytes, bump
`SEALED_GENERATION_FORMAT_REVISION_V1` and rebuild from source. Do not write
a migration. The restore path already refuses an older revision with
`SupersededSealedGenerationRevision`, and the scheduler then rebuilds the
generation from the worktree. A sealed generation is a cache that the source
tree derives. A migration would need a reader for every shipped revision,
and `sealed_codec.rs` deliberately keeps no reader for older revisions. The
cost of a rebuild is one cold pass per worktree. Construction admission
bounds that pass, so concurrent rebuilds after an upgrade wait instead of
exceeding the limit.

## Measure the win

Use the task-supplied checkout and the release daemon in an isolated temporary
profile. Do not create a clone or another project-data copy. The earlier
figures are historical measurements, not instructions to preserve that setup.

1. Build the release CLI with
   `bazel build --config=release //crates/tracedecay-cli:tracedecay`.
2. Start the daemon with `TRACEDECAY_DATA_DIR` and `HOME` inside the temporary
   profile, then run `tracedecay init <checkout>`.
3. While the cold pass runs, sample `tracedecay status --json <checkout>`.
   Record `memory.resident_bytes`, `memory.reserved_bytes`, and
   `memory.reservations`.
4. Run a query that decodes the generation, then record the
   `decoded_generation` bytes in `memory.owners`. That value is the
   generation's `retained_bytes`. Record the measured decode peak separately
   and compare admission against the larger demand.
5. Record the on-disk size of the project's `code-index-v1/` store. The size
   must not change. A change in size means a decoded change reached the
   sealed bytes.

Stop and join the isolated daemon before removing its temporary profile.
Do not read or reset the operator's active profile.

For the per-part split, sum the `resident_bytes.rs` helpers per part:
`chunk_bytes` with each identity and revision string summed separately,
`file_bytes` with clone bodies, schema evidence, and `document.chunk_ids`
summed separately, and the projection request rows. Emit the sums as fields
of one `tracing` event at seal time. A trace event is the operator log
surface, so the event can stay after the work lands.

Report the served generation's bytes and the peak `resident_bytes` before
and after each change, with the median of three cold passes per side.
