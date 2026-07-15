# TraceDecay V2 Code Intelligence Indexing Plan

## Status / role

Planned for PR9. This plan delivers the code-indexing product boundary after sanitized capture and durable storage exist. Start as a focused module; extract `tracedecay-code-index` only when independent reuse, dependency isolation, or compile-time savings justify a crate boundary.
PR9/PR10 record incremental, no-op, generation, and resource baselines for
[PR20](33-end-to-end-performance-optimization.md).
Generation-bound diagnostics compose with the daemon gateway defined by
[Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).

## Outcome

TraceDecay builds deterministic, immutable code-intelligence generations from sanitized repository snapshots. Incremental builds reuse unchanged work, preserve symbol lineage, and attach diagnostics and tests to the exact source generation they describe.

## Owns

- Versioned tree-sitter grammar registration and deterministic language extraction.
- One versioned language descriptor per language, shared by extraction,
  structural search, outline, rewrite, analyzer routing, and host LSP
  projection.
- Canonical symbol, occurrence, relationship, diagnostic, and test-attribution records.
- Content-addressed incremental reuse and bounded sanitized dirty-worktree
  indexing overlays captured from repository state. Unsaved per-client LSP
  document overlays are separate Plan 35 daemon session state.
- Logical generation planning, sealing, digests, and lineage evidence.
- Read-only conversion of V1 graph records into the V2 logical model.

## Does not own

- Filesystem watching, repository reads, snapshot coalescing, or redaction; capture owns those.
- Database connections, generation files, transactions, manifests, pointers, or publication; store owns those.
- Projector scheduling, retries, or checkpoints.
- Query ranking, semantic embedding inference, UI, or public transport bindings.
- Analyzer executable commands or settings, which remain configuration-owned
  by Plan 20.
- A host-facing analyzer broker; the Plan 35 daemon gateway is the sole broker
  presented to LSP hosts.
- A second repository identity, intake queue, or write path.

## Required behavior

### Sanitized intake

- Accept only receipt-bound sanitized snapshots carrying repository, checkout, worktree, ref, source revision, sanitizer revision, and content identity.
- Reject missing, stale, mixed-snapshot, or unsanitized input before parsing.
- Treat deletions, renames, ignored files, binary files, generated files, and unsupported languages explicitly.

### Deterministic extraction

- Select grammar, aliases, extensions, expando behavior, and extractor revision
  through one versioned registry. Duplicate language tables and parser
  acquisition paths are forbidden.
- The same canonical descriptor supplies extension, language-ID, root-marker,
  and capability facts for analyzer routing and host LSP projection. It does
  not absorb configuration-owned executable commands or settings.
- Acquire one Tree-sitter parser from that descriptor. Extraction and the
  in-process `ast-grep-core` structural-match/outline/rewrite kernel share its
  pinned grammar and source generation; no host `ast-grep` binary is authority.
- Produce stable canonical rows and digests for identical input, registry, and extractor revisions on every supported host.
- Preserve parse errors and unsupported constructs as evidence; never invent successful structure.
- Keep language-specific logic behind a small extractor interface while sharing identity, lineage, and output contracts.
- Keep parser and grammar dependencies behind the code-intelligence ownership
  boundary so unrelated domain, store, application, and adapter checks do not
  compile them. Feature groupings reflect shipped language capability, not a
  convenience meta-feature that silently expands unrelated builds.
- Record same-host clean, warm incremental, and no-op check/test compilation
  for the core registry and representative grammar groups. If an extractor-only
  change repeatedly rebuilds unrelated grammar bindings, use that evidence to
  refine module, feature, or crate boundaries without weakening default product
  capability.
- Structural results report deterministic file/span order, parse coverage,
  unsupported regions, and bounded errors. Pagination cursors bind query,
  descriptor, generation, and ordering; cancellation cannot publish partial
  extraction or mutation state.

### Generations and incremental reuse

- Build one immutable logical generation from one fenced snapshot.
- Reuse file and symbol results only when content, grammar, extractor, identity, and sanitizer inputs match.
- Force a full rebuild for incompatible schema, grammar, identity, or privacy changes and for quarantined corruption.
- Seal the generation before handing rows and the expected digest to the store publication port.
- Never mutate a published generation or substitute the active checkout for the selected snapshot.

### Identity and lineage

- Derive stable symbol identities from repository identity, language, qualified structure, and source evidence.
- Record rename, move, split, and merge candidates with evidence and confidence.
- Keep ambiguous lineage explicit; do not silently merge unrelated symbols.

### Diagnostics and tests

- Attach compiler and language-server diagnostics to exact file and symbol
  occurrences only within the matching sanitized clean generation and content
  digest.
- Retain producer kind and identity, analyzer and configuration revisions,
  evidence class, freshness, and clearing or supersession provenance.
- Keep clean-generation persistence separate from unsaved LSP overlays.
  Overlays remain ephemeral daemon session state and become durable only after
  saved content passes the normal capture and generation pipeline with the same
  digest.
- Stale, cleared, historical, or cross-snapshot diagnostics remain evidence but
  cannot publish as current. Plan 35's daemon gateway is the only host-facing
  analyzer broker and cannot create a parallel diagnostic authority.
- Map test definitions and runs to the generation, source revision, and candidate production symbols they cover.
- Distinguish direct evidence, inferred candidates, stale evidence, and unknown attribution.

### V1 migration

- Consume logical batches emitted by the store-owned, read-only V1 importer through the sanitizer boundary.
- Preserve source generation and migration provenance, rebuild deterministic V2 identities, and verify counts and digests before publication.
- Never open a V1 database from the indexer.

## Acceptance

- Identical sanitized fixtures produce byte-identical logical rows and generation digests across repeated and supported-host runs.
- One-file edits re-extract only affected files and dependents; unchanged results are reused without changing their identities.
- Rename, move, split, merge, ambiguous-lineage, parse-error, deletion, and unsupported-language fixtures remain truthful.
- Diagnostic and test attribution never crosses snapshots, never upgrades
  inference to fact, and never publishes stale or cleared evidence as current.
- Canonical descriptor fixtures prove analyzer routing and host LSP projection
  use the same extension, language-ID, root-marker, and capability facts without
  copying executable commands or settings into this boundary.
- Dirty-overlay fixtures create no durable generation rows, while matching
  saved content preserves producer provenance through capture and publication.
- Crash, cancellation, disk-full, stale-snapshot, and concurrent-build tests publish either one complete generation or none.
- V1 fixtures migrate through logical batches with no indexer database open and no lost or duplicate supported records.
- Direct behavior tests prove capture is the only intake and store/projector composition is the only publication path.
- Focused non-indexing package checks do not compile Tree-sitter grammars or
  structural-search implementation, and PR9 publishes the compilation baselines
  required for PR20 comparison.
