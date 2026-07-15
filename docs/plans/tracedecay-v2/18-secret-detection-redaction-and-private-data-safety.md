# Secret Detection, Redaction, and Private Data Safety

## Status / Role

- Cross-cutting V2 safety requirement; its PR5 capture foundation is complete.
- Mandatory for every later ingestion, storage, indexing, retrieval, logging, and export path.
- Delivered as product behavior, remediation, Doctor checks, and UI state; none is deferred.

## Outcome

TraceDecay does not persist or disclose known secrets and private values through derived data.
Structured content is parsed before scanning, safety state follows data through the system, and every
durable or external sink enforces the same policy.

## Owns

- Structured parsing and secret/private-data detection.
- Redaction, taint metadata, and verified-safe markers.
- Sink firewalls for storage, indexes, facts, sessions, analytics, logs, APIs, UI, and exports.
- Safe audit records and incident evidence.
- Existing-data scanning, quarantine, remediation, and derivative rebuilds.
- Doctor diagnostics and healing guidance.
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
   - Every durable or externally visible sink accepts only verified-safe payloads.
   - Diagnostic messages and provenance pass the same sink firewall without
     retaining raw analyzer stderr, environment values, command lines, or source.
   - Missing, stale, or incompatible safety metadata fails closed with a structured error.
   - Derived indexes and caches cannot retain unsafe source text after remediation.

4. Detect realistically
   - Combine exact credential formats, entropy and context signals, configured private patterns,
     structured sensitive keys, and known-value fingerprints.
   - Bound scanning cost and payload size without silently accepting an unscanned remainder.
   - Findings include detector, location, confidence, and remediation class, never the secret value.

5. Audit safely
   - Record policy version, source class, detector, action, timestamps, and opaque record identifiers.
   - Logs, metrics, traces, errors, and diagnostic bundles contain redacted evidence only.

6. Remediate existing data
   - Scan legacy records and their derivatives.
   - Quarantine unsafe records before they can be served.
   - Redact, delete, or replace sources according to policy, then rebuild affected derivatives.
   - Resume safely after interruption and report bounded progress.

7. Expose operational state
   - Doctor detects disabled protection, stale policy markers, unsafe legacy rows, failed remediation,
     and derivatives that need rebuilding.
   - Safe automatic repairs run through normal daemon operations; destructive choices stay explicit.
   - UI shows coverage, findings by class, quarantine state, remediation progress, and failures.

## Acceptance

- PR5 established shared parsing, detection, redaction, receipt, and safe-marker primitives.
- Representative structured and malformed inputs prove parse-before-scan behavior.
- Every sink rejects raw, tainted, unmarked, and stale-policy payloads.
- End-to-end tests prove secrets do not appear in databases, indexes, facts, sessions, logs,
  analytics, API responses, UI payloads, exports, or diagnostic bundles.
- LSP tests prove unsaved document content remains session-ephemeral, reaches
  only authorized analyzers, and cannot reach remote analyzers without the
  required capability and disclosure.
- Remediation tests quarantine unsafe legacy data and rebuild clean derivatives after repair.
- Doctor and UI expose actionable state without reproducing sensitive values.
- Performance limits fail visibly and safely instead of skipping protection.
