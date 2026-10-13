# Work and Remote Brain journey qualification

Issue #3656 asked whether a lighter core interface or an optional deployment
boundary would improve usability for Work and Remote Brain, decided only
after one representative job for each was qualified on a live daemon. This
record names the jobs, what ran, what the results were, and the decision.
It authorizes no removal, no remote deployment change, and no inspection of
operator data.

## Evidence rules

- Every result below comes from spawned `tracedecay daemon run` processes
  under throwaway profiles and HOME directories. No run touched an operator
  profile or a live daemon.
- Work provider attempts ran a sha256-pinned stand-in script bound through
  the production configuration control plane. No run made a provider
  request, so provider token cost was zero.
- No adoption telemetry was read; reading it is not authorized here.
  Missing adoption data is not evidence that a job has no users, and this
  record makes no claim either way.

## Work: bounded parallel implementation with reviewed synthesis

**Job.** An operator splits work into two tasks, runs one agent attempt per
task at the same time in separate worktrees, can pause a run without losing
in-flight work, survives a daemon restart, and gets one synthesis attempt
that cites both results.

**Harness.** `//crates/tracedecay-cli:work_loop_journey`,
`the_work_loop_journey_runs_end_to_end_through_the_daemon` in
`crates/tracedecay-cli/tests/work_loop_journey.rs`. Every call is an HTTP
POST to the daemon's published application endpoint with the daemon's own
token and origin.

**Result: qualified.** Every leg below passes on master plus the journey
change. Two legs were also checked by breaking them on purpose. With both
sources printing the same stdout, the source artifact assertion fails on the
digest. With the per-repository cap left at one, the second start fails with
`application.work-attempt.capacity-exhausted` (429, `saturated`).

| Leg | What the journey asserts |
| --- | --- |
| Concurrency policy | The default topology admits one active attempt. A protected preview and apply raise the global and per-repository caps to two, committing on the previewed revision, and take effect after a restart. |
| Propose, accept, admit | A second task is added, its generated proposal recommends `proceed_to_acceptance`, it is accepted, and its execution is admitted. |
| Place | Each run gets a linked worktree whose preflight has no blockers and whose admission is at authority version 1. A start on run A that names the primary checkout is refused with `application.work-attempt.placement-root-conflict`. |
| Parallel attempts | One `list-attempts` read shows both sources `running` while both stand-in processes have started, each in its own placement root. Neither can exit before the test releases them. |
| Pause | Pausing run A records exactly the in-flight attempt it fenced, and a new start on run A is refused with `application.work-run-control.paused`. The fenced attempt still settles. |
| Restart | After a daemon restart, run A's control record equals the pause response field for field, run B is uncontrolled, and both attempt-status payloads equal their pre-restart values. |
| Resume and synthesize | Synthesis is refused while paused. After resume it is admitted, cites exactly the two source digests with nothing uncited, succeeds, and its provider receives both digests on stdin. |
| Canonical artifacts | Each source's artifact digest equals a digest the test computes from the exact bytes the stand-in printed. `hydrate-artifacts` returns all three attempts with those digests and byte lengths. |
| Release | Both placements release to authority version 2 and `placement-status` reports exactly the released record. |

**Operator steps.** Done from scratch, the job takes 26 state-changing calls
across 12 operations before any status polling. Per task that is create
(prepare and mutate), generate a proposal, accept it (prepare and mutate),
admit execution (prepare and admit), admit a placement (preflight and admit),
and start an attempt. Then come a protected topology preview and apply with a
daemon restart, one synthesize, one hydrate, and one release per placement.
Each call carries values from earlier responses by hand, such as the
execution snapshot from admission into start and authority versions into
resume and release. The whole journey, including its older legs, issued 87
HTTP requests in one logged run, polls included.

**Cost.** The full journey passed in 14.7 s to 33.4 s of test time across
three runs. Provider cost was zero tokens, because the provider is a
stand-in.

**Manual alternative.** Without Work, the operator creates two worktrees by
hand, starts two agent sessions, and tracks them in terminals. Nothing stops
a third session from starting, a pause is a killed process, a daemon or
machine restart loses which session did what, and the operator merges the
two outputs by reading them. The journey shows what Work adds over that:
concurrency caps the operator set, a pause that fences new attempts while
in-flight ones settle, run state and receipts that survive a restart, and a
synthesis whose cited digests are the exact source artifacts.

**Gaps found.** An admitted placement did not bind where an attempt runs.
Both parallel attempts ran in the primary checkout while their runs held
distinct linked-worktree placements (#3693). Start now refuses an attempt
outside its run's admitted placement root. A `clean_in_place` preflight on
a checkout created with `git init --separate-git-dir` reports
`target_unreadable` (#3694), which is why the journey places both runs in
linked worktrees. The route limit `max_concurrency` travels in every execution
snapshot but nothing enforces it (#3695).

## Remote Brain: offline capture replayed to one authority

**Job.** A laptop keeps capturing observations while its owning authority is
unreachable, then replays them to that one fenced authority, queries the
shared result, and loses its credential when revoked.

**Harness.** `//crates/tracedecay-sdk:sdk_suite` spawned-daemon tests in
`crates/tracedecay-sdk/tests/sdk_suite/production_daemon.rs` (run with
`--include-ignored`), plus a throwaway probe of the same harness for the legs
no committed test reaches. The probe's per-leg output is recorded on #3656.

**Result: not qualified. The job cannot run on master.** The legs that exist
pass. The offline path between them does not exist in production.

| Leg | Result on spawned daemons |
| --- | --- |
| Authenticate and enroll over TLS | Passes. A wrong trust root fails as a transport error, a wrong authority as a protocol error, and local routes are not served on the TLS listener (`enrolled_remote_client_rejects_an_untrusted_private_authority_and_isolates_enrollment`). |
| Single fenced writer and failover | Passes. Publication is idempotent, promotion moves epoch 1 to 2 and fences the old writer, and republishing after promotion returns 409 (`enrolled_node_fails_over_after_the_operator_publishes_the_first_writer`, `enrolled_remote_failover_without_a_published_writer_is_a_typed_refusal`). |
| Capture while offline | Refused. Before writer publication the answer is the concealed `not_found_or_not_authorized`; after it, `remote.authority_reachable`. Production never records an unreachable authority, so capture is never admitted (#3687). |
| Transfer with ordered drain | No sender. The receiving route exists, but nothing exports a pending frame (#3688). |
| Laptop and authority in one Brain | Refused. Each profile mints its own Brain and provisioning a foreign Brain returns 409 (#3689). |
| Replay and query of replayed data | Unreachable on a live daemon because no frame arrives. Component tests cover the replay state machine and query coverage. |
| Revoke | No entry point. Every candidate route returns an empty 404 and the CLI has no subcommand (#3690). |

Two further defects surfaced in the same runs. Protocol envelopes report the
authority unavailable after publication while `remote-status` reports it
available (#3691). The SDK reports a concealed 404 as an opaque protocol
error, and a foreign-Brain provisioning refusal is labelled a database error
(#3692).

**Local alternative.** Each machine keeps its own profile and its own
memory. That is what every operator has today, because the remote job cannot
run.

## Decision

**No optional deployment boundary for Work.** Work owners mount inside the
daemon's project-open owners (`crates/tracedecay/src/daemon/project_open_owners.rs`)
and share the project store, configuration control plane, and receipt
history with every other capability. A separately deployed Work
service would need its own scheduler and store authority, which #3656 rules
out, and the journey shows the pieces that would have to cross that boundary
(topology policy, execution snapshots, placement, run control, attempt
receipts, synthesis provenance) all resolve through one authority today.

**No packaging split for Remote Brain.** The boundary already exists at
runtime without a second store. The TLS listener binds only when the daemon
is started with `--remote-listen`, `--remote-tls-cert`, and `--remote-tls-key`
(`crates/tracedecay-cli/src/cli.rs`), and the dashboard already places Remote
Brain inside Settings rather than as a workspace. A packaging decision on
value grounds is premature, because the job cannot run yet and there is no
result to weigh. #3687 to #3690 are the work that makes it runnable. TLS
identity, revocation, writer fences, ordered drain, pending encrypted frames,
and replay receipts stay in place meanwhile.

**Advanced grouping: use the catalog profiles that already exist.** The
catalog already composes `profile.compact` (22 bindings) and
`profile.host-limited` (17 bindings), and every Work and workflow capability
is eligible only for `profile.default`
(`crates/tracedecay-contracts/src/work_catalog.rs`,
`crates/tracedecay-contracts/src/catalog_composition.rs`). No host can select
a non-default profile at serve time today, so every MCP client sees all 47
Work and workflow capabilities (31 in `work_catalog.rs`, 16 in
`workflow_catalog.rs`). A lighter interface is a profile selection, not a
build split. #3697 lets a host select it.

**Guided default path for Work: yes.** The 26 hand-threaded calls are the
usability cost the journey measured, and `docs/USER-GUIDE.md` does not
describe the Work loop. A guided path that composes the same operations
through the same authority, and stops at the first refusal with its typed
problem, keeps every safety check while removing the manual threading
(#3696). Pause, resume, cancel, retry, and adjudication stay available as
advanced operations.

## Retained behavior

Nothing in this record removes or weakens: task and definition histories,
canonical artifacts, grants, budgets, cancellation, pause and frontiers,
effect reconciliation, placement and worktree ownership, pending encrypted
frames, replay receipts, TLS identity and authentication, revocation checks,
writer fences, or ordered drain. Existing tasks and frames are not migrated
or stranded.
