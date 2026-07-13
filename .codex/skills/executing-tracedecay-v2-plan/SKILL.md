---
name: executing-tracedecay-v2-plan
description: Parse and execute the linked TraceDecay V2 redesign plans without skipping prerequisites or duplicating completed work. Use when selecting the next V2 PR/task, turning the redesign into Kanban/worktree assignments, auditing completion, resuming after compaction, or reconciling plan dependencies with live Git, review, test, and task-graph evidence.
---

# Execute the TraceDecay V2 plan

Treat plan text as the intended dependency specification and the current activated canonical task graph as dispatch authority. Treat immutable Git/review/test receipts as completion authority. A mismatch between plan and graph blocks dispatch until an explicit versioned reconciliation; neither side is silently rewritten. Never infer completion from a checked box, task status, branch name, or worker prose alone.

## Build the inventory once

Run from the repository root:

```bash
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/plan_inventory.py
```

Use `--json` for machine processing and `--id 'PR 4E'` for every declaration of one slice. The script is read-only. It locates PR/task headings, source lines, ordering statements, referenced PR IDs, acceptance-checkbox counts, declared commit subjects, and block hashes. Treat this output as source observations only: preserve each raw mention and `path:start_line-end_line`/block-hash anchor while resolving its normalized ID against the canonical V2 manifest. It does not decide that prose references are gating edges or collapse master/owner/companion declarations silently.

Read these authorities in order:

1. `docs/plans/tracedecay-v2/00-plan-set-index.md` for plan ownership and cross-plan order.
2. `docs/plans/2026-07-09-tracedecay-brain-rewrite.md` for the integrated phase/PR sequence.
3. The numbered owner plan for full acceptance and files.
4. Any companion plan named by `Ordering`, `after`, `depends`, `blocked by`, or the index.

Do not make every worker reread the full plan set. The orchestrator parses once and gives each worker exact source sections, complete acceptance, constraints, and retrieval anchors.

## Normalize and reconcile slice authority

Follow plan 00 §2.1 exactly. Normalize simple scalar, compound scalar, slash, dotted, range, and series forms before grouping declarations. Preserve the single identity-bearing compound hyphen: `22F-LE`, `22F-LS`, `24D-API1` through `24D-API4`, `24D-SDK1` through `24D-SDK3`, `24E-API5`, and `33S-2` are scalar IDs. U+2013 EN DASH always denotes a range. For ASCII `-`, test the three complete legacy simple-range productions first (including `24E0-24E8`), then test the whole token as exactly one compound scalar; reject tokens matching neither grammar. Require an en dash plus two complete endpoints for a range of compound IDs. Produce one `tracedecay.v2.slice-dag/v1` owner record per normalized scalar ID; attach other declaring sections as companions, merge all non-conflicting acceptance criteria, and retain incidental references as non-dispatchable evidence. Never turn a range, series heading, duplicate description, companion, or prose mention into another executable record.

Reject the whole candidate manifest on an unknown/duplicate owner, malformed or oversized range, ambiguous slash/dot form, empty/recursive series, contradictory merged field, stale/missing source anchor, unknown or cyclic typed edge, digest mismatch, or duplicate idempotency key. Do not “repair” these by source order or partial import. Compute canonical digests and `v2-slice-owner/v1:<percent-encoded-normalized-id>:<content-digest>` keys only after reconciliation.

Use this deterministic validation flow completely and accumulate all diagnostics before rejecting the candidate:

1. Pin the Git commit, resolve the indexed plan set, and run the deterministic bootstrap-manifest locator. Pre-V2 board/database state must first be explicitly exported to that manifest; never discover it from ambient board, profile, task-history, or UI state.
2. Scan files by `(path, line)`, preserving raw tokens and source anchors. Classify every occurrence as declaration, series, or incidental reference before normalization.
3. Normalize declarations under plan 00 §2.1 and join them to the located bootstrap manifest's expected IDs before cutover, or the activated canonical graph's IDs afterward. Missing bootstrap input blocks validation; it never permits prose to create the key set. A declaration without an authority key, or an authority key without a declaration, is `missing_id`, not a new slice. Incidental references stay non-dispatchable unless an explicit authority revision promotes them.
4. Resolve the indexed owner and attach all same-ID declarations as companions. Merge fields without precedence guessing. Deduplicate canonically equivalent acceptance descriptions into one criterion while unioning sorted source anchors; retain distinct compatible criteria; reject contradictory owners, criteria, phases, subjects, edge kinds, or file bounds.
5. Validate phase `0..5`, typed edge kinds and payloads, known dependency endpoints, series, anchors, `source_set_digest`, per-owner `content_digest`, and exact idempotency keys. Detect cycles over gating edges only after all endpoints resolve.
6. Compare IDs, edges, owners, and digests with both bootstrap input and candidate graph; enforce the atomic cutover receipt. Sort records by normalized ID and anchors by source location so identical inputs produce byte-identical output.

Diagnostics are two separate, deterministically sorted collections. Errors block the complete candidate and `next_ready`: `missing_id`, `malformed_id`, `ambiguous_id`, `missing_owner`, `conflicting_owners`, `conflicting_field`, `invalid_series`, `unresolved_dependency`, `invalid_phase`, `invalid_edge_type_or_payload`, `source_anchor_mismatch`, `digest_mismatch`, `idempotency_mismatch`, `duplicate_idempotency_key`, `reconciliation_mismatch`, and `cycle`. Warnings are evidence only and cannot change records or eligibility: `duplicate_description`, `compatible_companion_addition`, and `incidental_reference`. Include normalized ID when known, exact source anchor and block hash, raw value, violated rule, and only an unambiguous suggested spelling. Deduplicate identical diagnostics and use plan 00's total sort key. If equivalence or compatibility is uncertain, emit `conflicting_field` rather than a warning.

The current `plan_inventory.py` is only a legacy heading/block-hash inventory aid. It does not implement plan 00 §2.1 normalization or fail-closed classification: in particular, do not use its treatment of slash IDs, dotted IDs, compound IDs, ranges, or `series` headings as canonical IDs. Reconcile those forms independently against §2.1, and block on any difference. Its output cannot satisfy the bootstrap gate. The V2 manifest parser is not accepted until focused contract tests cover every normative compound/range example and malformed case in §2.1; passing the legacy helper tests does not discharge that acceptance.

## Bootstrap without ambient state

Until the frozen manifest/state export is explicitly activated, plan authoring, finalization, order auditing, and review use the inventory and cited plan sections; do not report the expected missing operational state as a plan-review failure. Manifest/state is mandatory for selecting or dispatching implementation work.

Before V2 cutover, resolve the manifest only from one explicit argument, `TRACEDECAY_V2_EXECUTION_MANIFEST`, or the repo-root `.tracedecay/v2-execution-manifest.json`, in that order and with plan 00's containment/failure rules. Never search ambient/current boards, sibling databases, profiles, task history, or UI state. Missing or ambiguous location blocks dispatch but not read-only inventory, audit, or plan review.

Validate complete inventory coverage, normalized ownership, anchors/digests, typed edges, acyclicity, and stable-key import; compare the imported candidate to canonical IDs/edges/digests; require zero extras/conflicts; and record one atomic activation receipt. Install the frozen controller export through the sole canonical command:

```bash
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/compile_plan_authority.py \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan \
  --manifest-output docs/plans/tracedecay-v2/execution-authority.json \
  --state-output .tracedecay/v2-execution-state.candidate.json
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/compile_plan_authority.py \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan --check
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/bootstrap_execution.py \
  --manifest docs/plans/tracedecay-v2/execution-authority.json \
  --state-export .tracedecay/v2-execution-state.candidate.json \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan
```

The compiler consumes the explicit reviewed `plan_authority_registry.json`: 257 executable slices plus eight non-executable series, with checked owner anchors, phases, commit subjects, and prerequisites. It materializes every input from the exact immutable Git tree and emits `activation_mode=verify_only`: a complete topological verification graph with no completion entries, worker packets, tests, branches, or worktrees. Bootstrap cross-checks IDs, content digests, dependencies, graph revision, the exact canonical Git-tree source-set digest, and the complete execution-state validator, stages manifest/state in one immutable generation, then atomically switches `.tracedecay/v2-execution-active.json`. It never installs partial or dispatch-mode state. Verification-only activation returns the full order and zero dispatchable `next_ready` packets; a later reviewed daemon authority revision must supply exact bounded packets and a fenced transition before dispatch mode exists. After cutover, treat the locator only as explicit reconciliation input: the activated canonical graph remains dispatch authority.

## Transition the reviewed initial staged scope

Do not widen V1. `compile_plan_authority.py` and `bootstrap_execution.py` remain verify-only V1 entrypoints. The first dispatch authority uses exact schemas `tracedecay.v2.execution-state/v2`, `tracedecay.v2.canonical-dag/v2`, and `tracedecay.v2.next-ready-view/v2`; `plan_execution.py` routes V1 and V2 explicitly and rejects every unknown version with an empty ready set.

The checked reviewed source is `scripts/staged_dispatch_pr1.json`. It authorizes exactly `PR 1` and contains its full owner/content/acceptance/source-block/file/command/test/workspace/lane/gate contract plus an explicitly disabled retained Claude-review compatibility field. V2 materializes exactly one sealed PR 1 packet and 256 sealed `not_in_dispatch_scope` block records. Packet and blocker IDs are disjoint and exhaustive over all 257 nodes; every per-node entry and the complete entry set are domain-separated and digest-bound to repository, source, graph, policy, and packet-source pins. Missing, duplicate, overlapping, unknown, unsealed, or extra entries invalidate all dispatch. Execution order is never scope authority.

Use `transition_execution_authority.py` only. Commit the transition implementation and checked packet first, regenerate the checked manifest from that same immutable canonical plan tree, and bootstrap a fresh compiler-produced V1 predecessor pinned to that exact canonical commit and source-set digest. Never transition from a predecessor pinned to the pre-commit tree. Then prepare deterministic review bytes from the named stored V1 predecessor generation and immutable checked packet source:

```bash
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/transition_execution_authority.py \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan \
  --expected-active-generation <exact-revision-5-generation> \
  --activated-at <reviewed-RFC3339-time> \
  --prepare-candidate <review-candidate.json>
```

An independent control-plane reviewer records a receipt for that exact candidate, packet-source digest, and predecessor generation/state/graph fence beneath `.tracedecay/v2-authority-review-receipts/`. The operator records its digest in the fixed owner-only mode-`0600` ledger `.tracedecay/v2-authority-review-observations.json`; activation accepts no caller-selected observation path. This ledger is an explicit local operator trust assertion, not cryptographic proof of reviewer identity. Then invoke the same command with `--candidate <review-candidate.json> --trusted-review-receipt <trusted-receipt.json>` and the identical root/ref/generation/time. Under the existing activation lock it reloads the named stored predecessor generation, revalidates its exact compiler bytes, target revision `prior+1`, source/manifest/graph/packet/block/set digests, receipt binding, and active-pointer bytes before one atomic compare-and-swap. Exact replay reloads that stored predecessor even after the pointer names the target generation and is idempotent. Stale or missing generation, source drift, revision skip, unobserved or malformed receipt, equal-revision different bytes, or concurrent activation writes nothing.

The initial packet keeps GPT-5.6-Sol as reasoning, lifecycle, decomposition, and complex-review owner. Its retained `claude_adversarial_review` compatibility field is explicitly disabled with zero steps and no runtime or acceptance criteria. Claude/Opus is never a planning, audit, reasoning, authority-review, or integration lane. A later GPT-decomposed single-branch micro-implementation may opt into Opus only through a separately reviewed packet revision; it is not implied by this packet. TraceDecay validates but does not create, relocate, push, merge, or otherwise own the declared worktree.

## Validate the activated graph and ledger

Export exactly one supported execution-state document. `tracedecay.v2.execution-state/v1` remains the complete verify-only or all-node dispatch contract. V2 is the explicit staged-dispatch contract containing one reviewed packet, one explicit blocker for every other node, the predecessor transition fence, its activation receipt, the pinned `tracedecay.v2.completion-ledger/v1`, and retired-obligation tombstones. Then run:

```bash
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/plan_execution.py \
  --root /path/to/authoritative/checkout \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan --next-ready
```

`--root` is mandatory. An explicit `--graph` wins, followed by `TRACEDECAY_V2_EXECUTION_STATE`. Without either override, exactly one repo-local source may exist: the legacy direct `<repo-root>/.tracedecay/v2-execution-state.json` or the atomically switched `<repo-root>/.tracedecay/v2-execution-active.json` generation. Coexistence fails closed as ambiguous; neither silently shadows the other. Manifest resolution uses the identical explicit/environment/one-repo-local-source policy. Validation hashes exact plan blobs from the Git tree resolved by `--canonical-ref`, never files from a different checked-out commit. Before this redesign branch is integrated, pass its full ref rather than `master`. `compile_plan_authority.py --check` byte-compares the regenerated manifest with the canonical-ref blob at `docs/plans/tracedecay-v2/execution-authority.json`. Markdown is the bounded human/MCP-default view. Use `--format json` for the sealed `tracedecay.v2.next-ready-view/v1` or `tracedecay.v2.next-ready-view/v2` matching the input state schema. Both formats contain the same validity/source/digest/revision pins, diagnostics, complete packet fields, and blocker reasons. After V2 activation, ordinary reads automatically resolve the fixed authority-review observation ledger above. Invalid input or any Git/source observation failure exits 2 and always returns an empty ready set (`Unknown`, never guessed false/true).

The canonical DAG must pin repository identity, exact current canonical source/integration SHA, source-set digest, positive graph revision, graph digest, nodes, and a byte-matching activation receipt. Nodes have unique IDs and unique owners, explicit prerequisites, and a canonical digest over their complete dispatch contract. V1 requires a complete packet for every node in dispatch mode. V2 requires the reviewed packet/block partition and complete-set digest. The helper rejects duplicate IDs/owners, unknown/self/retired prerequisites, cycles, stale graph or activation pins, contract rewrites that no longer match canonical digests, missing partition members, and any reference to retired corrected tombstone `FM-168`.

Every ledger entry repeats the current source commit, source-set digest, graph revision, and graph digest and contains:

- candidate commit/digest and a fresh live clean-worktree observation proving the exact canonically declared branch/worktree;
- implementation task/actor plus parent/review/remediation/successor-review/integration task lineage;
- exact-candidate independent review verdict and anchored receipt, with reviewer principal and authority distinct from implementation;
- complete named required tests bound to exact declared acceptance commands, with exit code and candidate pins;
- canonical integration receipt embedding the live sealed `git merge-base --is-ancestor` observation for the exact candidate/current canonical commit, resolved full canonical ref, and repository identity;
- the attempt/lease fence, observed steering watermark, terminal-CAS sequence, every required steering directive through that cutoff, and canonical disposition receipts binding directive/attempt/fence/event/delivery/ack/disposition/actor/authority.

Candidate, workspace, review, test, integration, ancestry-observation, and steering receipt digests are recomputed from canonical payload bytes. Integrated history does not require its old worktree to remain present; immutable candidate evidence plus fresh ancestry remains mandatory. Review and test receipts must also appear in trusted daemon/task-event observations; self-authored JSON plus a recomputed hash never proves occurrence. Until that application boundary exists, the standalone helper fails closed on such completion claims. Shape-valid digest strings and asserted ancestry/independence booleans are never trusted. Every Git subprocess has a finite timeout and bounded output; failures are `Unknown`. Required steering delivered after the recorded observation but before terminal CAS invalidates the attempted completion; required steering arriving after terminal CAS must bind exact remediation and successor-review task IDs in lineage before opening that path without rewriting history. Advisory steering does not fence completion.

The validator rejects duplicate/ambiguous entries and stale or mismatched receipt pins. Candidate-only or otherwise incomplete entries remain valid evidence but are explicitly blocked; they are never completion. The helper never reads a card/task status field and its exact schemas reject such extra fields.

The graph export is operational state and stays outside Git. Do not commit live task statuses, private task text, worktree paths, provider output, or receipts into the skill. The plan set defines intent; the activated graph defines dispatch; immutable receipts define completion.

## Determine completion

For each candidate slice, collect all of:

- exact implementation commit reachable from the intended branch;
- clean, correct worktree/branch binding;
- required independent review verdict over that exact candidate;
- named tests/checks and their receipts;
- remediation and successor-review state for every negative verdict;
- integration commit when downstream work requires integrated output;
- current canonical integration SHA/source digest and open changes that supersede plan assumptions.

The view derives only `verified_integrated`, `untouched`, or explicit blocker reason codes from those receipts. Operational labels such as `not_started | active | changes_requested | implemented_unreviewed | approved_unintegrated | integrated | superseded | blocked_unknown` may be displayed elsewhere but are never completion inputs. A task marked `done` with `CHANGES_REQUESTED` is terminal review evidence, not approved implementation.

## Select the next work

1. Validate the complete export; any authority error suppresses every packet.
2. Exclude slices with verified integrated completion.
3. Block a slice with any candidate/incomplete ledger entry; remediation and successor review must advance that exact lineage before reconsideration.
4. Block a slice unless every canonical prerequisite has verified integrated completion. Candidate-only, approved-unintegrated, or same-stack assumptions do not satisfy a prerequisite.
5. Return untouched eligible slices in canonical ID order with their checked bounded packets. Critical-path preference requires a separate canonical policy field; never infer it from prose or source order.
6. Create implementation, independent review, remediation, successor review, and integration gates as distinct work items.
7. Attach parents at creation time. Publish multi-edge graph grooming atomically; block dispatch, add replacement parents before removing old parents, and recheck for stale claims after every mutation.
8. Use stable idempotency keys derived from plan ID + slice + role + candidate generation.

Never call a slice eligible because its parent title/status looks complete. Resolve canonical IDs and inspect results.

## Worker packet

Include:

- plan file + exact section/line and PR ID;
- objective, bounded files, required skills, workspace/branch, effect ceiling;
- every prerequisite ID and accepted input commit;
- full acceptance checklist and exact commands;
- requested lifecycle owner and acting runtime/model;
- prohibition on self-approval, merge, push, or unrelated edits as applicable;
- required handoff: candidate SHA, diff scope, tests, risks, retrieval anchors.

Enforced packet bounds are 32 exact files, 32 exact commands, 64 retrieval anchors, and 2,048 characters per string. Every packet names GPT-5.6-Sol as multi-step reasoning, decomposition, review, and lifecycle owner. The initial staged packet disables its retained Claude-review compatibility field. Any future Opus use requires a new reviewed packet revision bounded to one GPT-decomposed micro-implementation branch; Opus receives no planning, audit, reasoning, authority-review, or integration role. Oversized, missing, duplicate, or non-GPT-owned packet fields fail the whole export closed.

Use native Claude Code/Codex CLI acting lanes as separate attempt participants only when a later canonical V2 route explicitly requests them; do not disguise them as Hermes provider profiles. The initial staged packet is entirely GPT-5.6-Sol-owned and has no Claude step. A future Opus micro-implementation lane requires an explicit canonical packet-policy revision rather than overloading the disabled review field. Record every actual participant and route receipt; no lane self-approves and native-CLI exit success is not acceptance.

## Review and advance

After every coherent checkpoint, independently inspect the actual diff, branch, plan authority, and board graph before promoting downstream work. A negative review completes as evidence and creates one idempotent remediation + successor review. Integration depends on the latest approved successor review, not merely on remediation completion.

Before handoff, report:

```text
Selected: <canonical plan slice>
Why eligible: <all prerequisite receipts>
Blocked alternatives: <exact missing evidence>
Worker packet: <task IDs/worktree/acceptance>
Next gate: <independent review or integration>
```

Stop on ambiguous authority rather than inventing an edge or completion state.
