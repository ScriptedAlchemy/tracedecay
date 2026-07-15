# TraceDecay V2 Secret Detection, Redaction, and Private-Data Safety Plan

**Status:** implementation plan; no product code, store mutation, or secret discovery/remediation is performed by this pull request.

**Parent plan:** [`../2026-07-09-tracedecay-brain-rewrite.md`](../2026-07-09-tracedecay-brain-rewrite.md)

**Related plans:** [`01-domain-crate.md`](01-domain-crate.md), [`02-store-crate.md`](02-store-crate.md), [`03-capture-crate.md`](03-capture-crate.md), [`04-projectors-crate.md`](04-projectors-crate.md), [`05-query-crate.md`](05-query-crate.md), [`09-application-crate.md`](09-application-crate.md), [`10-api-crate.md`](10-api-crate.md), [`11-dashboard-frontend.md`](11-dashboard-frontend.md), [`12-root-compatibility-migration.md`](12-root-compatibility-migration.md), [`13-research-provenance-and-context-anchors.md`](13-research-provenance-and-context-anchors.md), [`14-historical-failure-regression-matrix.md`](14-historical-failure-regression-matrix.md), [`17-official-public-api-and-sdks.md`](17-official-public-api-and-sdks.md), [`20-configuration-control-plane.md`](20-configuration-control-plane.md), [`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md), [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md), [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md), [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md), and [`27-cross-host-agent-plugin-bundles.md`](27-cross-host-agent-plugin-bundles.md).

Plan 20 is the only user-control surface for this plan's detector/redactor/privacy/retention/quarantine configuration. It must render the mandatory floor, effective source, coverage, consumer acknowledgement, and rescan/reproject/reindex impact in Brain Settings and generated CLI/MCP/API/SDK bindings; no provider metadata or hidden file may weaken the floor.

Plans 22–23 add model prompts/outputs, suggestion envelopes, query literals/logs, temporal assertions, summary DAGs, logical-copy fingerprints, evaluation qrels, and context bundles as explicit source/sink classes. They require authorization before hydration, privacy-domain-keyed identity, local-only defaults, egress grants, deletion lineage, and zero unsafe content in hints, indexes, fixtures, reports, or transport explanations.

Plan 24 adds initiative/plan/task text, dependency/acceptance/decision records, executor manifests/routes, capability grants, context packets, sibling summaries, workspaces, logs, handoffs, artifacts, outcomes, costs, adapter streams, task views, and orchestration fixtures as explicit sources/sinks. Lease proofs and credentials are protected control-plane values that never enter ordinary stores, prompts, logs, transports, screenshots, exports, or research anchors.

Plan 27 adds canonical host-bundle sources, every rendered host/package tree, signed manifests, native marketplace payloads/release indexes, license/SBOM files, owned host-config semantic diffs and rollback backups, raw hook stdin envelopes, capability probes, doctor output, and conformance diagnostics as explicit scanner surfaces. General stores retain only safe component IDs, bounded states, digests, path fingerprints, coverage, and protected receipt references. Config bodies, config hunks, backup bytes, raw hook input, environment values, and unsanitized diagnostics live only in the protected operation/rollback/quarantine boundary for the minimum TTL; they are never searchable, indexed, exported, or copied into research/accounting stores.

**Publication snapshot:** [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md) are normative. Branch/session variants, consolidation indexes and retirement ledgers, lifecycle/registry repair diagnostics, FTS maintenance evidence, graph checkpoint artifacts, both source families/backups, and every doctor/command output are separate privacy canary surfaces.

## 1. Verdict on the current system

TraceDecay already contains useful redaction code, but it is not one reliable redaction system.

The strongest current path is LCM raw-message ingest in `src/sessions/lcm/raw.rs`:

- `prepare_message` calls `redact_sensitive_text` before payload externalization and before the canonical session projection is written.
- Text redactors cover API-key assignments, bearer tokens, password assignments, and complete private-key blocks.
- Structured JSON traversal can redact values under sensitive-looking keys.
- Redaction metadata records patterns and irreversible/lossy status.
- Tests prove enabled redaction prevents plaintext from reaching the LCM/session FTS projection.

The critical problem is `ingest_config`: `sensitive_patterns_enabled` defaults to `false`, and the setting comes from each message’s metadata. Current provider adapters do not establish one mandatory profile policy. In ordinary ingest, the same storage path therefore preserves content unchanged and makes it searchable. Existing status reports infer “redaction enabled” from whether any lossy rows exist, not whether a protective policy is configured and complete.

Other protections are separate and inconsistent:

- `src/memory/hygiene.rs::detect_secret_like` rejects secret-like fact creates/updates and suppresses memory injection/digests. It is a detector/reject guard, not a general redactor.
- Project-list/context renderers omit credential-bearing Git remote URLs, but the registry and every other output path do not share a universal safe-text contract.
- Current Codex structured tool-event projection stores byte counts rather than raw tool arguments/output in FTS text; the historical bug and fix must remain a regression fixture.
- Claude `redacted_thinking` and Codex encrypted reasoning are respected, but provider-native redaction is not equivalent to credential scanning.
- Memory curation detects secret-like legacy facts but currently includes a 200-character `truncated_content` field in its candidate, which can reveal the exact value it is recommending for deletion.
- The LCM placeholder contains plaintext length and, for most classes, a 16-hex unkeyed SHA-256 prefix. That leaks equality and enables dictionary testing for weak secrets.
- There is no complete inventory/scan/quarantine/rebuild process for secrets already present in session rows, FTS, summaries, vectors, facts, code graph snippets, analytics, caches, exports, WAL, backups, fixtures, or release artifacts.

Conclusion: preserve useful current behavior as fixtures, then replace it with one fail-closed classification and sanitization boundary. “Opt-in lossy LCM redaction” is not an acceptable V2 default.

The implementation audit also found concrete bypass classes that the migration cannot hide behind the generic word “content”:

| Current seam | Observed gap | Required V2 disposition |
|---|---|---|
| Hermes activity/session projection | A projection-only path can bypass the normal LCM preparation/redactor. | Every provider and legacy importer must produce `Unclassified<T>` and cross the same capture sanitizer before any observation, projection, or compatibility read becomes serving. No projector accepts provider-native content directly. |
| Hook analytics | The full command can be persisted twice through separate analytics fields. | Analytics accepts only catalog-safe IDs/classes/counts and sanitization receipt refs; command/query/prompt/tool text is structurally unrepresentable, including compatibility analytics. |
| Bounded MCP failure rendering | A bounded/truncation failure reason is returned without secret scanning. | Application errors and retry directives use `LogSafeText`/`CatalogSafeText`; transport-generated detail is sanitized before serialization and cannot embed a rejected input or payload excerpt. |
| LCM summaries | Summary output does not pass a complete post-model scan. | Models receive only `PromptEligibleText`; every returned summary is `Unclassified` again, rescanned, and converted to `SearchEligibleText` before storage/indexing. |
| Response handles | Payloads are written with direct filesystem writes. | Response handles are migration-only and store only sink-eligible bytes through a private atomic writer; V2 paging uses authenticated cursors and durable citations use `RetrievalAnchorId`/`RetrievalAnchorRecordV1`. |
| LCM backup/copy | Backup paths can copy store bytes directly. | Backup/restore uses privacy manifests, isolated staging, current scanner versions, projection rebuild, and promotion receipts; a direct serving `copy` path is prohibited. |
| Dashboard server and plugin views | Raw content/metadata can be rendered; arbitrary `--host` can expose an unauthenticated surface. | Plan 10 loopback authentication/Host/Origin/CSRF/CSP is mandatory; plan 11 consumes only sanitizer-eligible application views. Non-loopback bind is rejected in the first V2 default. |
| Memory facts/entities | Tags, entity labels, source fields, metadata, and the V11 legacy direct-insert/vector path are not covered uniformly. | Every fact field and legacy row is sanitized before entity extraction, vectors, FTS, trust, or projection; the V11 path is import-only, non-serving, and rebuilt from sanitized authority. |
| Redaction status | “Enabled” is inferred from the existence of a lossy row. | Status reports configured policy, effective safety floor, source/sink/detector coverage, scanner version, legacy unknowns, and last verified scan independently; historical row existence is never configuration evidence. |
| Credential-shaped tests | Nine existing fixtures resemble live credential formats closely enough to trigger repository scanners. | Replace them with reserved/invalid scanner-safe canaries, keep structural detector coverage, and require zero findings across source plus every generated derivative. |

These are named regression rows in PR 2B/7A/10A/24H/33A. A generic “secret scan passed” without one result per seam is incomplete.

## 2. Historical and current evidence anchors

| Anchor | Evidence | Required regression |
|---|---|---|
| `session:agent-a0142b3f24b97b5de` | 2026-07-09 adversarial audit confirmed unredacted Codex tool preview content reached FTS on the then-audited master and separately confirmed `sensitive_patterns_enabled=false`. It recommended a source-ingest redaction contract rather than reusing memory’s reject-only detector. | Tool arguments/output, messages, metadata, replay fields, and provider additions pass one mandatory sanitizer before any TraceDecay persistence/search projection. |
| `session:agent-adbbfd3b92fec0808` | Parallel audit aggregation preserved the same security finding and evidence. | Copied audit traffic clusters under one canonical incident and does not inflate evidence strength. |
| `session:019ee5d9-6b70-7e81-b9d2-804c61fc4bea` | Live status explicitly reported Codex LCM redaction disabled. | Status distinguishes configured policy, coverage, findings, sanitized rows, quarantined rows, legacy/unscanned data, and detector version. |
| `session:1172143d-d85c-4cd8-aeac-3d4af50dc7e8` | Earlier LCM parity work added private-key and quoted-password redaction tests while retaining opt-in behavior. | Import all redactor fixtures, but change policy ownership/default and add every forbidden sink. |
| `session:bb6a2927-0ae6-46ed-9aed-b2e5928eb20a` | Parity review found quoted multi-word password behavior diverged across implementations. | One canonical detector/redactor implementation and conformance corpus serve every provider/transport. |
| Current planning parent `session:019f4906-a411-7a11-ad3f-0d58deb0e847` | Plan/corpus secret scan found default scanner false positives on private-key markers; parsed-value validation rejected a serialized-JSON cross-field URL alert; conservative sanitization was required. | Parse structured fields before scanning; never run permissive credential regex across serialized record boundaries; report no candidate content. |

The private chronological research corpus remains outside Git and mode `0600`. Its sanitized canonical files contain 34,333 native `role=user` rows and 9,969 best-effort human rows, including a manifest-labeled 28-record root-rollout fallback after supported replay failed; `gitleaks 8.30.1` reports zero findings after conservative redaction. The corpus is still not a fixture and must never be ingested into a TraceDecay test/profile store.

## 3. Non-negotiable security invariants

1. Secret or unadjudicated secret-like plaintext never enters any general-purpose TraceDecay store, index, prompt, output, cache, fixture, export, log, or package.
2. Every content-bearing input crosses one versioned sanitization boundary before persistence or agent/API exposure.
3. Scanner failure, timeout, unsupported encoding, incomplete structured parsing, or unknown policy fails closed: persist a non-content quarantine/coverage skeleton, not plaintext.
4. Provider/source metadata can request stricter handling; it cannot disable the profile’s mandatory secret policy.
5. Raw provider transcript/source remains provider-owned. TraceDecay stores a source locator/digest and sanitized projection by default, not a second raw secret copy.
6. Optional forensic retention uses a separate encrypted quarantine/key domain, is never indexed or exported, requires explicit user policy/access, and has short retention.
7. Detector findings, logs, analytics, errors, and UI never include the candidate value, prefix/suffix, surrounding text, raw URL, or unkeyed candidate digest.
8. Public redaction markers reveal class and an opaque receipt ID only. They do not reveal exact length, byte count, source substring, or cross-domain equality.
9. False-positive adjudication is scoped to a keyed fingerprint, rule version, source field/context class, owner, reason, and expiry. It never contains the secret and never disables a detector globally by accident.
10. A detected credential is assumed compromised. TraceDecay explains rotation/revocation as the first remediation step; deletion/redaction alone is not presented as sufficient.
11. “Zero findings” is claimed only with complete named coverage and scanner/policy versions. Locked, skipped, corrupt, incompatible, too-large, timed-out, and unsupported inputs remain explicit unknowns.
12. V1 rollback stores/backups cannot reintroduce unsafe content. They are rescanned/migrated in isolation before becoming eligible for restore.
13. Host installation metadata is not permission to retain host content. Config/backup bodies and raw hook/probe/diagnostic payloads are protected-operation data only; general projections contain content-free digests/counts/states and authorized receipt references, and retirement preserves foreign or ownership-ambiguous bytes.
14. Multi-machine transfer is a separately enforced sink. The sender sanitizes/classifies before spooling/upload, the authority revalidates eligibility, and protected quarantine defaults to local-only. Connectivity, enrollment, replication, backup, or a Tailscale/VPN identity never implies payload permission.

## 4. Threat model

### 4.1 Inputs

- Human, assistant, system/developer, visible reasoning-summary, and protocol messages.
- Tool arguments, results, errors, environment captures, terminal output, approvals, patches, screenshots/OCR, browser/network payloads, and external payloads.
- Codex, Claude, Cursor, Hermes, Kiro, Cline/Roo/Kilo, Vibe, and future provider transcripts/stores.
- Hooks, notifications, session spools, LCM summary/compression/replay fields, goals, workflows, inter-agent messages, work claims, and automation artifacts.
- Repository files, Git diffs/commits/remotes/config, diagnostics, build logs, generated source, dependency files, fixtures, and archives.
- Facts, memories, skill drafts/support files, proposals, annotations, saved views, imports, exports, support bundles, and API/SDK requests.
- Canonical host-bundle skill/rule/command/agent/hook/MCP sources; resolved rendered package trees; plugin/marketplace manifests, upload payloads, release indexes, signatures, SBOM/license inventories, install selections, and component archives.
- Host-config parse trees and owned semantic diffs, pre-change rollback backups, hook stdin/output envelopes, capability probes, host/version/surface diagnostics, conformance fixtures/results, doctor/repair output, cache/package inventories, and reload/trust state.
- V1 stores, SQLite WAL/SHM/temp files, graph generations, payload directories, caches, backups, recovery sets, and crash artifacts.

### 4.2 Forbidden sinks

- `catalog.db` and safe identity/alias labels.
- Activity/project canonical content rows unless sanitized.
- FTS indexes, token dictionaries, snippets, facets, and rank features.
- Dense, sparse, late-interaction, rerank, or summary representations and model caches.
- Code graph node/edge labels, source snippets, fingerprints, symbol docs, diagnostics, and embeddings.
- Facts/entities/memories/trust/feedback, managed skills, curation proposals, automation prompts/artifacts.
- Hint/tool-routing context, nearby-agent summaries, work claims, research-anchor labels, prompt injections.
- Logs, traces, metrics labels, analytics metadata, errors, panic/crash reports, doctor output.
- HTTP/MCP/CLI responses, SSE events, cursors, pagination tokens, deep links, browser history/storage, source maps.
- Query/result caches, response handles, summary DAGs, replay bundles, exports/shares/support bundles.
- Raw host config or backup bodies, config diff hunks, hook stdin/tool payloads, environment snapshots, host probe command output, unclassified doctor/conformance diagnostics, rendered plugin trees, marketplace staging payloads, and failed publication/install archives.
- Tests, fixtures, snapshots, benchmark/qrel corpora, docs/examples, generated OpenAPI/SDK/frontend bundles, release archives.

### 4.3 Adversaries and accidents

- A real credential pasted by the user or returned by a tool.
- A provider transcript storing secrets that TraceDecay did not create.
- Nested/escaped JSON, URL/userinfo, query parameters, headers, shell assignments, multiline PEM, encoded or split tokens.
- A malicious payload designed to evade regex, cause catastrophic work, cross structured-field boundaries, or poison an allowlist.
- A false-positive detector that destroys useful evidence or exposes the candidate during review.
- A stale projection/backup/cache restoring content after apparent deletion.
- Cross-project HMAC/equality correlation leaking that two privacy domains contain the same secret.
- A detector plugin exfiltrating candidate text or using the network.
- A revoked/compromised enrolled node, misconfigured reverse proxy/VPN, stale replica/cache, or offline node retaining/resurrecting content after policy/retention changes.

### 4.4 Host-bundle and installation containment

- Scan the exact canonical source tree, each deterministic rendered package tree, every signed manifest/SBOM/license file, component archive, and marketplace upload/index before signing and again from the downloaded candidate. A source-tree pass cannot stand in for a rendered-artifact pass.
- Parse host config by its native typed format. The deploy adapter computes a bounded owned semantic diff and stores prior owned bytes/hunks only in the encrypted protected rollback store with private modes, a short TTL, ownership/device/inode checks, and an opaque receipt. General integration state stores relative component identity, privacy-bound path fingerprint, pre/post digest, mode, and receipt reference only.
- Raw config bodies, backups, hook stdin, environment data, capability-probe output, and unclassified doctor/conformance diagnostics never enter the observation journal, activity/project stores, FTS/vector indexes, metrics, research manifests, support exports, or release receipts. Sanitization occurs in memory before any safe event; scanner failure produces an unknown/quarantined skeleton.
- Hook stdin is schema-bounded per event and sent directly to the mandatory sanitizer. The hook dispatcher may retain safe IDs, event kind, byte counts, timing, coverage, and a sanitization receipt; it cannot spool the original envelope for later analysis.
- Codex `transcript_path`, `agent_transcript_path`, and `cwd` are untrusted classified locators, never identity, authorization, database paths, or implicit read grants. The synchronous hook never opens either transcript path or relies on its unstable format; a separately authorized source broker may later resolve a fingerprinted locator under a versioned provider adapter. `prompt`, `tool_input`, `tool_response`, `last_assistant_message`, approval descriptions, stdout/stderr, and rewritten inputs remain transient unclassified content until sanitized into protected payload refs. Scan failure persists only a content-free failure/coverage receipt and emits no hint, context, rewrite, or cached raw envelope.
- `PLUGIN_ROOT` is contained package state and `PLUGIN_DATA` is an owned writable plugin-data root; their Claude compatibility aliases have identical privacy treatment. None may hold or name TraceDecay databases, keys, raw transcripts, authorization material, or durable Brain identity. Package/render/secret scans cover Unix and Windows command variants without retaining their resolved environment.
- Probe and doctor commands use static argv and a restricted environment. Their structured output crosses the same sanitizer and safe-error boundary before persistence or rendering; raw stdout/stderr exists only inside the protected operation workspace until success/failure cleanup.
- Uninstall/retirement removes only bytes proven owned by signed install receipts. Foreign cache entries, user/team/workspace config, unknown fields, backups, unmanaged plugins, and ownership-ambiguous files are preserved and reported without body content.

## 5. Domain contracts

Plan 01 §7.5 is the sole definition of the closed `DataSensitivity` enum. This security plan owns its meaning, transition rules, and sink eligibility, while the domain crate publishes that exact shared type. The same module adds opaque, validated detection and receipt types:

```rust
pub struct DetectionV1 {
    pub detection_id: DetectionId,
    pub rule_id: DetectorRuleId,
    pub rule_version: DetectorRuleVersion,
    pub class: SecretClass,
    pub confidence: DetectionConfidence,
    pub field_path: ProtectedFieldPath,
    pub span: ProtectedSpan,
    pub fingerprint: KeyedSecretFingerprint,
    pub evidence: DetectionEvidenceClass,
}

pub struct SanitizationReceiptV1 {
    pub receipt_id: SanitizationReceiptId,
    pub source_observation_id: ObservationId,
    pub policy_digest: PrivacyPolicyDigest,
    pub detector_set_digest: DetectorSetDigest,
    pub parser_digest: ParserDigest,
    pub sanitizer_version: ComponentVersion,
    pub input_domain: PrivacyDomainId,
    pub input_fingerprint: KeyedPayloadFingerprint,
    pub output_digest: SanitizedOutputDigest,
    pub resulting_sensitivity: DataSensitivity,
    pub findings_total: u64,
    pub findings_by_class: std::collections::BTreeMap<SecretClass, u64>,
    pub structured_fields_scanned: u64,
    pub raw_fallback_used: bool,
    pub decode_depth: u8,
    pub completeness: ScanCompleteness,
    pub occurred_at: UtcMicros,
    pub expires_at: Option<UtcMicros>,
    pub supersedes_receipt_id: Option<SanitizationReceiptId>,
}

pub struct SanitizationReceiptRevocationV1 {
    pub revocation_id: ManifestId,
    pub receipt_id: SanitizationReceiptId,
    pub reason_code: RegistryEntryId,
    pub revoked_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}
```

This is the exact canonical field set generated from `tracedecay-domain` and losslessly lowered by plan 02. `findings_total` must equal the checked sum of `findings_by_class`; expiry and append-only `SanitizationReceiptRevocationV1` can only narrow eligibility; and `supersedes_receipt_id` points from the new receipt to the immediately previous receipt for the same source observation. Supersession is acyclic and single-successor, never crosses observations/privacy domains, and neither supersession nor later revocation mutates the historical receipt. Canonical schema/encoding and domain -> store -> domain round-trip fixtures fail on any missing, renamed, defaulted, or extra semantic field.

Content taint types enforce sink eligibility:

- `Unclassified<T>` can exist only inside capture/parser memory and cannot serialize to a repository interface.
- `Classified<T>` carries sensitivity and detector receipt but is not automatically indexable.
- `Sanitized<T>` proves the mandatory detector/policy version and contains no retained candidate bytes.
- `CatalogSafeText`, `SearchEligibleText`, `PromptEligibleText`, `ExportEligibleText`, and `LogSafeText` require explicit checked conversions from `Sanitized<T>`.
- `ProtectedSecretRef` points only into the isolated quarantine service; it cannot implement display/serialize-to-public-envelope.
- Repository traits accept eligible types rather than `String` for content-bearing fields.

Raw `String`, `serde_json::Value`, and byte slices are forbidden at the application-to-store, projector-to-index, and application-to-transport content ports. Architecture lint/compile-fail tests enforce this boundary.

This is the single authoritative content-safety type system for plans 01–12 and 15–17. Those plans may define domain-specific view models, but every content-bearing field must be one of these eligible wrappers or a typed redacted/denied/unknown state; they must not introduce a parallel `SafeText`, `AuthorizedContent`, or “already trusted JSON” bypass. `RetrievalAnchorRecordV1`, cursor, status, capability, error, and operation envelopes contain opaque identifiers and safe metadata only and are subjected to the same output-sink check.

## 6. Privacy policy ownership and precedence

`PrivacyPolicyV1` is profile-owned, versioned, signed/digested, and evaluated before content persistence:

1. Non-disableable built-in safety floor.
2. Profile policy.
3. Project/privacy-domain policy that may strengthen but not weaken the floor/profile.
4. Source/provider policy that may add formats or stricter retention.
5. One-record metadata that may request quarantine/drop but cannot disable scanning.

The current pattern—reading `sensitive_patterns_enabled=false` from message metadata—is retired. Migration interprets legacy metadata only as provenance explaining old behavior.

Policy chooses:

- detector set/version and confidence thresholds.
- field/source exclusions that remain structurally scanned.
- normal/sensitive/reasoning/secret retention.
- drop versus marker versus protected-quarantine action.
- maximum field/record/archive/decode sizes and time budgets.
- optional custom detector manifests.
- authorized quarantine roles and audit requirements.
- false-positive allow decisions and expiry.

No user option makes secret plaintext searchable. The user may choose “drop completely,” “sanitized marker only,” or “marker plus protected short-lived quarantine.”

## 7. Structured parse-before-scan pipeline

The current research false positive demonstrates why serialized-envelope scanning is unsafe. One regex over a JSONL line can match characters from different JSON fields and fabricate `scheme://username:password@host` evidence.

Required pipeline:

1. Frame one provider-native record with strict byte/size/deadline limits.
2. Parse the provider/event schema and classify fields by semantics.
3. Traverse string/byte leaves independently with a canonical protected field path.
4. Decode bounded known wrappers only inside that field: JSON string layer, URL percent encoding, base64/base64url when high-confidence metadata or detector requests it, and supported archive members under budgets.
5. Normalize Unicode for detector comparison without changing the source span mapping.
6. Run structured key/pair/format detectors, then content/prefix/context/entropy detectors.
7. Merge overlapping detections deterministically by severity/specificity; preserve all rule evidence in the protected receipt.
8. Replace spans in the typed field; validate that no candidate bytes survive.
9. Re-serialize the sanitized structure canonically.
10. If parsing fails, run a bounded raw-field fallback inside the record only. A fallback cannot scan across records and is marked incomplete when truncation/encoding prevents proof.

Chunk boundaries retain a bounded overlap window for multiline/token-split formats. The scanner never concatenates unrelated messages, fields, files, rows, or shards merely to increase recall.

## 8. Detector engine

Create `crates/tracedecay-capture/src/privacy/`:

```text
privacy/
├── engine.rs
├── policy.rs
├── registry.rs
├── structured.rs
├── normalize.rs
├── spans.rs
├── redact.rs
├── fingerprint.rs
├── allow.rs
├── budgets.rs
├── builtins/
│   ├── field_keys.rs
│   ├── credential_pairs.rs
│   ├── private_keys.rs
│   ├── authorization.rs
│   ├── connection_urls.rs
│   ├── query_parameters.rs
│   ├── assignments.rs
│   ├── providers.rs
│   ├── entropy.rs
│   └── encoded.rs
└── plugins/
    ├── manifest.rs
    ├── wasm.rs
    └── subprocess.rs
```

### 8.1 Built-in detectors

Run cheapest/highest-precision first:

- Provider-specific prefixes and version/checksum/length rules.
- Paired identifiers plus secret values when a format defines them.
- JSON/YAML/TOML/env/header field names with typed string values.
- PEM/OpenSSH/private-key blocks and credential files.
- `Authorization` bearer/basic/token values and cookies/session keys.
- URI userinfo, database/cache/broker/cloud connection strings, and SCP/remote credential forms.
- Secret-bearing query/fragment parameters.
- Shell/env/config assignments, including quoted/multiline values and compact/camel/hyphen aliases.
- JWT and signed token shapes without attempting online validation.
- Context-keyword plus tuned entropy for otherwise unknown tokens.
- Bounded encoded-form recursion with a maximum depth and decoded-byte budget.

Detection output never includes the matched value. Do not copy `gitleaks`/provider rule output blindly; translate it into the protected `DetectionV1` shape.

### 8.2 Runtime versus offline scanners

- The Rust built-in detector is the mandatory low-latency runtime safety floor.
- Pinned `gitleaks` is a CI/release/fixture/offline differential scanner, not an in-process library or the sole runtime guarantee.
- A second independent scanner may run in offline audit/shadow mode to estimate missed classes.
- Rule changes ship with corpus/precision/latency results, migration scan version, and rollback.
- Network validity checks are disabled by default. They can disclose candidate values; any future provider-specific validity integration requires explicit user consent, an allowlisted endpoint, no log/cache, and a separate threat review.

### 8.3 Detector plugins

Custom detectors are signed/versioned manifests plus constrained WASM or supervised subprocess ABI:

- Input is one bounded field buffer and safe metadata, never arbitrary filesystem/store access.
- Output is spans, class, confidence, and reason code only.
- Network, environment, filesystem, process spawn, clocks, and randomness are denied by default.
- Per-call CPU/memory/deadline limits and deterministic conformance tests are mandatory.
- Plugin timeout/crash marks the scan incomplete and blocks the sink.
- In-process dynamic-language plugins are prohibited because detector code sees candidate secrets.

## 9. Fingerprints and redaction markers

Current `sensitive_placeholder` length/hash metadata is removed from public output.

Use:

```text
⟦REDACTED:credential:sr_01J...⟧
```

`sr_...` is a random sanitization-receipt reference safe to reveal inside the authorized profile. It is not a content hash. Detailed detector evidence is separately authorized and still contains no candidate.

For dedupe, repeated-leak correlation, allow adjudication, and purge verification, compute `HMAC-SHA-256(domain_key_epoch, canonical_candidate_bytes)`:

- Key is unique per privacy domain and stored/wrapped through the protected key service.
- Fingerprint never crosses profiles/domains or appears in normal API/CLI/MCP/dashboard/log output.
- Key rotation prevents indefinite correlation; historical fingerprints are rewrapped/recomputed only inside remediation.
- Very short/low-entropy credentials may omit reusable fingerprint entirely and use random detection IDs.
- Input/output content-addressing never uses an unkeyed hash for secret plaintext.

Internal spans use byte ranges for deterministic replacement and Unicode tests. External safe findings expose field class/marker ordinal, not enough exact positions/length to reconstruct a secret.

## 10. Protected quarantine

Default behavior stores sanitized content plus a source locator only. Optional forensic preservation is explicit:

- Separate directory/store and encryption key domain from normal blobs.
- Random blob ID; no cross-domain content-addressed dedupe.
- Per-record data-encryption key wrapped by an OS-keyring/profile key-encryption key.
- Authenticated encryption includes profile/domain/source/receipt/policy as associated data.
- Mode `0600`/private directory at first syscall; no plaintext temp file, SQLite value, WAL, log, or crash artifact.
- No FTS/vector/summary/fact/graph/export/backup with normal data.
- Access requires exact finding, explicit role, reason, confirmation, and audit; API/SDK agent tokens are denied by default.
- Initial TTL 24 hours; user hold is explicit, visible, time-bounded, and reviewed.
- Expiry destroys the wrapped data key and blob; non-content tombstone/receipt remains.
- Quarantine backup is disabled by default. If enabled, it is separately encrypted/restricted and restore-scanned in isolation.

The durable object state machine is `Staged -> Attached -> Retiring -> Retired` for committed evidence and `Staged -> Retiring -> Retired` for unattached expiry. A hold appends `Attached -> Held`; release or hold expiry appends `Held -> Attached`, re-evaluates the original absolute retention deadline, and immediately enters `Retiring` when that deadline has passed. Repeated hold/release/retire requests are idempotent under optimistic version; a hold cannot revive `Retiring`/`Retired`, and a crash at every journal/key-destruction/blob-unlink boundary resumes from the append-only event chain without making plaintext readable or losing the non-content tombstone.

If the OS keyring is unavailable or locked, quarantine retention fails closed to sanitized-only/drop; it never stores plaintext “temporarily.”

## 11. Sink firewalls by domain

### 11.1 Sessions, LCM, tools, goals, and workflows

- Sanitize every message/content part, tool argument/result/error, visible summary, goal/task, inter-agent message, workflow input/output, and replay field before observation persistence.
- Preserve the provider-native source locator as a privacy-domain-bound digest, offset, keyed source fingerprint, and sanitized structure; do not copy raw content or an unkeyed checksum into metadata.
- Parent/subagent prompt copies reuse sanitized entities/refs, never rescan a concatenated serialized envelope.
- Summary/compression models receive only `PromptEligibleText`; summaries are rescanned before storage.
- LCM/session FTS indexes only `SearchEligibleText`.
- Status reports policy configured/effective, adapter/detector coverage, last full scan, sanitized/quarantined/legacy-unscanned counts, and unknowns separately.
- Hermes/legacy projection-only importers and the V11 memory migration path are source adapters, not trusted projectors: they must cross the sanitizer before canonical append and cannot insert serving rows/vectors directly.
- Response-handle payloads and LCM backups use the private store/backup ports with eligible bytes, manifests, atomic publication, and restore scanning. Direct `fs::write`/`fs::copy` of content-bearing serving artifacts is a forbidden-import regression.

### 11.2 Code graph, Git, diagnostics, and delivery

- Scan repository text/string literals/config/diffs/commit messages/diagnostics/logs before storing snippets, graph labels, FTS, or embeddings.
- Ignore policies reduce input scope but cannot make included secret content indexable.
- Secret files/fields/lines create redacted node/coverage markers and source locators; structural code identity may remain if it contains no candidate bytes.
- Git remote/userinfo is parsed and credentials removed before catalog identity, matching, rendering, or analytics.
- Live GitHub/provider payloads pass the same sanitizer before cache/storage; remote URLs are allowlisted and credential-free.
- Code examples use synthetic invalid/reserved canaries; no real token copied into a fixture to test detection.

### 11.3 Facts, memories, entities, skills, and automation

- Keep write-time secret rejection as defense in depth, but feed it the shared detector engine/profile.
- Legacy secret-like facts become immediately non-hydratable/quarantined; curation receives safe class/ID/reason only, never `truncated_content`.
- Entity extraction, embeddings, trust/dedupe/conflict logic, digests, memory injection, and skill writing never receive candidate plaintext.
- LLM/automation “do not include secrets” prompts are not controls; sanitizer and typed output validation are mandatory after model output.
- Managed-skill support files, candidates, run artifacts, and materialization packages scan before autonomy decision and again before autonomous materialization/recovery. A failed or incomplete scan automatically rejects/quarantines the candidate; it never creates a human approval queue or bypass.
- Fact tags, entity names/aliases, source descriptors, metadata/extensions, feedback, and deletion/supersession notes are content-bearing; all use eligible wrappers before persistence or projection. Legacy V11 inserts and their vectors are quarantined/imported/rebuilt, never trusted in place.

### 11.4 Hooks, hints, policy, coordination, and analytics

- Hook hot path uses the compiled runtime safety floor under its latency budget; timeout emits no content/hint and spools only an encrypted or non-content blocked receipt.
- Claude's 30-event surface expands the mandatory source map to original/expanded prompts, display deltas, successful/failed/batched tool payloads, permission suggestions/denials, instructions/config/file/worktree locators, task/team text, background tasks/session crons, compact summaries/instructions, MCP elicitation form schemas/values, stop failures, notifications, and session-end metadata. Every field has an explicit content/locator/control classification; `MessageDisplay` bodies are dropped by default and lagging `transcript_path` is never synchronously read.
- Foreign Claude definitions add protected command/args/shell, HTTP URL/header/`allowedEnvVars`, MCP server/tool/substituted input, prompt/agent/model bodies, `${user_config.*}`, path placeholders, environment-derived values, and async result artifacts. General projections/UI/replay retain only type/state/digest/coverage refs. Foreign command/HTTP/MCP/prompt/agent handlers are never executed during replay, scan, doctor, migration, or verification.
- Generated TraceDecay Claude hooks are synchronous command exec form with closed args and never write `CLAUDE_ENV_FILE` or `${CLAUDE_PLUGIN_DATA}`. HTTP/MCP fail-open behavior, prompt/agent model calls, async next-turn delivery, and `asyncRewake` cannot become sanitization, durability, authorization, or hint-delivery boundaries.
- Claude's 10,000-character `additionalContext` spill creates an external session-file descendant. Generated context stays below the cap; observed spill records only classified locator fingerprint, content-free coverage, and source-broker eligibility. TraceDecay never tells a model to open an unscanned spill file. Other large event/error bodies retain the ordinary bounded-decode rules rather than inheriting this spill claim. `terminalSequence` is accepted only as an allowlisted control-class field and is never replayed/exported as raw bytes.
- Hint candidates/context/payload, nearby-agent summaries, work claims, and research labels require `PromptEligibleText`/`CatalogSafeText`.
- Policy/replay input bundles reference sanitization receipts and cannot request unsanitized content.
- Analytics store event/use-case/safe dimensions/counts only. Tool arguments, outputs, prompts, query literals, error bodies, candidate values, and secret fingerprints are prohibited.
- Hook analytics schemas have one payload-free command/use-case identity field. Differential tests prove the former two-field full-command duplication cannot be serialized through either live or compatibility analytics.

### 11.5 API, MCP, CLI, dashboard, SDK, and browser

- Application use cases return typed redacted/denied/unknown states; transports cannot bypass classification.
- Errors and retry directives name safe rule/state/action only.
- Cursors, anchors, URLs, deep links, SSE IDs, ETags, and operation receipts contain opaque IDs/digests of sanitized contracts, never query/candidate content.
- Dashboard never places payloads/query text in URL/history/local storage and never shows secret candidate previews.
- SDK `Debug`/`Display`, generated examples, OpenAPI examples, explorer history, source maps, and exception/log hooks pass secret canaries.
- Bounded/truncated MCP/CLI/HTTP failure reasons are constructed from safe reason enums plus `LogSafeText`; adapters cannot attach the rejected request, raw error, command, query, or payload excerpt.
- Dashboard/API startup refuses arbitrary unauthenticated host exposure. Every raw-content and metadata view is authorized and sanitizer-eligible before it reaches JSON, SSE, DOM, renderer workers, browser cache, or export.

### 11.5A Database client isolation

Application authorization does not make a SQLite pathname safe. Strong isolation requires one of two verifiable plan-01 modes:

- `DedicatedServiceIdentity`: the daemon runs under a dedicated OS service identity. Its state/database/WAL/SHM/backup/key directories are owned by that identity with no client-user read/traverse ACL; the service-owned local Unix socket or Windows named pipe has a narrow connect-only client ACL and performs peer plus application authentication/authorization. Linux uses a dedicated system user plus systemd state/socket controls, macOS a dedicated service account/LaunchDaemon and ACL-owned state root, and Windows a service virtual account plus NTFS/named-pipe DACL. Database encryption/key material, when enabled, is owned by the service identity and never supplied to clients. The daemon receives no broad client-home access: user-side hooks/read-only source brokers read only registered provider/repository inputs, sanitize/frame typed observations or immutable snapshots, and submit them over that endpoint. A distinct user-effect broker executes application-authorized filesystem/Git/worktree/owned-host-config/contained-workspace mutations through short-lived signed exact-resource grants, race-safe primitives, revocation, idempotency, receipts, and uncertain-effect reconciliation; it never widens the source broker or reads TraceDecay stores. Optional direct project ACLs are explicit, narrow, and audited.
- `RemoteAuthorityOnly`: no mutable or replicated database file exists on the client node; clients hold only the authorized encrypted spool/cache classes explicitly allowed by plan 28 and query the authority API.

`SameUserDegraded` is supported only as an honestly labeled portability mode. `0700`/`0600`, hidden paths, SQLite locks, daemon leases, and an in-process keyring cannot prevent another process controlled by the same OS user from reading bytes; doctor/UI/API must report `database_read_denied_to_clients=false` and may never call this strong isolation. A user can migrate to a dedicated service identity or remote authority without changing Brain/profile identity.

In every mode, clients receive no store pathname, SQLite URI, database file descriptor/handle, page/WAL bytes, raw backup, or key. All ordinary reads/exports are semantic application use cases. Strong-mode conformance launches an untrusted client under the real client identity and proves it can connect to the authorized endpoint but receives access denied when listing, statting, opening, memory-mapping, copying, or backing up every database/WAL/SHM/key/backup path; it also proves the daemon can operate them and that ACL drift closes readiness rather than silently degrading. Install/upgrade and periodic renewal use a packaged, narrowly privileged service-manager helper: a signed nonce plus fixed path-free probe-manifest ID causes systemd/launchd/Windows SCM to run the negative checks as the configured client identity and return only signed content-free results. The daemon continuously verifies metadata/ACL drift between challenges but cannot self-attest or renew real-identity evidence. Missing helper, caller-supplied paths, identity mismatch, expiry, or failed challenge stops strong readiness. These probes mint the variant-specific, expiring `StoreIsolationStatusV1` receipts; a stored boolean is insufficient.

### 11.6 Fixtures, evaluation, exports, and release artifacts

- A production DB/store/transcript/cache/export is never copied directly into a fixture.
- Fixture promotion selects minimum structure, sanitizes, replaces all identity/content with synthetic values, scans every generated derivative, and records a zero-findings receipt.
- Private qrels/eval prompts remain protected local data; committed evals are synthetic/minimal redacted.
- Export/share/support-bundle jobs rescan output bytes/archives before publication and include privacy manifest/coverage.
- CI scans staged diff, introduced history, archives, snapshots, generated API/SDK/docs/frontend/source maps, binaries/packages where supported, and release bundles.

### 11.7 Task-graph edit-bundle containment and retirement

The plan-10/17 `task_graph.edit_bundles.*` workflow treats every exported, downloaded, edited, uploaded, rebased, and submitted byte as protected temporary user data. It never accepts or returns a server path. The API streams only the generated uncompressed edit-bundle media type containing strict-frontmatter `manifest.md` and sharded CommonMark; local CLI/SDK helpers may open a caller-owned directory or archive, but that path is consumed in the caller process and is absent from requests, errors, telemetry, anchors, and receipts.

- Composition owns one profile-private runtime root at `0700`; each random bundle/generation directory is `0700` and each manifest/shard/staging file is `0600`. Creation uses exclusive no-follow/open-beneath semantics where supported and a component-by-component descriptor walk elsewhere. Publication verifies parent/child device and inode identity before atomic rename.
- Archive intake is single-pass and bounded before extraction. Absolute paths, `..`, empty/dot components, normalization or case-fold collisions, duplicate entries, undeclared entries, symlinks, hardlinks, devices, sockets, FIFOs, sparse/overlapping entries, extended-header path rewriting, nested archives, depth greater than eight, or names longer than 128 normalized UTF-8 bytes fail closed. No archive metadata may change owner, group, mode, time, xattr, ACL, or destination.
- The strict YAML subset permits only bounded maps, sequences, strings, booleans, integers, and null. Tags, anchors, aliases, mapping-merge syntax, repeated or structured mapping names, inferred timestamps, floats/non-finite numbers, multiple documents, unknown schema fields, and executable constructors are forbidden. CommonMark is parsed with raw HTML disabled; code fences and links are data, never executable UI or filesystem instructions.
- Every candidate generation is scanned after streaming/containment and again after parse/canonicalization; submit and rebase verify the exact scan/policy/input digests. A zero-findings claim requires complete manifest/file/byte coverage. A finding, scanner timeout/crash/version gap, unsupported encoding, truncated upload, or unknown coverage purges the candidate immediately and retains only a safe failure receipt; secret-like bytes are never kept merely to help the agent repair them.
- Ordinary secret-clean syntax, reference, CAS, or semantic validation failures remain in the private runtime only so the agent can fetch diagnostics and resubmit before expiry. Defaults are two hours, 64 MiB total uncompressed bytes, 2 MiB per file, 4,096 files, and 50,000 items; hard ceilings are 24 hours, 256 MiB, 8 MiB, 16,384 files, and 100,000 items. Bounds are checked against declared and observed counts throughout streaming.
- Successful atomic submit purges every bundle byte before returning terminal success. Explicit delete, token/profile revocation, lease/TTL expiry, failed containment, and policy invalidation also purge immediately. A startup scan plus a five-minute sweeper retires crash residue using descriptor/inode checks; it never follows a discovered link or trusts a filename from a receipt.
- Durable submit/delete/expiry/sweeper receipts contain only `TaskGraphEditWorkspaceId`, `TaskGraphEditCandidateRefV1`, source/manifest/policy digests, safe part/item/byte counts, disposition/reason enums, timestamps, and audit/retrieval anchors. They contain no Markdown/YAML/archive content, logical filename, client path, physical path, candidate excerpt, secret fingerprint, or inode/device value.

Line-addressed validation output is plan 01's exact transient `TaskGraphEditDiagnosticV1`, not durable content: safe code/severity/phase, optional contained relative-file byte and line/column span, optional editable subject and field path, safe message, optional bounded deterministic text edit, and evidence anchors. Parser/library exceptions, YAML tokens, source lines, archive entry bytes, and surrounding text never reach the diagnostic.

### 11.8 Multi-machine synchronization and remote stores

Every domain resolves exactly one plan-28 sync class: `NeverSync`, `MetadataOnly`, `SanitizedEncrypted`, or `FullEligible`. Classes bind source and destination privacy domain, principal/node grant, sanitizer/detector/policy versions, retention, and placement. The sender enforces before durable upload; the receiver revalidates before canonical commit. A stricter policy applies immediately, while relaxation requires explicit activation and a fresh eligibility scan.

- `NeverSync` content and all reconstructable descendants remain on the local authority. Only allowlisted non-sensitive availability may be reported.
- `MetadataOnly` permits bounded allowlisted identity/count/health fields, never payload or reversible features.
- `SanitizedEncrypted` permits sanitizer-approved records over authenticated transport and encrypted storage.
- `FullEligible` still means sanitizer-approved fields allowed by domain/principal policy; it never means raw bypass.
- Protected quarantine is `NeverSync` by default. A future protected transfer requires a distinct elevated design/ADR and is not inferred from ordinary remote eligibility.
- Node revocation closes streams and blocks new reads/writes. Local cache/spool bytes remain encrypted, retention-bound, and purgeable; signed tombstones/purge proofs prevent offline resurrection.
- Every cache/replica authorization is a signed `CacheGrantSnapshotV1` containing plan 01's complete bounded `CacheAccessManifestV1`: principal/node, exact resolved scope, allowed registry field IDs and payload classes, capability-grant set, policy version, privacy-policy digest, schema-registry digest, and capability-catalog generation/digest. Offline validation uses this signed immutable manifest rather than an unavailable mutable grant lookup. The cache locks at mandatory `not_after`; clock rollback cannot extend it. Reconnect applies and acknowledges tombstones/purge directives before serving again, and UI/coverage exposes pending purge acknowledgements.
- Sync manifests, receipts, logs, metrics, errors, topology, and backup catalogs contain only opaque IDs, safe counts/states/digests, and authorized anchors—never addresses, paths, remote credentials, token material, candidate fingerprints, or content.

## 12. Retroactive whole-profile audit

Create a read-only-first privacy auditor. It uses store APIs/manifests, not ad hoc SQL or raw renderer output.

### 12.1 Inventory

Enumerate with stable IDs and coverage:

- Catalog, activity, project, graph generations, blobs, quarantine.
- Session/LCM/message/tool/reasoning/goal/workflow content and metadata.
- Search/FTS/representation/summary/facet/rank/cache projections.
- Facts/entities/memories/skills/automation candidates/decisions/effects, imported legacy proposal evidence, annotations, and saved content.
- Hooks/analytics/logs/error/crash/support/export/response caches.
- WAL/SHM/temp/spool/dead-letter files, backups, recovery sets, V1 stores.
- Repository fixtures/snapshots/docs/generated packages/release assets when scanning a checkout/release.

The scanner executes inside each privacy domain and emits only safe findings/counts. A locked/unopenable/corrupt source is unknown coverage, not clean.

### 12.2 Immediate containment

When a high-confidence/confirmed finding appears:

1. Mark the owning entity/store generation unsafe at a catalog-safe level.
2. Block hydration/search/export/share/hint/automation use of the entity and all derived descendants.
3. Show redacted/quarantined coverage and rotation guidance.
4. Do not print or copy the candidate while asking for remediation.

### 12.3 Rotation and remediation

The operator workflow is:

1. Rotate/revoke at the credential provider outside TraceDecay; TraceDecay may link safe documentation but performs no validity/revocation network call by default.
2. Preview the full descendant graph: canonical rows, FTS, representations, summaries, facts/entities, caches, exports, backups, references, and consumers.
3. Create sanitized replacement observations/entities or tombstones.
4. Build new FTS/vector/summary/graph/project database generations from sanitized authorities.
5. Atomically swap after secret-canary/manifest/parity verification.
6. Invalidate response/export/browser caches and revoke shared bundles.
7. Checkpoint/retire old SQLite WAL/SHM/temp/database generations under a lifecycle lease.
8. Cryptographically erase quarantine keys/blobs and delete retired artifacts after recovery policy.
9. Rescan every new generation and eligible backup; retain safe remediation receipt.

SQLite row deletion is not proof that bytes disappeared from pages, WAL, temp files, backups, or SSD media. Derived databases are rebuilt into new sanitized generations; protected raw uses cryptographic deletion. Physical-storage limitations are documented honestly.

### 12.4 Restore gate

Every backup/V1 rollback restore lands in an isolated non-serving staging profile:

- verify manifest/signature/integrity.
- apply current privacy migration and scan.
- rebuild derived projections.
- require zero unexplained/unknown forbidden-sink findings.
- issue a promotion receipt before serving.

No “emergency restore” may bypass this gate and silently reindex secrets.

For #425/V2 split-store consolidation, the selected and legacy families remain separate privacy authorities until reconciliation proves otherwise. The workflow creates two independent encrypted/restricted backup manifests, scans both plus WAL/SHM/temp/ledger/staging/table-report/collision/remapped-edge descendants, and records unknown/unsupported coverage without copying candidate values into the plan or doctor output. A clean result for one family cannot authorize the other. Deterministic confirmation binds both source manifests, privacy policy/detector versions, table dispositions, edge-remap digest, backups, and intended marker/registry change; any drift invalidates confirmation. Marker/registry publication is forbidden until both backup restore probes and the fully rebuilt candidate pass current sanitizer/canary/parity verification.

## 13. False positives, adjudication, and rule evolution

Finding states:

- `unreviewed`, `high_confidence`, `confirmed`, `false_positive`, `revoked_rotated`, `contained`, `sanitized`, `purged`, `verification_failed`, `unknown`.

Adjudication record contains detector/rule version, keyed fingerprint, field/source context class, safe reason code, owner, created/expires time, reviewer, and policy scope. It contains no candidate or surrounding source.

Rules:

- Default expiry forces re-evaluation after detector/source changes.
- Allowlists are exact keyed-fingerprint/context decisions or narrowly anchored synthetic fixture classes.
- Regex/string allowlists containing a candidate secret are prohibited.
- Broad path/project/provider exclusions cannot bypass the mandatory safety floor.
- Rule changes run read-only shadow scans and measure new/removed findings before activation.
- Historical observations retain the old receipt; current projection uses the new rule version after controlled rescan/rebuild. Rescans issue superseding `SanitizationReceiptV1` rows that reference the superseded receipt ID; sinks honor the newest non-revoked receipt. The durable home is plan 02's per-shard immutable `sanitization_receipts` plus append-only `sanitization_receipt_revocations` tables (minted by plan 03, validated by plan 04's sink firewall), so supersession, expiry, and revocation never mutate a historical receipt.
- False-positive review UI uses synthetic/structural metadata. Viewing plaintext requires separate quarantine authorization and is not required to mark common safe examples.

Durable detector-registry state (owning shard: profile catalog; contains no secret content):

- `detector_rules(detector_id, rule_version)` PK — enable state, activation source (bundled/config/plugin), complexity-policy verdict, corpus-eval result digest, created/retired timestamps; index on enable state. Retention: retired rows kept for the receipt-audit horizon.
- `adjudication_records(adjudication_id BLOB PRIMARY KEY, privacy_domain_id BLOB NOT NULL, key_epoch INTEGER NOT NULL, fingerprint_hmac BLOB NOT NULL, detector_id TEXT NOT NULL, rule_version INTEGER NOT NULL, field_context_code INTEGER NOT NULL, source_context_code INTEGER NOT NULL, owner_scope_digest BLOB NOT NULL, policy_scope_digest BLOB NOT NULL, state TEXT NOT NULL, safe_reason_code TEXT NOT NULL, reviewer_id BLOB NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL)` — UNIQUE `(privacy_domain_id, key_epoch, fingerprint_hmac, detector_id, rule_version, field_context_code, source_context_code, owner_scope_digest, policy_scope_digest)` and indexes on expiry/state. One adjudication can authorize only the exact context/owner/policy tuple; key rotation or any context/rule change requires a new row. The opaque ID is the only ordinary-surface locator.

Terminology: capture-side quarantine "skeletons" (plan 03's non-content provenance records) and this plan's section 10 protected quarantine (encrypted forensic payloads) are distinct stores with distinct lifecycles; plans must qualify which one they mean.

## 14. Product surfaces

### 14.1 CLI

```text
tracedecay privacy status [--scope ...] [--json]
tracedecay privacy scan inspect --scope current|all|<selector>
tracedecay privacy scan start --scope current|all|<selector>
tracedecay privacy scan resume <cursor>
tracedecay privacy findings list [--class ...] [--state ...]
tracedecay privacy findings show <safe-finding-id>
tracedecay privacy remediate plan <finding|scan-id>
tracedecay privacy remediate start <plan-id> --confirm
tracedecay privacy verify <remediation-id>
tracedecay privacy detectors list|test|diff
tracedecay privacy quarantine status|hold|release
```

Default output contains safe counts/classes/coverage/actions only. JSON has the same typed envelope as API/MCP and never includes candidate values.

### 14.2 Official API/MCP

- `GET /api/v2/privacy/status`, `/scans`, `/scans/{id}`, `/findings`, `/findings/{safe-id}`, `/remediations/{id}`, and `/quarantine/status` (the last under elevated authorization); read-shaped `POST /api/v2/privacy/scans:inspect` accepts protected scope/source selectors and performs no scan persistence.
- `GET /api/v2/privacy/detectors` and `POST /api/v2/privacy/detectors:diff` using synthetic caller-supplied fixtures only; the richer synthetic detector run uses plan 10 §8.5's generic experiment lifecycle with `LabKindV1::Privacy`. No privacy-specific lab endpoint or real-candidate input exists.
- Mutations use the same generated command routes as plan 10: `POST /api/v2/commands/privacy/scans/{start,cancel}`, `/commands/privacy/remediations/{plan,start,verify}`, and `/commands/privacy/quarantine/{hold,release}`.
- SSE emits safe scan/remediation progress and gaps, never findings content.

MCP exposes bounded read-only status/scan-result tools by default. Mutations require explicit current capability, exact scope, preview, idempotency, optimistic version, elevated user authorization, and audit receipt.

### 14.3 Dashboard: Privacy Observatory and Secret Safety Lab

Privacy Observatory shows:

- coverage matrix by profile/project/store/sink/source/detector generation.
- sanitized/quarantined/legacy-unscanned/unknown counts and trends.
- finding class/state/age/owner without candidate preview.
- descendant/repair graph and backup/restore eligibility.
- policy/rule versions and pending rescan scope/cost.
- remediation progress, rollback window, and verification receipt.

Secret Safety Lab is read-only and synthetic by default:

- enter or generate a synthetic invalid canary; never load a real finding value.
- visualize parse tree, decoded layers, detector candidates, overlap merge, marker output, sink eligibility, and latency.
- compare detector/policy versions, false-positive allow decision, and expected descendant purge.
- promote only a fully synthetic/minimal-redacted fixture after scan.
- lab runs never write live findings, allowlists, analytics outcomes, facts, hints, or quarantine.

## 15. Observability without leakage

Safe metrics:

- records/bytes/fields scanned by source/domain/version.
- complete/incomplete/skipped/locked/corrupt/timeout counts.
- findings by broad class/confidence/state, with minimum aggregation thresholds.
- redactions/quarantines/drops and scan/projector lag.
- detector latency/CPU/memory/decode depth/timeouts and false-positive adjudication rate.
- forbidden-sink canary results.
- remediation descendant counts/state/duration and restore eligibility.

Prohibited telemetry:

- candidate values, substrings, prefixes/suffixes, plaintext hashes, exact low-cardinality lengths.
- raw field paths when they contain user content/credentials.
- full query/prompt/tool/error/URL/header/env/config bodies.
- secret fingerprints as labels or cross-domain IDs.

Every report declares population, horizon, detector/policy version, source watermarks, cap/sampling, skipped/unknown coverage, and privacy domain.

## 16. Performance, resilience, and backpressure

- Precompile reviewed Rust regex/automata; reject patterns exceeding complexity/size policies.
- Per-record/field/decode/archive budgets; bounded overlap for chunk/multiline detection.
- Incremental scan changed sources/records by sanitized source generation, not full profile on every ingest.
- Detector registry orders cheap exact/structured rules before entropy/decoding/plugins.
- Hook target: mandatory runtime floor remains inside the existing prompt-hook p95 budget; on timeout or overrun the hook blocks the content, emits a durable non-content receipt, and produces no hint (plan 03's hook contract is canonical). Pre-scan content is never spooled for deferred scanning; the only permitted forensic retention is the section 10 protected quarantine ingress under its own TTL, keying, and mandatory-scan policy.
- Async transcript target: >= 50 MiB/s on current corpus for built-ins without encoded recursion; report cold/warm/hardware.
- Offline full-profile audit is cancellable/resumable with stable cursor and bounded concurrent shard readers.
- Regex stress, huge field, malformed Unicode, archive bomb, decode bomb, plugin hang/crash, disk full, process death, and locked keyring fail closed without plaintext persistence.
- Backpressure prioritizes non-content receipts and source cursors; it never drops a finding then indexes the original.

Targets are measured gates, not reasons to reduce security silently.

## 17. Evaluation and test matrix

### 17.1 Positive synthetic corpus

- Provider-specific invalid/reserved token shapes and identifier/secret pairs.
- PEM/OpenSSH/multiline/quoted password/authorization/cookie/session/connection URI/query-param/env/config forms.
- Nested JSON/YAML/TOML arrays/objects, camel/hyphen/compact aliases, double-encoded JSON strings.
- URL-encoded/base64/base64url depth 1–3 and tokens split across bounded chunks.
- Tool arguments/results/errors, goals, summaries, facts, skills, annotations, Git remotes/diffs/diagnostics, logs, exports.
- Unicode/confusables and malformed/truncated records.

### 17.2 Negative/adversarial corpus

- Git SHAs, UUIDs, cache keys, content hashes, public keys, package integrity, minified code, lockfiles.
- Environment-variable references with no value, docs about secret handling, `sk-test` prose, placeholders, redaction markers.
- Reserved domains and invalid synthetic credentials.
- Serialized JSON fields whose adjacent characters would form a false credential if scanned as one line.
- Secret-like identifiers with safe public values, short ordinary assignments, URLs without userinfo.
- Regex/backtracking stress, giant encoded blobs, archive/decode bombs, hostile detector plugin output.

### 17.3 Sink canary matrix

For every positive class, drive a unique invalid synthetic canary through:

- every provider transcript adapter and hook event.
- observation/spool/activity/project/graph/blob/quarantine paths.
- sessions/LCM/tools/reasoning/goals/workflows.
- FTS/vector/sparse/rerank/summary/fact/memory/skill/automation/analytics/cache projections.
- query/search/graph/timeline/replay/hint/nearby-agent APIs.
- CLI/MCP/HTTP/SSE/SDK/dashboard/browser/source maps/logs/errors/cursors/anchors.
- fixture promotion/export/share/support/backup/restore/migration/recovery/rebuild/release packages.
- task-graph edit-bundle export stream, strict-frontmatter `manifest.md` and CommonMark shards, validate upload, candidate-ref diff/rebase/submit, retained invalid candidate, runtime staging tree, explicit delete, expiry, startup sweep, and content-free receipts.
- host-bundle canonical sources, every host/package rendered tree, component archives, signed manifests/SBOMs/licenses, marketplace staging/download/index artifacts, install/update/repair/uninstall owned-config diffs and backups, hook stdin/output, capability probes, doctor/conformance diagnostics, crash staging, and content-free receipts.

Assertions:

- zero plaintext/candidate digest bytes in every forbidden sink.
- marker/receipt/class/coverage correct.
- no secret equality leak across privacy domains.
- detector timeout/crash yields blocked/unknown, never stored plaintext.
- deletion/remediation rebuild removes every descendant and old serving generation.
- backup/restore cannot resurrect the canary.
- the nine replaced credential-shaped legacy occurrences across the six exact PR 2B paths remain scanner-clean reserved/invalid canaries while still exercising every intended detector branch.
- Hermes projection-only, hook analytics duplicate-command, bounded MCP failure, post-model summary, response-handle writer, LCM backup copy, dashboard raw/host, memory metadata/V11 vector, and status-inference regressions each fail independently when its firewall is disabled.

### 17.4 Quality metrics

- Per-rule/class precision, recall, false-positive/false-negative count on frozen corpus.
- High-confidence miss rate must be zero on required classes.
- Structured span/replacement accuracy and raw-fallback coverage.
- Adjudicator agreement and allowlist expiry regression.
- p50/p95/p99 latency, throughput, CPU, allocation, memory, decode amplification.
- End-to-end forbidden-sink leakage count: zero.
- Full audit coverage and unknown/locked/skipped count: zero before cutover unless explicitly waived with non-serving quarantine.

No real secret is used to evaluate the system.

## 18. Primary research applied

- [Gitleaks](https://github.com/gitleaks/gitleaks): versioned regex/secret-group/entropy/allowlist/fingerprint/decode-depth concepts inform offline differential scanning; TraceDecay still owns typed runtime safety and privacy-safe result shapes.
- [detect-secrets design](https://github.com/Yelp/detect-secrets/blob/master/docs/design.md): source transforms/location preservation and audit workflow reinforce parse-before-scan and reviewable findings.
- [detect-secrets plugin warning](https://github.com/Yelp/detect-secrets/blob/master/docs/plugins.md): custom plugins execute code, so TraceDecay uses constrained WASM/subprocess isolation rather than arbitrary in-process imports.
- [GitHub secret-scanning scope](https://docs.github.com/en/code-security/reference/secret-security/secret-scanning-scope): patterns, non-provider pairs, validity, history, and scope motivate detector classes and whole-history/store coverage.
- [GitHub custom patterns](https://docs.github.com/code-security/secret-scanning/using-advanced-secret-scanning-and-push-protection-features/custom-patterns/): dry-run rule evolution and rescanning motivate versioned profiles and shadow diffs.
- [GitHub remediation](https://docs.github.com/en/enterprise-server%403.17/code-security/secret-scanning/working-with-secret-scanning-and-push-protection/remediating-a-leaked-secret): revoke/rotate before purge is the product remediation order.
- [Google Sensitive Data Protection pseudonymization](https://docs.cloud.google.com/sensitive-data-protection/docs/pseudonymization): keyed pseudonymization supports domain-scoped correlation without an unkeyed dictionary target.
- [OWASP Secrets Management Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html): lifecycle, rotation, least privilege, encryption, and audit shape quarantine/remediation.
- [OWASP Logging Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html): secrets must not reach logs/events; archives/backups and sanitization remain part of the boundary.
- [NIST SP 800-57 Part 2 Rev. 1](https://csrc.nist.gov/pubs/sp/800/57/pt2/r1/final): key-management policy and cryptographic lifecycle shape quarantine key separation/rotation/destruction.
- [Rust `regex` crate](https://docs.rs/regex/latest/regex/index.html): bounded linear-time matching helps runtime safety, but field/pattern/decoded-size limits remain mandatory.

## 19. Implementation ownership and files

| Owner | Files/modules |
|---|---|
| Domain | `crates/tracedecay-domain/src/privacy.rs`, sensitivity/receipt/error schemas, taint-state compile-fail tests. |
| Capture | `crates/tracedecay-capture/src/privacy/**`, provider structured field maps, runtime scanner, markers, receipts, quarantine decisions. |
| Store | `crates/tracedecay-store/src/{quarantine,privacy_scan,privacy_manifest,key_service,secure_retire}.rs`; no plaintext content API. |
| Projectors | `crates/tracedecay-projectors/src/privacy.rs` sink firewall and sanitized descendant/rebuild graph. |
| Query | `crates/tracedecay-query/src/privacy.rs` authorization/redaction/coverage and forbidden-sink canary inspection. |
| Policy/hooks/catalog | Privacy capability metadata, non-disableable floor digest, hook fail-closed behavior, detector/plugin inventory. |
| Application | `crates/tracedecay-application/src/privacy/**` status/scan/finding/remediation/verify/quarantine workflows. |
| API/SDK | Official schemas/routes/types/errors/jobs/SSE; generated conformance and redacted debug/display. |
| Root/CLI/doctor | `privacy` commands, lifecycle leases, migration/restore gate, gitleaks/CI/release integration, service/keyring ownership. |
| Dashboard | Privacy Observatory and synthetic Secret Safety Lab. |

The existing `src/sessions/lcm/raw.rs` redactors and `src/memory/hygiene.rs` detectors become V1 fixture/reference adapters, not two competing V2 implementations.

## 19.1 Bounded delivery clarification for PR 2B — reviewed amendment v1

This amendment clarifies the bounded PR 2B delivery scope above; it neither changes PR 2B's product meaning nor records implementation completion. PR 2B is evidence-, fixture-, test-, scanner-config-, and CI-only work. It owns no V2 runtime, crate, database, detector engine, public API, workflow runtime, or Claude workflow JavaScript. The only existing `src/**` edits authorized here are the literal scanner-safe fixture replacements listed below; subsequent runtime work remains owned by PR 4B/6B/7A/10A and their companion plans.

**Exact owned paths**

- Four separate corpus producers own one path each: `tests/fixtures/v2/privacy/positive-invalid.json`, `tests/fixtures/v2/privacy/negative-realistic.json`, `tests/fixtures/v2/privacy/serialized-field-boundary.json`, and `tests/fixtures/v2/privacy/forbidden-sink-canaries.json`.
- Sink-inventory producer owns only `tests/fixtures/v2/privacy/v1-v2-sink-inventory.json`.
- Historical-regression producer owns only `tests/fixtures/v2/privacy/historical-regressions.json`.
- Privacy-manifest producer owns only `tests/fixtures/v2/privacy/privacy-manifest.schema.json` and `tests/fixtures/v2/privacy/privacy-manifest.json`. The schema rejects unknown fields and requires stable surface/fixture/occurrence IDs, relative paths, content digests, source classes, detector/rule versions, coverage state, receipt references, and dependency edges; neither manifest may contain candidate content.
- Seven separate host-lane producers own one path each: `tests/fixtures/v2/privacy/host-canonical-sources.json`, `tests/fixtures/v2/privacy/host-rendered-trees.json`, `tests/fixtures/v2/privacy/host-component-archives.json`, `tests/fixtures/v2/privacy/host-marketplace-artifacts.json`, `tests/fixtures/v2/privacy/host-owned-config-backups.json`, `tests/fixtures/v2/privacy/host-hook-stdin.json`, and `tests/fixtures/v2/privacy/host-probe-diagnostics.json`.
- Gitleaks producer owns only `.gitleaks.toml` and `scripts/check-v2-gitleaks.sh`, which emits the content-free build artifact `target/v2-privacy/receipts/gitleaks-8.30.1.json`; the executable and receipt must identify exactly `gitleaks 8.30.1`.
- Differential-scanner producer owns only `.secrets.baseline` and `scripts/check-v2-detect-secrets.sh`, which emits the content-free build artifact `target/v2-privacy/receipts/detect-secrets-1.5.0.json`; the executable and receipt must identify exactly `detect-secrets 1.5.0`.
- Test producer owns only `tests/v2_corpus_suite/privacy.rs`.
- The final integration producer may edit only `tests/v2_corpus_suite/main.rs`, `tests/fixtures/v2/manifest.json`, and `.github/workflows/ci.yml`. These are registration/wiring edits only; it may not absorb the privacy test module, corpus, scanner, fixture-replacement, or runtime implementation.

Only the privacy-manifest producer defines the PR 2B manifest schema and child manifest, including the two exact scanner-receipt artifact contracts. Scanner producers write those build receipts but cannot change either manifest. Only the integration producer registers the already-reviewed child manifest in `tests/fixtures/v2/manifest.json` and publishes the receipts as CI artifacts; it cannot reinterpret or regenerate their semantic contents. Receipts stay outside the scanned committed fixture tree to avoid self-reference through their candidate commit and artifact digest.

The nine legacy scanner findings are nine **occurrences**, not nine paths. They are split into six mutually exclusive file-owner micro-items:

1. `src/dashboard/memory_analysis.rs` — one occurrence.
2. `src/hooks/memory_inject.rs` — one occurrence.
3. `src/memory/hygiene.rs` — two occurrences.
4. `tests/agent_suite/memory_digest_test.rs` — one occurrence.
5. `tests/memory_suite/memory_test.rs` — one occurrence.
6. `tests/session_suite/lcm_payload.rs` — three occurrences.

The immutable occurrence IDs below remain stable if line numbers move. The line is a reviewed candidate-tree anchor, the symbol is the semantic relocation anchor, and the scanner rule ID is part of the acceptance receipt:

| Stable ID | Reviewed path:line | Symbol anchor | Detector rule ID |
|---|---|---|---|
| `PR2B-LEGACY-001` | `src/dashboard/memory_analysis.rs:784` | `propose_hygiene_candidates_flags_secret_transient_and_supersession_for_review` | `generic-api-key` |
| `PR2B-LEGACY-002` | `src/hooks/memory_inject.rs:898` | `secret_like_facts_are_never_selected` | `generic-api-key` |
| `PR2B-LEGACY-003` | `src/memory/hygiene.rs:161` | `detects_pem_blocks_and_bearer_tokens` | `private-key` |
| `PR2B-LEGACY-004` | `src/memory/hygiene.rs:175` | `detects_known_prefixes_and_credentialish_assignments` | `generic-api-key` |
| `PR2B-LEGACY-005` | `tests/agent_suite/memory_digest_test.rs:72` | `selection_excludes_secret_like_and_injection_like_content` | `generic-api-key` |
| `PR2B-LEGACY-006` | `tests/memory_suite/memory_test.rs:2264` | `add_fact_rejects_secret_like_content_without_storing` | `generic-api-key` |
| `PR2B-LEGACY-007` | `tests/session_suite/lcm_payload.rs:586` | `api_alias_assignments_redact_apikey_and_apitoken` | `generic-api-key` |
| `PR2B-LEGACY-008` | `tests/session_suite/lcm_payload.rs:637` | `private_key_redaction_is_lossy_and_not_indexed_when_enabled` | `private-key` |
| `PR2B-LEGACY-009` | `tests/session_suite/lcm_payload.rs:1266` | `redaction_applies_before_whole_message_externalization` | `generic-api-key` |

Each file owner may replace only those credential literals with reserved/invalid scanner-safe canaries and adjust only directly adjacent expectations needed to preserve the existing detector branch. This is not ownership of those modules' behavior. Findings in `.codex/skills/**`, `.claude/**`, workflow scripts, generated output, or any other path are reported separately and are not silently folded into this nine-occurrence correction.

**Exact tests and gates**

`tests/v2_corpus_suite/privacy.rs` defines exactly these PR 2B tests:

- `privacy_manifest_is_complete_and_hashes_are_deterministic`
- `privacy_positive_invalid_corpus_covers_required_classes`
- `privacy_negative_corpus_has_no_builtin_findings`
- `privacy_serialized_fields_are_scanned_independently`
- `privacy_sink_inventory_covers_v1_v2_and_forbidden_sinks`
- `privacy_historical_regressions_are_anchored`
- `privacy_host_surfaces_have_independent_receipts`
- `privacy_legacy_fixture_replacements_preserve_detector_coverage`
- `privacy_scanner_receipts_pin_gitleaks_8_30_1_and_detect_secrets_1_5_0`
- `privacy_repository_and_generated_derivatives_are_zero_finding`

CI runs `cargo test --test v2_corpus_suite privacy`, `scripts/check-v2-gitleaks.sh`, and `scripts/check-v2-detect-secrets.sh`. Each scanner lane emits its exact content-free receipt named above, containing tool version, config digest, reviewed base commit, candidate commit, scanned-surface stable IDs, coverage state, finding count, and artifact digest; it emits no candidate value, snippet, candidate fingerprint, or raw path containing private data. Detect-secrets independently scans the same committed/generated surface inventory. A missing executable, version mismatch, skipped/locked/unsupported surface, stale baseline, nonzero candidate finding count, or absent receipt fails the gate; neither scanner substitutes for the other.

The Gitleaks PR gate accepts two explicit immutable revisions, `reviewed_base` and `candidate`, verifies both exist and `reviewed_base` is an ancestor of `candidate`, scans the complete candidate working tree/generated derivatives, and scans Git objects only for the reviewed `reviewed_base..candidate` range. It must not infer a base from the current branch name, mutable remote default, merge-base drift, shallow-clone boundary, or prior receipt. The receipt binds both full commit IDs and reports incomplete history as a failed/unknown gate.

Historical coverage is a separate scheduled and release gate: it scans every reachable commit from the release roots under the same pinned policy and publishes a content-free coverage receipt. A historical finding is tracked by stable finding ID, commit, relative path, detector rule/version, and remediation state. It is fixed forward, rotated/revoked when applicable, and explicitly adjudicated without candidate bytes. History is not rewritten by PR 2B, and repository-, path-, rule-, or history-wide blanket allowlists are prohibited. A narrowly adjudicated historical occurrence never suppresses a candidate-tree finding, a new commit, another path, or another detector version.

**Bounded multi-agent route and dependency DAG**

All items run in `/fast/projects/tracedecay/.worktrees/codex-tracedecay-total-redesign-plan` through the canonical plan-execution route and shared PR 2B ledger. Every producer receives one exact path set, acceptance tests, and an independent durable receipt. No producer receives an entire PR, multiple corpus or host lanes, a corpus-plus-tests bundle, or open-ended discovery. The four corpus, sink-inventory, historical-regression, privacy-manifest, seven host, two scanner, and six legacy-file producers run independently and may run in parallel. The test producer starts only after all fixture-data and manifest producers publish accepted receipts. The integration producer starts only after the fixture-data, manifest, scanner, six file-owner, and test receipts are accepted. A separate reviewer then validates the aggregate diff and gates; corrections return as new bounded owner-specific micro-items rather than one aggregate fix task.

```text
corpus lanes (4) ─────┐
sink inventory ──────┼──> privacy tests ──┐
historical anchors ──┤                    │
privacy manifest ─────┤                    │
host lanes (7) ──────┘                    │
gitleaks lane ────────────────────────────┤
detect-secrets lane ──────────────────────┼──> integration wiring ──> independent review
six file owners (parallel) ───────────────┘
```

The route uses explicit cancellation, progress/heartbeat visibility, resumable checkpoints, and receipt-based restart. It imposes no automatic wall-clock, per-agent, workflow, or no-progress timeout. Cancellation never converts incomplete work into success. No workflow `.js` file, Claude workflow implementation, or task-executor runtime is modified by PR 2B. Completion requires every named receipt, exact test, scanner lane, and independent review to pass on the aggregate tree; a planning amendment or individual producer success is not completion.

## 20. Reviewable PR sequence

Integrate these slices into the master Phase 0–5 sequence.

### PR 2B — Secret corpus, sink inventory, and scanner receipts

- Create invalid synthetic positive, realistic negative, serialized-envelope false-positive, and forbidden-sink canary corpora.
- Generate complete V1/V2 sink inventory and privacy manifest schemas.
- Pin `gitleaks` CI/offline scan and a second differential detector; record scanner versions and zero-findings artifacts without candidate content.
- Import historical session anchors and current LCM/memory/remote/tool-preview tests.
- Replace all nine credential-shaped repository fixtures with reserved/invalid scanner-safe canaries and freeze their detector-coverage equivalence plus zero-findings repository scan.
- Inventory and scan plan 27's exact canonical host-bundle sources, all deterministic rendered host/package trees, component archives, marketplace candidates/indexes, owned-config diff/backup lane, hook stdin lane, and probe/doctor/conformance diagnostic lane independently; store only safe manifests and protected receipt refs.

The companion bounded-delivery clarification is §19.1; it adds no product acceptance criterion.

### PR 4B — Privacy domain and taint-state contracts

- Add sensitivity/detection/receipt/policy/fingerprint/marker/coverage/finding/remediation types.
- Add `Unclassified` -> `Classified` -> `Sanitized` -> sink-eligible conversions and compile-fail architecture tests.
- Remove secret lengths/unkeyed content digests from public marker contracts.
- Freeze one golden `SanitizationReceiptV1` schema/canonical encoding; round-trip every field through plan 02 and reject findings-total mismatch, expired/revoked eligibility, cross-observation supersession, forks, and cycles.

### Companion requirements for PR 6B — Sanitized blob storage, protected quarantine, and key service

- Add isolated random-ID encrypted blobs, per-record DEK wrapping, OS-keyring profile KEK, private I/O, TTL/holds, access audit, cryptographic deletion, and recovery tests.
- Prove unavailable keyring fails to sanitized-only/drop without plaintext fallback.
- Kill-test `Staged -> Attached -> Held -> Attached -> Retiring -> Retired`, unattached expiry, hold expiry after the original deadline, repeated release/retire, and crashes before/after journal append, key destruction, and unlink; attached data must retire unless an active hold exists and no terminal object may be revived.

### Companion requirements for PR 7A — Mandatory structured sanitizer and provider conformance

- Implement parse-before-scan engine, built-ins, bounded decoding, span merge/replacement, policy precedence, receipts, and fail-closed budgets.
- Wrap providers/hooks one at a time in shadow/differential mode, then make sanitized observation the only journal input.
- Retire message-metadata opt-out; source metadata may only strengthen policy.
- Route Hermes/legacy projection-only ingest, post-model summaries, response-handle payload preparation, and bounded transport-error detail through the same taint-state boundary.

### Companion requirements for PR 10A — Projector sink firewalls and descendant lineage

- Require sink-eligible types for session/FTS/vector/code/knowledge/policy/automation/analytics/cache projectors.
- Record sanitized descendants so one finding can preview/block/rebuild every derivative.
- Remove secret candidate previews from memory curation and equivalent inspection paths.
- Reject direct V11 fact/vector inserts, raw memory tag/entity/source/metadata projection, duplicate-command hook analytics, and content-bearing backup/response-handle filesystem writes at architecture and sink-canary gates.

### Companion requirements for PR 12C — Privacy-aware query and global containment

- Enforce authorization, safe markers, blocked/redacted/unknown coverage, no content fingerprints, cache invalidation, and cross-shard containment.
- Prove an unsafe shard/entity cannot leak through search, graph expansion, aggregation, ranking explanation, cursor, or exact-load routing.

### PR 22B — Privacy observability and safe metrics

- Project coverage/findings/state/performance/remediation aggregates with minimum thresholds and no values/fingerprints.
- Add privacy doctor predicates shared with actual remediation commands.

### PR 24H — Privacy application/API/CLI/MCP/SDK workflows

- Ship status/scan/findings/remediation/verify/detector/quarantine use cases and official contracts.
- Direct-agent credentials remain read-only and cannot access quarantine plaintext.
- Run whole-transport secret-canary conformance.
- Add the edit-bundle archive/path/link/inode/permission/size/TTL/crash matrix; prove ordinary validation retention is repairable while secret/unknown/containment failure and successful submit purge bytes immediately, and prove every durable receipt is content/path-free.
- Replace lossy-row-derived “enabled” status with one generated `PrivacyProtectionStatusV1` reporting policy/effective-floor/source/sink/detector/legacy/last-scan evidence; add authenticated-loopback dashboard and safe bounded-error conformance.

### PR 31M — Privacy Observatory and Secret Safety Lab

- Ship coverage/repair views and synthetic detector/policy comparison without live mutation or real finding values.

### PR 33A — Retroactive V1/V2 audit, containment, rebuild, and restore gate

- Scan every named sink/store/artifact/backup with complete coverage manifests.
- Include both #425 split-store source families, canonical-path aliases, WAL/SHM/temp files, backups, consolidation ledger/staging/table/collision reports, remapped LCM source edges, doctor commands/errors, and candidate rebuilt store as separate coverage rows; neither family inherits the other’s clean status.
- Include installed/generated host-package trees, downloaded marketplace artifacts, owned config/current/backup generations, hook spool/runtime remnants, probe/doctor/conformance workspaces, and legacy host-installer fragments as separate coverage rows. Config and backup bodies stay in the protected rollback/quarantine domain; foreign caches/config and unproven ownership remain preserved and non-serving rather than copied into a general scan store.
- Block flagged descendants, guide rotation, rebuild sanitized generations, retire old WAL/DB/cache/export artifacts, rescan, and issue verification receipts.
- Cutover requires zero forbidden-sink canary hits and zero unexplained serving unknowns.

## 21. Cutover and rollback

- Runtime sanitizer ships in shadow mode only against copied/private test stores; shadow findings never expose candidate values.
- Mandatory sanitizer enables for one provider/domain at a time after precision/latency/coverage gates.
- Old V1 stores remain non-serving/read-only rollback evidence and are never queried alongside sanitized V2 results.
- Rollback restores the previous V2 code/policy only if it still enforces the non-disableable floor; it cannot restore plaintext projection behavior.
- Detector false-positive regression may change classification/marker/projection after rescan, but never rehydrate a deleted secret from V1 source automatically.
- After whole-profile audit/remediation, any backup or old store without a clean current manifest remains quarantined/non-restorable.

## 22. Verification commands and artifacts

Planned implementation checks:

```sh
cargo test -p tracedecay-domain privacy
cargo test -p tracedecay-capture privacy -- --test-threads=1
cargo test -p tracedecay-store privacy quarantine restore
cargo test -p tracedecay-projectors privacy sink_firewall
cargo test -p tracedecay-query privacy containment
cargo test -p tracedecay-application privacy
cargo test privacy public_api_conformance
cargo nextest run --workspace --no-fail-fast
gitleaks git --redact --no-banner
gitleaks dir dashboard packages python docs tests --redact --max-archive-depth 2
```

Artifacts:

- secret corpus manifest with only synthetic/redacted fixture hashes.
- detector/policy/source/sink coverage matrix.
- per-class precision/recall and false-positive adjudication report.
- latency/resource/regex/decode/plugin stress report.
- forbidden-sink canary report.
- full-profile scan manifest and safe finding counts.
- remediation descendant/rebuild/retirement/rotation acknowledgement receipt.
- backup/restore eligibility manifest.
- generated API/SDK/frontend/release scan receipt.
- host-bundle source/rendered-tree/marketplace/config-backup/hook-input/probe-diagnostic coverage and release scan receipt, containing digests/counts/states only.

Reports never contain candidate values, raw snippets, or secret fingerprints.

## 23. Definition of done

- [ ] One mandatory versioned sanitizer replaces fragmented provider/LCM/memory/output-specific behavior.
- [ ] Redaction is secure by default; no message/source metadata can disable the safety floor.
- [ ] Structured fields are parsed/scanned independently; serialized-envelope cross-field false positives are a frozen regression.
- [ ] Every input and forbidden sink is enumerated in a generated capability/privacy inventory.
- [ ] Secret plaintext cannot compile through a store/projector/application/transport sink without an eligible wrapper.
- [ ] Public markers reveal no secret length, prefix/suffix, unkeyed hash, or cross-domain equality.
- [ ] Optional raw retention is encrypted, isolated, private, audited, short-lived, and cryptographically deletable.
- [ ] `SanitizationReceiptV1` has one exact domain/schema lowering with byte-stable round trips and enforceable expiry/revocation/supersession.
- [ ] Attached quarantine objects retire through the append-only hold/release/retirement state machine; crash recovery cannot leak, revive, or strand them beyond an active hold.
- [ ] Runtime, offline, CI, release, fixture, export, backup, and restore scanners have explicit complementary roles.
- [ ] Host-bundle source/rendered/marketplace artifacts are scanned independently; config/backup bodies and raw hook/probe/diagnostic payloads remain protected-operation data only, and foreign host state is preserved.
- [ ] Detector plugins cannot access filesystem/network or emit content and fail closed on timeout/crash.
- [ ] Facts/memory/skills/automation/hints/coordination/analytics never receive candidate plaintext.
- [ ] Code/Git/diagnostic/tool/session/LCM content uses the same boundary and retains source provenance.
- [ ] False-positive decisions are keyed/scoped/versioned/expiring and contain no candidate.
- [ ] A finding immediately blocks descendants; rotation precedes sanitized rebuild/purge verification.
- [ ] SQLite/WAL/temp/cache/vector/summary/graph/export/backup descendants are rebuilt/retired, not assumed clean after row deletion.
- [ ] Every restore is isolated, migrated, scanned, rebuilt, and receipt-gated before serving.
- [ ] Split-store consolidation preserves two independently verified backups, exposes complete privacy coverage for every source/derived artifact, invalidates confirmation on detector/manifest drift, and cannot publish marker/registry state before both restore probes and the candidate store pass.
- [ ] Privacy Observatory reports complete/unknown coverage and remediation state without previews.
- [ ] Secret Safety Lab uses synthetic values only and cannot mutate live policy/findings/analytics.
- [ ] Real local stores are never copied to fixtures; committed corpus and plan set pass pinned secret scans.
- [ ] End-to-end synthetic canaries produce zero plaintext bytes across every forbidden sink and every transport/package.
- [ ] No stale live client, V1 adapter, or rollback path bypasses the current privacy boundary.
