# Secret Detection, Redaction, and Private Data Safety

## Status / Role

- Cross-cutting V2 safety requirement; its sanitized-capture foundation is complete.
- Mandatory for every later ingestion, storage, indexing, retrieval, logging, and export path.
- Delivered structural sanitization remains required product behavior.
  Sensitive-value redaction of lossless LCM raw payloads is conditionally
  delivered through the owner setting described below.

Historical detector-corpus names, sink inventories, remediation packets, and
intermediate gate layouts are evidence, not mechanisms that later work must
recreate. Persisted safety markers and published safety states retain their
compatibility and migration obligations; acceptance otherwise follows the
direct prevention, remediation, disclosure, and regression behavior below.

## Outcome

TraceDecay does not persist or disclose known secrets and private values
through sanitized observations and derived data. Structured content is parsed
before scanning, safety state follows data through the system, and each
covered durable or external sink enforces the same policy.

**Conditional LCM raw-payload guarantee (2026-07-26).** Sensitive-value
redaction of new `lcm_raw_messages` input is enforced when
`UserConfig::lcm_sensitive_redaction_enabled` is set. The profile default
flows through `IngestProtectionDefaults::from_profile()` into `ingest_config`
and `redact_sensitive_text`; a per-message metadata key can override that
profile default in either direction. The default remains `false` because
redaction is irreversible and the LCM contract is lossless by default.
Enabling it protects newly ingested values; it does not rewrite transcripts
already at rest. Structural payload externalization remains independent of
this sensitive-value setting.

**Dated amendment (2026-08-07, recorded decision — supersedes the
conditional guarantee above).** The owner setting described above no longer
exists in code. `d69ffa3504 fix(privacy): hard cut durable content
boundaries` deleted `IngestProtectionDefaults` and
`UserConfig::lcm_sensitive_redaction_enabled`
(`crates/tracedecay-sessions/src/runtime/lcm/raw.rs`,
`ingest_protection_defaults_tests.rs` removed); sensitive-value redaction of
`lcm_raw_messages` input is now mandatory and unconditional at ingest — there
is no per-profile opt-in and no per-message metadata override. Ingest marks
affected payloads `redacted: true, lossy: true` in metadata (`raw.rs:779-786`
at the time of this note). This is a hard cut, stricter (safer) than the
plan's prior "lossless LCM by default, irreversible redaction is opt-in"
contract, and is consistent with the index's "capture sanitizes before
persistence" doctrine, which outranks this plan's text. The former
`UserConfig::lcm_sensitive_redaction_enabled` toggle referenced in
`docs/USER-GUIDE.md` and `crates/tracedecay-sessions/SEAMS.md` is stale and
should be treated as retired along with this note. Post-RC register items
untouched by this amendment: at-rest rescan/quarantine/remediation UI on
detector upgrade, and detector evaluation corpora remain absent (see the
2026-08-07 plan-conformance audit, section 4D).

## Owns

- Structured parsing and secret/private-data detection.
- Redaction, taint metadata, and verified-safe markers.
- Sink firewalls for storage, indexes, facts, sanitized session projections,
  analytics, logs, APIs, UI, and exports. LCM raw sensitive-value protection
  is conditional on the named owner setting above.
- Safe audit records and incident evidence.
- Existing-data scanning, quarantine, remediation, and derivative rebuilds.
- Read-only Doctor diagnostics and evidence.
- Operator UI for safety state, incidents, and remediation progress.

## Does not own

- Credential storage or configuration resolution; Plan 20 supplies opaque credential references.
- Provider-specific business logic unrelated to identifying sensitive values.
- A speculative threat-model registry, compliance framework, or policy-document bureaucracy.
- Generated inventories, plan parsers, trackers, executors, or workflow JavaScript.
- A claim that heuristic detection can identify every possible secret.

## Required behavior

1. Parse before scan
   - JSON, YAML, TOML, dotenv, URLs, headers, and known transcript/event envelopes are parsed first.
   - Detectors inspect field meaning and decoded values as well as bounded raw text.
   - Malformed structured input is treated as untrusted raw input, never implicitly safe.

2. Propagate safety state
   - Untrusted values enter as tainted.
   - Unsaved LSP documents are tainted ephemeral session data. They may be
     disclosed only to explicitly authorized analyzers; their content is never
     persisted, logged, embedded, exported, or captured as a TraceDecay
     observation.
   - Remote analyzers are denied by default and require an explicit policy
     capability and privacy disclosure, as specified by
     [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
   - Redaction creates a safe representation without erasing the source's tainted provenance.
   - A verified-safe marker identifies the policy version and transformation that produced it.
   - Concatenation, formatting, summarization, and extraction preserve taint unless re-sanitized.

3. Enforce sink firewalls
   - Every covered durable or externally visible sink accepts only
     verified-safe payloads; LCM raw sensitive-value protection is conditional
     on the owner setting above.
   - Diagnostic messages and provenance pass the same sink firewall without
     retaining raw analyzer stderr, environment values, command lines, or source.
   - Missing, stale, or incompatible safety metadata fails closed with a structured error.
   - Derived indexes and caches cannot retain unsafe source text after remediation.

4. Detect realistically
   - Combine exact credential formats, entropy and context signals, configured private patterns,
     structured sensitive keys, and known-value fingerprints.
   - Bound scanning cost and payload size without silently accepting an unscanned remainder.
   - Findings include detector origin/revision, location, remediation class,
     evidence anchors, scanned coverage, and an optional typed assessment:
     `ordinal_rank`, `heuristic_score`, `calibrated_probability`, or
     `calibrated_interval`. Rank names its comparison set and deterministic
     components; a heuristic names its versioned scale and never renders as a
     probability. Probability or interval output requires a valid held-out
     calibration profile naming detector cohort, horizon, support, error, and
     drift validity. No finding or assessment contains the secret value.

5. Audit safely
   - Record policy version, source class, detector, action, timestamps, and opaque record identifiers.
   - Logs, metrics, traces, errors, and diagnostic bundles contain redacted evidence only.

6. Remediate existing data
   - Scan legacy records and their derivatives.
   - Quarantine unsafe records before they can be served.
   - Redact, delete, or replace sources according to policy, then rebuild affected derivatives.
   - Maintain a deletion/quarantine/correction overlay whose lineage is applied
     before migrated, restored, cached, indexed, or derived data can serve.
     Restore and archive recovery replay every newer disposition and rebuild
     affected derivatives; provenance never overrides erasure.
   - Preserve opaque source and derivative identity, transformation/privacy
     revisions, receipts, corrections, tombstones, quarantine, and derivative
     ownership. Do not retain raw sensitive payload merely to make a migration
     reversible.
   - Resume safely after interruption by consuming
     [Plan 12](12-root-compatibility-migration.md)'s
     destination-committed checkpoints bound to the privacy revision, and
     report bounded progress. A missing or incompatible overlay/checkpoint
     fails closed.

7. Expose operational state
   - Doctor detects disabled protection, stale policy markers, unsafe legacy rows, failed remediation,
     and derivatives that need rebuilding.
   - Safe automatic repairs run through normal daemon operations; destructive choices stay explicit.
   - UI shows coverage, findings by class, quarantine state, remediation progress, and failures.

## Acceptance

- Sanitized capture established shared parsing, detection, redaction, receipt, and safe-marker primitives.
- Representative structured and malformed inputs prove parse-before-scan behavior.
- Every covered sink rejects raw, tainted, unmarked, and stale-policy payloads;
  LCM raw sensitive values follow the conditional guarantee above (see the
  2026-08-07 dated amendment above — this is now a mandatory hard cut, not a
  conditional guarantee; the "conditional" and "profile setting" language in
  this bullet and the next describes the retired mechanism).
- End-to-end tests prove secrets do not appear in covered databases, indexes,
  facts, sanitized session projections, logs, analytics, API responses, UI
  payloads, exports, or diagnostic bundles. LCM raw tests prove the profile
  setting enables redaction without message metadata, message metadata
  overrides the profile in both directions, the default remains off, and
  enablement affects new ingestion without claiming an at-rest rewrite.
- LSP tests prove unsaved document content remains session-ephemeral, reaches
  only authorized analyzers, and cannot reach remote analyzers without the
  required capability and disclosure.
- Remediation tests quarantine unsafe legacy data and rebuild clean derivatives after repair.
- Migration, backup, and restore fixtures prove newer deletion, quarantine,
  correction, and policy state is replayed before serving and that raw
  sensitive payload is not retained for reversibility.
- Direct detector-contract tests reject findings with a numeric assessment but
  no origin, score kind, scale/calibration revision, evidence anchors, or
  scanned coverage. Checked-in positive/negative evaluation corpora report
  precision, recall, false-positive/false-negative counts, and coverage by
  detector/source cohort; held-out calibration tests report probability and
  interval error/support and force stale, shifted, or under-supported
  calibration to heuristic output or abstention without weakening the sink
  firewall.
- Doctor and UI expose actionable state without reproducing sensitive values.
- Performance limits fail visibly and safely instead of skipping protection.
