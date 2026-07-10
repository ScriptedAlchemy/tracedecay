# TraceDecay V2 Historical Failure and Regression Matrix

> **Purpose:** Turn problems repeatedly experienced by the user and agents into architecture invariants, owned tests, product states, and cutover blockers. This is not a changelog and not a promise that every old implementation detail survives.

**Evidence sources:** chronological private user-message corpus and manifest; parent planning session `019f4906-a411-7a11-ad3f-0d58deb0e847`; anchored child research in `13-research-provenance-and-context-anchors.md`; live/open PR state; merged PR history since 2026-06-28; TraceDecay doctor/analytics/LCM/Git/code-health snapshots.

## 1. Classification rules

- One issue may belong to multiple rows. Counts from keyword/theme mining are navigation aids, not prevalence statistics.
- A merged fix is a regression fixture, not proof that the architectural failure class is gone.
- User correction outranks inferred intent. The “no stale-client compatibility fallback” correction is a hard boundary.
- A missing session/workflow correlation is coverage evidence, not proof that no work happened.
- A test retry is not a root-cause fix. A green isolated test does not erase an order-dependent suite failure.
- Every row names a prevention owner, a detection surface, recovery behavior, and a release/cutover gate.

## 2. Storage, identity, and durability

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| Disk full leaves active graph DB as non-SQLite bytes | Human message search recipe `disk fills graph database non-SQLite garbage`; PR #406 | store/capture; staged graph generations; lifecycle coordinator | Observatory shows exact corrupt family, disk preflight, last good manifest, quarantine, recovery-set path/digest; healthy shards remain usable | Kill/disk-full at every staged write/rename/WAL boundary; never replace last good generation; whole recovery family preserved |
| Doctor or updater opens DB while daemon/background writer owns WAL | PR #370, merged #412 | root lifecycle + store writer lease | Typed `unsafe_live_writer`, owner/process/service state, drain progress, checkpoint receipt | Concurrent doctor/update/backup/retention/migration tests; no env bypass; checkpoint only after all owned writers stop |
| MCP resolves/opens local stores before choosing a reachable managed daemon, or replays an uncertain write after reconnect | Open #420 | root composition + lifecycle/client routing | Authority/proxy decision precedes store-open; per-request connection/reconnect state and typed new-session/tools-list guidance | Reachable/rebound/replaced/disconnected daemon, explicit project, config-gated init, read/write disconnect matrix; zero local side effects before proxy and zero write replay |
| Update restarts a deliberately stopped/disabled/masked service | PR #412 | root service manager | Before/after systemd/launchd state and compensation receipt | Matrix for running/stopped/enabled/disabled/masked/unmanaged reachable daemon |
| Corrupt recovery artifacts overwritten or treated disposable | PR #406; durable graph-resident memory correction | store/root migration | Quarantine/recovery-set manifest; no automatic delete | Backup/restore plus facts/entities/graph verification before any graph-store retirement |
| Repo moves, linked worktrees, renamed checkouts split identity | PR #269, #371, merged #405 | domain/store identity | Alias/adoption candidates, conflict relation, canonical repository inspector | Moved/symlinked/linked/detached/remote-changed fixtures; nonempty ambiguity blocks cutover |
| Branch-per-DB multiplies nearly identical 140–150 MB stores | `branch_list` inventory | store graph generations | Storage topology/compaction visualization | Pack/overlay benchmark at current and 10x; bounded files/open generations; snapshot identity parity |
| Private append/lock created with unsafe mode or races | PR #323, #328, #399 | capture/store | Permission/lock doctor with exact owner/mode | First-syscall `0600`/`0700`, Windows/Linux/macOS lock and process-death suite |
| Config saves or sidecars tear on crash | PR #337 and JSONL/ledger fixes | root/store | Recovery receipt and last-good version | Atomic staged write/fsync/rename/dir-fsync kill matrix |
| Semantic code edit follows a symlink escape, aliases source/destination through hard links, loses a concurrent edit, or rollback clobbers newer bytes | Merged #414/#419 | application command + root filesystem adapter | Preview names exact identities/versions/same-file evidence; apply/rollback receipt reports revalidation/conflict | Symlink/hard-link/cross-platform same-file, dual-file race, atomic sibling-rename, and concurrent rollback-edit fixtures; no overwrite on mismatch |

## 3. Capture, sessions, LCM, and agent structure

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| One parser-version bump reprocesses every provider | PR #387; search recipe in anchor plan | capture/store | Per-source/provider/parser checkpoint/lag | Change Claude parser only; prove Codex/Cursor offsets and rows unchanged |
| Structured backfill races across processes | PR #374 | capture/store/root | Lease owner/epoch, checkpoint, duplicate/gap report | Competing processes, stale lease, crash/takeover, idempotent replay |
| Global process flag makes libtest suite order-dependent | current `structured_backfill_one_shot_process_never_spawns` suite failure; isolated pass | root/test architecture | Test diagnostics distinguishes isolation assumption from product failure | Remove/reset process-global mutable state or declare nextest-only binary; full suite deterministic in repeated shuffled order |
| Provider tools/reasoning/goals/custom events missing | PR #325/#346/#348/#350/#352/#372/#382/#383 | capture/projectors | Provider coverage matrix with unsupported/unknown/malformed states | Golden fixture per provider/event family; source offsets/hashes; forward fields retained |
| Claude reasoning duplicated; parent prompts copied into children | PR #384, #410; current child coordination duplication | capture/projectors/query | Sanitized-native plus representative/human/direct-user/subagent/protocol modes, hidden counts, evidence | Eight-child prompt case; native expansion exact; representative classifier versioned; no ingest deletion |
| Child task/session attribution missing or copied into wrong children | Current planning child `parent_tool_use_id: null`; copied coordination records | domain/projectors/research anchors | Agent graph shows asserted/candidate/unresolved parent and task ownership | Provider-declared parent/tool fixtures; copied text cannot establish authorship; unresolved remains visible |
| Session project/worktree/branch context lost | PR #230/#233/#239/#269 | capture/projectors | Session spans with source/occurred/ingested evidence | CWD/ref changes inside session, generic zero-project chat, multi-project session, renamed checkout |
| Produced commit confused with observed/overlap | PR #369/#376 | projectors/query/policy | Produced/observed/encountered labels and evidence inspector | Direct producer, later checkout, reflog overlap, cherry-pick/rebase/force-push calibration corpus |
| LCM provider default/filter silently excludes evidence | PR #242 | query/application | Scope/provider/coverage always visible | Omitted provider means all; explicit provider stays scoped; cross-provider order and counts fixed |
| LCM/search cannot enumerate all rows and caps per session | PR #375; redesign export algorithm | query/API | Stable list-all cursor, cap/truncation/hidden counts and export manifest | Match-all list sessions/messages; live snapshot completeness; exact-second cap and pagination property tests |
| Message search ranks inventory/tool noise above intent | PR #358/#361 | query/policy | Rank explanation, result kind/origin filters, noise feature | Labeled intent corpus; inventory/paraphrase/fence/branch over-match regressions |
| Mixed timestamp units reorder activity | PR #234 | domain/projectors/query | Occurred/ingested/raw timestamp and missing-time reason | Unit normalization goldens, late events, half-open windows, deterministic ties |
| Payload missing/orphan/truncated without recovery handle | LCM payload/GC history; response-handle truncations | store/query/API | Payload integrity/coverage, stable cursor/export/anchor | Missing/orphan/tombstone/retention/GC fixtures; expiring response handle never sole citation |

### 3.1 Cross-project, repository, worktree, and store routing

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| Multi-token project search reports registered Rsbuild/Rspack repositories missing | `session:019f42c9-623a-7cc0-95c1-f073eaa05a4d`; user correction `session:019f4323-f569-74c0-9988-ea3851d14fd7`; root cause `session:019f4325-57ef-7a53-b6a0-5c583c759301` | domain/catalog/application | Resolver channel/score explanation, candidates, explicit current scope, one-step retry | Exact/quoted/token/fuzzy/alias/remote/path/relationship matrix; `rsbuild rspack` cannot be treated as one contiguous substring |
| Registry/list cap is mistaken for complete inventory | Same Rsbuild/Rspack session; capped 25-row list | catalog/query/API | `searched`, `returned`, `next_cursor`, `truncated`, total-known/unknown, omitted reason | 10k registry corpus; every transport paginates before rendering and never claims absence from first page |
| All-project message search returns a session that exact-load cannot route | Current supported replay: `message_search(all_registered)` result; `lcm_load_session` rejects project selector | catalog/activity/query/application/API | Location-independent retrieval ref, owning shard, retained tombstone/redirect | Search -> exact session/message/Turn -> adjacent context -> source observation succeeds across every authorized shard without CWD switch |
| Provider `sessions.project_key` or per-project copies become canonical activity boundary | `session:019f2538-0fd9-7362-a50b-96e36130643b`; Hermes/project isolation observations | activity/projectors/query | Profile activity canonical; project attribution relation and role per event/Turn | Zero/one/many-project sessions, provider-key collision, cross-project Hermes, copied rows; no duplicate transcript authority |
| First/initial CWD misattributes later Turns and Claude activity across worktrees | `session:019f2524-534d-7bd1-a3b1-675f242dcc0e` | capture/activity/projectors | Per-observation CWD/tool-workdir/worktree/ref interval and confidence | Session moves A -> B; parent in A/child in B; tool explicitly queries C; no session-wide first-CWD overwrite |
| Active MCP/base checkout graph or PR context is used for a different worktree/branch | `session:019f3edc-6a4e-7d80-b181-8f6d1e657859`; parallel-worktree issue history | domain/catalog/query/Git application | Resolved worktree/ref/head/snapshot, dirty/base/index drift, explicit-target refusal | Main + feature worktree, same branch/different head, detached/dirty/deleted branch; no silent active/base fallback |
| Missing local code index suppresses healthy sessions, memory, Git, or registry capabilities | `session:019f1204-5575-72a1-a2d1-ab5c6d1b310d` | catalog/application/policy/hooks | Per-domain capability/health/coverage rather than one project-health boolean | No-graph project still exposes profile activity/facts/Git/registry; hints name only unavailable domain |
| Registered selector/alias fails and forces manual project-list choreography | Research selectors for `lcm`, `disqmcp`, `browser-linux`; project-search history | catalog/application/API | Typed `ScopeNotFound`/`ScopeAmbiguous`, candidates and executable retry selector | Stable ID/name/path/alias/remote/worktree/PR forms share one resolver across CLI/MCP/API/SDK |
| Registry pollution, missing paths, and duplicate/stale stores cause wrong route or misleading doctor status | Historical ~12,852 project directories/29 stale findings; renamed tokensave -> tracedecay store history | catalog/store/root doctor | Reconciliation state, adoption/conflict receipts, previewable GC, exact healthy/unavailable domains | Moved/deleted checkout, duplicate legacy store, stale row, path reuse, symlink/case/mount, corrupt one shard; no newest-mtime guess or destructive GC |
| Cross-repository evidence loses source class or silently falls back to installed package code | `session:019efb4d-4508-7182-961b-9b30c739baa7`; Rspack/Rsbuild/React Router family | query/application/UI | Per-result repository/snapshot/source/fallback/evidence class and related-scope suggestion | Plugin/upstream/bundler/framework/benchmark corpus; local package, registered graph, live Git, and inferred impact remain distinguishable |
| Same/copied investigation across stores/sessions inflates confidence and eval counts | `session:019f1568-f9de-75c1-9870-7cee46944adc` and copied workflow descendants | query/eval/projectors | Canonical investigation/representative cluster, hidden copies, native expansion | Cross-store/session/provider copies dedupe for ranking/metrics while sanitized native evidence remains complete and provider raw source stays locatable under privacy policy |

### 3.2 Secret detection, redaction, and private-data safety

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| LCM redaction exists but defaults off and production providers never enable it | `src/sessions/lcm/raw.rs:748-853`; `session:agent-a0142b3f24b97b5de`; `session:019ee5d9-6b70-7e81-b9d2-804c61fc4bea` | domain/capture/privacy policy | Configured/effective detector policy, per-source/sink coverage, sanitized/quarantined/legacy-unscanned/unknown counts | Mandatory non-disableable sanitizer; message metadata may strengthen only; every provider/hook synthetic canary yields zero forbidden-sink bytes |
| Provider/tool/session paths persist commands, content blocks, tool calls/results, reasoning, or metadata before a common boundary | Current audit: Codex/Claude/Hermes/session projection paths; historical Codex preview finding | capture/projectors | Sanitization receipt on every observation/content part and blocked incomplete scans | Cross-provider message/tool/goal/workflow/replay conformance; raw source remains provider-owned; no unsanitized journal/projection input |
| Hook/log/analytics/error path records full command or bounded-but-unscanned failure text | `src/hooks/analytics.rs:40-86,171-215`; `src/mcp/tool_analytics.rs:36-59` | hooks/application/observability | Safe event dimensions/counts only; log-safe type and scanner receipt | Shell/header/query/token canaries absent from JSONL/SQLite/log/trace/error/crash/analytics; timeout fails closed |
| Memory detector protects fact content only; legacy backfill or metadata/tags/entities/source bypass it | `src/memory/store.rs:80-117`; `src/db/migrations.rs:1223-1243,1407-1485` | capture/projectors/knowledge migration | Entire typed fact/entity/provenance object classified; legacy unsafe descendants blocked | Backfill scans before vector/FTS; every field canary; no embedding/entity extraction/trust/curation before sanitization |
| Secret-like memory curation candidate exposes the first 200 content characters | `src/dashboard/memory_analysis.rs:537-643` and `truncated_content:468-475` | application/API/frontend | Candidate shows safe class/receipt/reason only | Legacy secret fixture yields zero candidate bytes in dry run, prompt, API, UI, logs, and proposal artifacts |
| Redaction marker leaks candidate length and truncated unkeyed SHA-256 equality/dictionary oracle | `src/sessions/lcm/raw.rs::sensitive_placeholder` | domain/capture | Opaque random receipt marker; protected domain-keyed HMAC internal only | Short/repeated/cross-domain synthetic secrets reveal no length/hash/equality; no fingerprint in transport/telemetry |
| Response handles, external payloads, summaries, backups, WAL/temp, caches, or exports duplicate plaintext | `src/mcp/response_handles.rs:249-278`; `src/sessions/lcm/doctor.rs:982-1010`; current audit | store/projectors/application/root | Descendant lineage, protected quarantine, unsafe-generation containment, backup/restore eligibility | Whole sink canary matrix; remediation rebuilds databases/indexes/caches and retires WAL/backup/exports; restore cannot resurrect canary |
| Dashboard/API exposes raw content/metadata or permits non-loopback use without authentication | `src/dashboard/lcm_queries.rs:29-47`; `src/dashboard/mod.rs:414-500` | application/API/frontend/root | Loopback/socket auth, host/origin/CSRF policy, typed redacted/denied coverage | Raw-route/remote-bind/auth/CSRF/browser storage/source-map canaries; no unauthenticated payload hydration |
| Status says redaction enabled only after a lossy row exists | `src/sessions/lcm/query.rs:905-909,946` | application/observability | Separate configured/effective/coverage/findings/legacy/unknown state and detector/policy version | Enabled-with-zero-findings, disabled legacy, partial/locked/corrupt, stale-rule, and mixed-provider fixtures |
| Scanner runs across serialized envelopes and fabricates cross-field credential match | Current planning corpus parsed-value follow-up | capture/eval/privacy lab | Structured field path/span, raw fallback declared, no cross-record concatenation | Adjacent JSON-field adversarial fixture; parsed leaves produce no false match; malformed fallback remains bounded/explicit |
| Real stores/transcripts are promoted directly to fixtures or tests contain credential-shaped values | Current `gitleaks` audit: nine source/test shapes; private corpus policy | test/release/root | Synthetic/minimal-redacted fixture manifest and pinned scanner receipt | Staged diff/history/archive/generated/package scans zero; no DB/transcript/export copied; allow decisions safe/scoped/expiring |
| Detection is followed by deletion but not credential rotation/revocation | Secret-scanning remediation research | application/UI/docs | Rotation-first state/checklist, containment and descendant repair graph | UI/API never calls purge “fixed” before explicit rotation acknowledgement/unknown; no online validity call by default |

## 4. Hooks, hints, tools, and policy

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| Hint appears on nearly every prompt, repeats, or wastes context | Hook-audit sessions; PR #319 | policy/hooks | Candidate/reject/suppress/dedupe/cooldown/budget tree; exact payload | Real transcript replay, generic chat, unindexed project, repeated prompt, token/latency budgets |
| Relevant TraceDecay/Git tool is not suggested or used | Parent planning correction; Git-tool section in master | tool catalog/policy/hooks | `missed_capability`, `human_correction`, eligible/suggested/used/unavailable | Branch/worktree/PR prompts route to branch/pr/session/workflow + live refresh when required |
| Wrong trust signal routes false compiler/build hints | PR #401 | capture/policy | Evidence class/source/diagnostic mapping | Trusted compiler failure, untrusted pasted text, behavioral test failure, forged path/result fixtures |
| Installed hook/plugin hash or marketplace identity drifts | PR #258/#303/#331/#401 | root/catalog/hooks | Installed source/version/hash/trust status | Tampered manifest/binary, old marketplace identity, partial update, rollback |
| Tool exists under wrong/multiple namespace or unclear branding | PR #330/#344/#400 | tool catalog/root | Host bindings show one current TraceDecay identity | Codex/Claude/Cursor/Kiro manifest schemas and visible-name E2E |
| Huge catalog injection would become prompt spam | 102 current tool names | tool catalog/policy | Compact category route and discovery action | Never inject full catalog; relevant routing recall/precision and token budgets |
| Git semantic output labels transitive fan-out as modified | #410 `pr_context` 16 files vs ~2,866 “modified” symbols | query/catalog | Direct change, structural impact, candidate test, context-only sections with evidence/caps | Exact 16-file regression; direct counts cannot include fan-out |
| Local semantic Git and live GitHub disagree silently | Open PR audits | query/application/policy | Both heads/merge base/changed-file digest/fetched/indexed watermarks and reconciliation | Drift blocks joined conclusion; refresh/reindex/recompute paths |
| Hint outcome mostly unresolved or falsely attributed | Analytics: 1,182 emitted, three acted | policy/projectors/observability | Eligible/emitted/delivered/observed/acted/ignored/missed/corrected/unresolvable with horizon | Labeled adoption corpus, attribution guard, no numeric rate without denominator/horizon |

## 5. Memory, automation, skills, and self-improvement

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| Fact-store route opens non-database while doctor says healthy | Live audit | store/application/doctor | Exact profile/shard/path identity, integrity, locked/quarantined/partial state | Multi-store route fixtures; corrupt one shard; healthy others query; no ambiguous “healthy” |
| Long fact content mangled by entity extraction | PR #349 | capture/projectors/policy | Source slice, extractor/version, proposed entities | Long/Unicode/code/URL facts; exact content round trip; extraction never rewrites fact |
| Paraphrased proposals duplicate memory | PR #359 | policy/projectors | Duplicate/conflict candidates and evidence | Labeled semantic duplicate corpus; no auto-merge without threshold/evidence/approval policy |
| Self-improvement output is unsafe, weak, self-referential, or unvalidated | PR #295; skill-writer evidence history | policy/application/Evolution Studio | Evidence→proposal→validation→approval→apply→use→outcome→rollback graph | Secret/transient/provider-mismatch/self-machinery/weak-evidence/loadability/rollback cases |
| Automation transient errors stall or lie about autonomy | PR #338 | application/root/observability | Retry class, attempt, next retry, terminal outcome, autonomy policy | Idempotent retry/backoff, permanent rejection, process death, duplicate delivery |
| Doctor calls foreign skill an orphan and recommends impossible update | PR #411 | catalog/application/root doctor | Shared ownership predicate, info/warning/error, applicable command/precondition | Foreign/legacy/self-owned/global/project matrices; advertised action must execute or not be shown |
| Managed skill ownership/materialization can overwrite/remove foreign data | PR #366/#385/#411 | root/store/application | `materialized_by`, source digest, fork/foreign state, preview | Foreign/forked/legacy/missing manifest; no delete without ownership evidence |
| Profile-vs-project memory/skill/automation scope is misleading | Live audit and #407 | domain/store/projectors/UI | Declared scope/owner/privacy domain on every entity/status | Profile/zero-project/cross-project/project cases; no arbitrary primary project or Hermes silo |

## 6. Dashboard, API, observability, and remediation

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| Dashboard exposes only one project/plugin silo | Parent planning request | application/API/frontend | `/` All/Brain, explicit active profile, coverage matrix, saved scope | Multi-project corpus; partial shard; global→project→Turn→evidence user task |
| Information explorer lacks connections and powerful pivots | Parent request and frontend audit | query/application/frontend | Shared investigation state and coordinated graph/table/timeline/matrix/chart views | Every result pivots without query/selection drift; evidence semantics preserved |
| Empty/loading/stale/error state looks like “no data” | Existing plugins and live drift | API/frontend | Named loading/empty/stale/partial/offline/locked/redacted/incompatible/error | Fault injection for every state and each workspace; last-known-good plus watermark |
| API/renderer truncates without pagination or retrieval anchor | Project list and response-handle observations | query/API | Cursor before renderer cap; exact truncation; export/anchor | Oversize result at every layer; no lost structured rows; response handle optional only |
| Analytics reports zero denominator or capped sample as whole | Live analytics audit | projectors/query/UI | Population/horizon/cap/sampling/source watermark/unknown | Missing denominator renders unknown; drill to source events; no false percentage |
| Doctor severity/action disagrees with actual command | PR #316/#370/#411 | application/root/UI | Finding uses command precondition and owner; preview/action receipt | Shared predicate mutation tests and live E2E; no warning nag with impossible remediation |
| Dashboard host session catch-up/global state causes flaky tests | PR #394 | frontend/API/test harness | Explicit client/session/test store ownership | Parallel browser/API tests with isolated storage/session/auth/subscription resources |

## 7. Host, release, test, and compatibility failures

| Failure | Evidence anchors | Prevention owner | Detection/product state | Regression/cutover gate |
|---|---|---|---|---|
| Provider plugin schema/cache/permission/install paths drift | PR #268/#273/#278/#303/#307/#316/#400 | catalog/root | Generated manifest, installed/current version, host-native diagnostic | Stock-host install/update/uninstall/restart for every provider; no handwritten divergent copy |
| Upgrade/release asset or version drift gives ambiguous failure | PR #310–#313 | root/release | Exact local/latest/release/asset/workflow state and action | Missing asset, unpublished version, replaced release PR, trigger token, rollback |
| Stale client is silently accommodated by compatibility fallback | Explicit user correction in chronological corpus | catalog/API/root | Version/catalog mismatch with current replacement | Old MCP/daemon/plugin process receives restart/update error; no obsolete-name behavioral fallback |
| Data migration is confused with client compatibility | Durable store/fact history | root/store | Migration receipt/read-only rollback state separate from protocol status | V1 evidence retained one rollback release; V1 live adapter removed at domain cutover |
| Global env/port/process state makes tests flaky | PR #204/#255/#263/#283/#326/#334 | all crates/test harness | Hermetic test context and resource ownership | Repeated shuffled libtest/nextest/platform runs; no retry-only acceptance |
| Windows path/handle/locking behavior differs | PR #207/#209/#328/#351 | store/root/test harness | Platform-specific typed error and conformance report | Windows/macOS/Linux matrices for paths, BOM, locks, rename, delete, service state |
| Timeout/hang is unbounded or hidden | PR #237/#244/#378 | query/hooks/root/API | Deadline/cancellation stage, timeout reason, partial receipt | Frozen clock and slow/stuck provider/store/network cases; cancellation reaches owner |

## 8. Cross-plan ownership

- `01-domain`: stable identities, evidence roles, scope, time, retention, message origin, research anchors.
- `02-store`: durability, corruption, lifecycle leases, private I/O, imports, backup/repair.
- `03-capture`: provider completeness, per-source checkpoints, spool/ack, native evidence.
- `04-projectors`: deterministic origin/agent/Turn/Git/memory/automation projections and rebuilds.
- `05-query`: list-all, caps/cursors, ranking, direct-vs-impact semantics, partial coverage.
- `06-policy`: hint/retrieval/routing/correlation/curation outcomes and replay.
- `07-hooks`: host latency/trust/durability and provider response conformance.
- `08-tool-catalog`: one current capability/name/binding, discoverability, version handshake.
- `09-application`: shared use cases, remediation predicates, idempotent commands/workflows.
- `10-api`: bounded routes, current protocol, SSE gaps, auth, export/anchor transport.
- `11-dashboard`: Brain/Explorer/Loom/labs/Evolution and all failure/coverage states.
- `12-root-compatibility-migration`: live daemon/install/doctor/release ownership and bounded V1 data cutover.
- `13-research-provenance-and-context-anchors`: stable evidence recovery for future implementation.
- `14-historical-failure-regression-matrix`: this prevention/detection/recovery/cutover inventory.
- `15-search-quality-evaluation-and-retrieval-research`: real local precision corpus, hybrid retrieval research, metrics, and Search Quality Lab.
- `16-cross-project-repository-worktree-scope`: canonical scope plane, federated graph/activity/store routing, Rspack/Rsbuild/React Router corpus, and CLI/MCP ergonomics.
- `17-official-public-api-and-sdks`: supported public contract, direct agent use, SDKs, conformance, docs, and sandbox.
- `18-secret-detection-redaction-and-private-data-safety`: one mandatory sanitizer/taint boundary, protected quarantine, sink firewalls, retroactive scan/remediation/restore, and synthetic Secret Safety Lab.
- `19-system-defragmentation-convergence-and-extensibility`: one-owner convergence map, extension SPIs, architecture governance, scale boundaries, and duplicate-path retirement.

## 9. Verification protocol

For every implementation PR:

1. Resolve the relevant research/failure anchors and record current drift/coverage.
2. Add the named historical case to a redacted fixture or hermetic copied-store harness.
3. Write the failing test for the invariant before moving behavior.
4. Verify prevention, detection/product state, recovery, and rollback—not only the happy path.
5. Run focused tests, affected tests from evidence-bearing impact, and the required platform/fault matrix.
6. Update compatibility/failure inventory status: V1-only, V2-shadow, parity-proven, V2-default, migration-only, retired.
7. Block cutover on unexplained behavior, data, identity, privacy, or outcome gaps.

## 10. Definition of done

- Every row has an owning crate/plan, deterministic or explicitly labeled probabilistic test, and visible product/operational state.
- Merged/open PR semantics #405/#406/#407/#410/#411/#412/#413/#414/#415/#416/#417/#418/#419/#420 are represented without assuming obsolete #409, treating open release PR #418 as published 0.0.48, or replaying uncertain writes during #420-style reconnect.
- The current libtest order-dependent backfill failure is recorded and cannot be waved away by its isolated pass.
- Disk-full, concurrent-writer, process-death, stale-client, provider-drift, and partial-shard failures have named end-to-end drills.
- Wrong-project/worktree/ref/store, capped registry, cross-project exact-load, first-CWD attribution, and missing-domain capability failures have named end-to-end drills.
- Secret default-off/bypass, hook/log/analytics leakage, unsafe legacy/vector backfill, marker oracle, handle/backup/dashboard copies, serialized-envelope false positives, and restore resurrection have named end-to-end drills.
- Historical issue fixes become fixtures before V1 code is deleted.
- Non-disposable evidence is migrated; stale live clients/tool names are not emulated.
- Future implementation agents can retrieve the evidence behind every failure class through document 13.
