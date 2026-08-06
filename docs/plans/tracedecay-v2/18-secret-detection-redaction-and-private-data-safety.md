# Secret detection, redaction, and private-data safety

## Outcome

TraceDecay does not persist or disclose known secrets or prohibited private
values through observations, derived indexes, facts, sessions, logs,
analytics, APIs, UI, exports, provider requests, or diagnostic bundles.
Coverage and uncertainty remain explicit: heuristic detection never claims it
can identify every secret.

Final V2 starts with fresh stores. This plan does not scan, migrate, rewrite,
backfill, quarantine, or rebuild an older TraceDecay database. Historical data
acquired from supported agent hosts enters through the same current capture
firewall as new data.

## Authority

- Structured parsing and bounded secret/private-data detection run before a
  value reaches a durable or external sink.
- Safety state includes detector/policy revision, scanned coverage, source
  class, transformation, privacy domain, and safe/tainted/redacted status.
- Owning content authorities retain exact redaction and authorization state.
  Grafeo stores only authorized typed identifiers/relations/vectors and never
  becomes a raw-content bypass.
- Configuration supplies opaque credential references and explicit privacy
  policy. No client, log, metric, or graph property receives secret bytes.
- Doctor is read-only and reports coverage, stale policy, disabled protection,
  or unavailable evidence. It never repairs or rewrites data.

## Required behavior

1. **Parse before scan.** JSON, YAML, TOML, dotenv, URLs, headers, and known
   event/transcript envelopes are parsed before inspection. Malformed content
   is untrusted bounded text, never implicitly safe.
2. **Propagate safety state.** Concatenation, formatting, summarization,
   extraction, embedding, projection, and export preserve taint unless a
   canonical transformation produces a revisioned safe representation.
3. **Fail closed at sinks.** Durable/external sinks accept only an allowed
   representation with current policy evidence. Missing, incomplete, stale,
   oversized, cancelled, or incompatible scanning returns a typed problem.
4. **Protect ephemeral source.** Unsaved documents and provider-local frames
   may reach only an explicitly authorized analyzer/provider for the admitted
   operation. They are not persisted, logged, embedded, exported, or captured
   as ordinary TraceDecay observations.
5. **Detect realistically.** Exact credential formats, structured sensitive
   fields, configured private patterns, known-value fingerprints, entropy, and
   context signals are combined without recording matched secret bytes.
6. **Audit safely.** Receipts record policy/detector revision, source class,
   action, coverage, timestamps, and opaque identities. Logs, errors, metrics,
   traces, and UI payloads contain redacted evidence only.
7. **Respect lossless LCM.** Exact authorized source content remains
   recoverable, but visibility is mediated by the owning redaction/content
   authority. Sensitive-value transformation is explicit policy and cannot be
   silently enabled or disabled per message.
8. **Preserve deletion and denial.** Current tombstones, deletion, quarantine,
   retention, authorization, and policy state are applied before restored
   final-V2 content or rebuilt derivatives can serve.

## Product journeys

- Capture rejects or redacts prohibited values before committing the source
  occurrence and before graph/vector publication.
- Retrieval authorizes candidates before ranking and again before hydration;
  denied candidates expose no identity, count, timing, cursor, vector, or
  alternate source.
- Session/LCM source pagination hydrates through each message's owning
  redaction/content authority, including cross-project selection.
- Memory writes store only an allowed project-wide fact representation and
  provenance; Grafeo fact relations/vector references contain no raw secret.
- Diagnostics and provider execution pass bounded safe views rather than raw
  environment, command, prompt, or analyzer payloads.
- Dashboard/CLI/MCP/HTTP/SDK render the same typed redacted finding and coverage
  without a private bypass.

## Verification

- Representative structured, encoded, malformed, oversized, split-field, and
  contextual inputs prove parse-before-scan and bounded failure behavior.
- Every sink rejects raw, tainted, unmarked, stale-policy, and
  incomplete-coverage payloads.
- End-to-end tests prove secrets do not appear in SQLite, Grafeo
  properties/vectors, facts, sanitized session projections, logs, analytics,
  API/UI responses, exports, or diagnostics.
- LSP tests prove unsaved content is ephemeral and remote analyzer access is
  denied without explicit capability and disclosure.
- LCM tests page summary sources and prove authorized redaction/content
  hydration without leaking ranked metadata.
- Revocation between candidate selection and hydration returns no payload and
  invalidates cached/cursor state.
- Backup/restore and rebuild tests apply current deletion/quarantine/privacy
  state before activation.
- Detector evaluation reports precision, recall, false positives/negatives,
  and coverage per declared cohort. Probability output requires a valid
  held-out calibration profile; otherwise output is a named heuristic or
  abstention.
- Read-only Doctor completes without a writer lane and exposes no sensitive
  evidence.

## Rejected designs

- raw-value logging, metrics, graph properties, receipts, or errors;
- ambient environment/PATH/credential authority;
- direct client access to a store or redaction key;
- old-database scanners, migrations, overlays, compatibility readers, or
  reversible secret retention;
- Doctor repair/apply modes;
- silent partial scanning, fabricated safe state, or fixed test-count/source
  inventories as acceptance.
