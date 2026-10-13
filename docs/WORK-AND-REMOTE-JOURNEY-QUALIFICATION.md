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

**Guided path.** Since #3738 the same job is one request. `tracedecay work
advance-job --request-file job.json --until-settled` (also
`tracedecay_work_advance_job` and `POST /application/work/advance-job`) names
the tasks, their placements, and the synthesis. Each call runs the next step
through the same Work dispatch, so every step keeps its own capability check,
receipt, and typed refusal. Section 8 of the journey drives the parallel job
this way. Both guided sources run at once and their digests equal the
step-by-step digests. A pause followed by a daemon restart refuses synthesis
with the step-by-step refusal. After resume the job completes, and a repeat
call performs nothing. The operator's steps drop from 26 hand-threaded calls
to one request, plus pause and resume when they want them.

**Gaps found, all fixed.** Each gap below was found by this journey and is
now asserted by it.

| Gap | Fix |
| --- | --- |
| Both parallel attempts ran in the primary checkout while their runs held distinct linked-worktree placements (#3693). | #3726. Start, retry, and synthesis refuse an attempt outside its run's admitted placement root with `application.work-attempt.placement-root-conflict`. |
| A `clean_in_place` preflight on a `git init --separate-git-dir` checkout reported `target_unreadable` (#3694). | #3708. The journey's fixture is such a checkout and its preflight now reads it as a worktree. |
| The route limit `max_concurrency` travelled in every execution snapshot but nothing enforced it (#3695). | #3727. Admission refuses an attempt once its route's active attempts reach the snapshot's limit. The journey route allows two. |
| The 26 hand-threaded calls had no guided path, and the user guide did not describe the loop (#3696). | #3738. See the guided path above. |

**Rerun on master.** `//crates/tracedecay-cli:work_loop_journey` passed in
15.4 s on master at `2088736535` (2026-10-13), with every leg above
including the guided section.

## Remote Brain: offline capture replayed to one authority

**Job.** A laptop keeps capturing observations while its owning authority is
unreachable, then replays them to that one fenced authority, queries the
shared result, and loses its credential when revoked.

**Harness.** `//crates/tracedecay-sdk:sdk_suite`, test
`remote_journey::a_laptop_captures_offline_and_its_frames_reach_one_fenced_authority_until_revoked`
in `crates/tracedecay-sdk/tests/sdk_suite/remote_journey.rs`, run with
`--include-ignored`. It spawns two daemons with their own HOME, profile, and
loopback TLS listener: a desktop that owns the Brain and a laptop that joins
it. Every leg goes through the daemons' published endpoints and the
`tracedecay` CLI.

**Result: qualified.** When this record was first written the job could not
run. The legs that existed passed, but no offline path connected them. The
first run of this journey then found that a drained frame could not be
replayed or queried (#3764). Every leg below now passes on master plus this
change.

| Leg | What the journey asserts |
| --- | --- |
| Authenticate and enroll | The desktop operator provisions the laptop's node and the laptop enrolls over TLS with the grant credential. The grant carries capture, transfer, replay, query, discovery, and authority capabilities. |
| Single fenced writer | The desktop operator publishes the laptop node as the one writer at epoch 1, with the replay and query policies that admit its frames. |
| Join | `POST /remote-nodes/join` on the laptop adopts the desktop's committed enrollment, so the laptop serves the desktop's Brain, not its own. |
| Capture while offline | A join against an endpoint nothing listens on records the authority unreachable. Two captures on the laptop's own daemon are then admitted in sequence, each chained to the previous event. |
| Restart | The laptop daemon restarts and still holds both frames pending. The desktop daemon restarts on its profile, keeps its Brain, and keeps serving the node. |
| Ordered drain | `POST /remote-nodes/drain` sends both frames to the restarted desktop. The desktop acknowledges them in capture order, and the laptop's pending count drops to 0. |
| Replay | Each drained frame replays on the desktop as `acknowledged`, committed under the published writer fence. The desktop's pending count drops to 0. |
| Query | An exact-observation query on the desktop returns `found` for each frame, with the exact observation the laptop captured offline. |
| Revoke | `tracedecay remote revoke --node-id` on the desktop revokes the node's enrollment. Replay and query then refuse with authority reason `enrollment_revoked`. |
| No stranded frames | After revocation the laptop still captures offline. Its drain is refused, and the frame stays pending on the laptop instead of being dropped. |

The other spawned-daemon Remote tests also pass on the same tree. That covers
the four `remote_journey` tests, the five `production_daemon` tests
(untrusted trust root, failover with and without a published writer, a
laptop joining a desktop Brain), and the five `core_cli_suite` tests matching
`remote` (revoke, revoke of an unredeemed grant, enroll, failover refusal,
and the project handshake on a renamed remote store).

**Gaps found, all fixed.**

| Gap | Fix |
| --- | --- |
| Offline capture was never admitted on a running daemon (#3687). | #3739 |
| Nothing sent a node's pending frames to its authority (#3688). | #3744 |
| A node profile could not join another profile's Brain (#3689). | #3742 |
| No operator path revoked a node (#3690). | #3741, plus #3746 for unredeemed grants. |
| Protocol envelopes reported the authority unavailable after publication (#3691). | #3724 |
| A concealed 404 surfaced as an opaque SDK error, and a foreign-Brain refusal as a database error (#3692). | #3717 |
| A restarted daemon stopped serving its node stores (#3730). | #3732 |
| A drained frame could not be replayed or queried on the authority (#3764). | This change. |

The #3764 fix has three parts. Writer publication now also stores the query
policy for the writer's scope. Nothing had written that policy since its
writer was deleted as dead code, so every live query failed authorization.
Replay now projects into the writer fence's generation, which is the
generation a query asks for. A query now checks the writer fence the project
store holds instead of the store's unrelated ownership epoch. It reads at a
bounded commit frontier so the manifest gets a watermark, and it accepts a
replayed row that carries no repository capture.

**Operator steps.** The desktop operator runs 3 commands: provision the
node, publish the writer with its policies, and later revoke. The laptop
operator runs 3: enroll, join, and drain. The laptop's agent captures
offline with no operator step. Replay takes one call per drained frame, and
query takes one call per observation.

**Cost.** The journey passed in 4.8 s to 5.0 s of test time across three
runs, including two daemon restarts. It made no provider request.

**Local alternative.** Each machine keeps its own profile and its own
memory. Observations a laptop captures offline never reach the desktop, and
nothing revokes a lost laptop's access to shared memory, because none is
shared.

## Decision

**No optional deployment boundary for Work.** Work owners mount inside the
daemon's project-open owners (`crates/tracedecay/src/daemon/project_open_owners.rs`)
and share the project store, configuration control plane, and receipt
history with every other capability. A separately deployed Work
service would need its own scheduler and store authority, which #3656 rules
out, and the journey shows the pieces that would have to cross that boundary
(topology policy, execution snapshots, placement, run control, attempt
receipts, synthesis provenance) all resolve through one authority today.

**No packaging split for Remote Brain.** The qualified journey shows why a
split would cost more than it saves. A replayed frame commits into the same
project session store that serves every local read, under a writer fence
that store holds. The query that reads it back goes through that same store,
and the enrollment, revocation, and spool records sit beside them in one
profile. A separately packaged Remote service would need its own store
authority for those records, which #3656 rules out. The optional boundary
already exists at runtime. The TLS listener binds only when the daemon is
started with `--remote-listen`, `--remote-tls-cert`, and `--remote-tls-key`
(`crates/tracedecay-cli/src/cli.rs`). Without those flags no Remote route is
reachable from the network, and the dashboard places Remote Brain inside
Settings rather than as a workspace. An operator who does not use Remote
Brain therefore already runs without it, and no build split is needed for
that. TLS identity, revocation, writer fences, ordered drain, pending
encrypted frames, and replay receipts all stay.

**Guided default path for Remote Brain: yes, for replay.** The remaining
manual cost is one replay call per drained frame, each naming an event id
from the drain response. One guided step that replays every drained frame
in order through the same replay service would remove that threading and
keep each frame's checks and receipt (#3781). Enrollment, join, and
revocation are one command each and need no guided path.

**Advanced grouping: use the catalog profiles that already exist.** The
catalog already composes `profile.compact` (22 bindings) and
`profile.host-limited` (17 bindings), and every Work and workflow capability
is eligible only for `profile.default`
(`crates/tracedecay-contracts/src/work_catalog.rs`,
`crates/tracedecay-contracts/src/catalog_composition.rs`). When this record
was first written no host could select a non-default profile, so every MCP
client saw all 47 Work and workflow capabilities (31 in `work_catalog.rs`, 16
in `workflow_catalog.rs`). A lighter interface is a profile selection, not a
build split, and it has landed that way (#3697). `tracedecay serve
--catalog-profile compact` lists no Work or workflow tool (#3729), and
`tracedecay install --agent <id> --catalog-profile <name>` saves that choice
in the host's registration (#3749). A withheld tool stays callable by name
and dispatches exactly as under the default profile, so no task, grant, or
history becomes unreachable.

**Guided default path for Work: yes, landed.** The 26 hand-threaded calls
were the usability cost the journey measured. `advance-job` (#3738)
composes the same operations through the same authority and stops at the
first refusal with its typed problem, so every safety check stays.
`docs/USER-GUIDE.md` describes it under "Running Parallel Work with
Synthesis". Pause, resume, cancel, retry, and adjudication stay available as
advanced operations.

## Retained behavior

Nothing in this record removes or weakens: task and definition histories,
canonical artifacts, grants, budgets, cancellation, pause and frontiers,
effect reconciliation, placement and worktree ownership, pending encrypted
frames, replay receipts, TLS identity and authentication, revocation checks,
writer fences, or ordered drain. Existing tasks and frames are not migrated
or stranded.
