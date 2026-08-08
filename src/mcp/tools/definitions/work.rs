//! MCP discovery definitions for the canonical Work operations.
//!
//! There is one definition per `WorkOperation` variant in `tracedecay-api`,
//! named `def_work_{operation_key}` and advertised as
//! `tracedecay_work_{operation_key}`. Each advertised schema mirrors the typed
//! request contract the operation decodes — those request types deny unknown
//! fields, so a property advertised here that the contract does not have would
//! make every call fail. Read-only versus mutating follows
//! `WorkOperation::is_read_only()`.

use serde_json::{Value, json};
use tracedecay_api::WorkOperation;

use super::{def, def_rw, required_object_schema};
use crate::mcp::tools::ToolDefinition;

// ---------------------------------------------------------------------------
// Shared property builders
// ---------------------------------------------------------------------------

/// One canonical identity string.
fn identity(description: &str) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": description
    })
}

/// One UTC instant, in microseconds since the Unix epoch.
fn micros(description: &str) -> Value {
    json!({
        "type": "integer",
        "description": description
    })
}

fn task_id() -> Value {
    identity("Canonical task identity this operation is scoped to.")
}

fn run_id() -> Value {
    identity("Canonical run identity within the task.")
}

fn attempt_id() -> Value {
    identity("Canonical attempt identity within the run.")
}

fn command_id() -> Value {
    identity(
        "Caller-minted command identity. Replaying the same identity is the same command, not a second effect.",
    )
}

fn occurred_at() -> Value {
    micros("When the caller observed this command, in microseconds since the Unix epoch.")
}

fn expected_version() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "description": "The Work version the caller read. A mismatch is refused as a conflict instead of overwriting a moved version."
    })
}

fn dependencies() -> Value {
    json!({
        "type": "array",
        "items": {"type": "string", "minLength": 1},
        "uniqueItems": true,
        "description": "Task identities this task depends on. Omit for no dependencies."
    })
}

fn projection_page_size() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "maximum": 1000,
        "description": "Maximum tasks to return in this page."
    })
}

fn attempt_page_size() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "maximum": 1000,
        "description": "Maximum attempts to return in this page."
    })
}

/// The resume cursor a Work projection delta continues from.
fn projection_resume_cursor() -> Value {
    let mut schema = required_object_schema(
        json!({
            "generation_id": identity(
                "The projection generation the cursor was minted under. A superseded generation is refused as stale."
            ),
            "token": {
                "type": "string",
                "minLength": 1,
                "maxLength": 2048,
                "description": "Opaque continuation token from the previous snapshot or delta page."
            }
        }),
        &["generation_id", "token"],
    );
    schema["description"] =
        json!("Resume point returned by the previous Work projection snapshot or delta page.");
    schema
}

/// One attempt identity: the task, the run within it, and the attempt.
fn attempt_identity() -> Value {
    required_object_schema(
        json!({
            "task_id": task_id(),
            "run_id": run_id(),
            "attempt_id": attempt_id()
        }),
        &["task_id", "run_id", "attempt_id"],
    )
}

/// The paging cursor shared by the attempt list, artifact hydration, and
/// topology reads. It is pinned to the verified topology generation it was
/// minted under.
fn attempt_list_cursor() -> Value {
    let mut schema = required_object_schema(
        json!({
            "generation": identity(
                "The verified Work topology generation the cursor was minted under."
            ),
            "start_after": attempt_identity()
        }),
        &["generation", "start_after"],
    );
    schema["description"] = json!(
        "Resume point from the previous page. Omit for the first page. A cursor minted under a superseded topology generation is refused as stale, never answered from a different generation."
    );
    schema
}

fn attempt_page_properties() -> Value {
    json!({
        "page_size": attempt_page_size(),
        "cursor": attempt_list_cursor()
    })
}

/// The command a proposal review or acceptance carries.
fn review_proposal_command() -> Value {
    let mut schema = required_object_schema(
        json!({
            "task_id": task_id(),
            "proposal_id": identity("Identity of the generated proposal being dispositioned."),
            "proposal_digest": identity(
                "Algorithm-tagged digest of the evaluated proposal content, exactly as it was generated. It binds the disposition to that content."
            ),
            "expected_version": expected_version(),
            "command_id": command_id(),
            "occurred_at": occurred_at()
        }),
        &[
            "task_id",
            "proposal_id",
            "proposal_digest",
            "expected_version",
            "command_id",
            "occurred_at",
        ],
    );
    schema["description"] = json!("The proposal this disposition is recorded against.");
    schema
}

/// The properties of one admitted provider attempt start.
fn start_attempt_properties() -> Value {
    json!({
        "task_id": task_id(),
        "run_id": run_id(),
        "attempt_id": attempt_id(),
        "operation": identity("The workflow operation reference this attempt executes."),
        "execution_snapshot": {
            "type": "object",
            "description": "The fully resolved execution snapshot: route, backend, protocol, executable reference, model, approval, egress, filesystem and sandbox policy, limits, deadline, credential references, environment allowlist, fallback topology, configuration revision and snapshot identities, and the behavior, resolution-provenance, and topology-policy digests. It is a resolved record, not a command line."
        },
        "worktree_root": {
            "type": "string",
            "minLength": 1,
            "description": "Absolute root the attempt executes in, as the admitted placement resolved it."
        },
        "reference": {
            "type": ["string", "null"],
            "description": "Git reference the attempt works on, or null when it works from a detached commit."
        },
        "commit": identity("Commit the attempt starts from."),
        "instructions": {
            "type": "string",
            "description": "The instructions handed to the provider for this attempt."
        },
        "effect_state": {
            "type": "string",
            "enum": ["compound_non_repeatable", "intercepted", "observational"],
            "description": "How this attempt's effects are treated: observational, intercepted, or compound and non-repeatable."
        },
        "occurred_at": occurred_at()
    })
}

const START_ATTEMPT_REQUIRED: &[&str] = &[
    "task_id",
    "run_id",
    "attempt_id",
    "operation",
    "execution_snapshot",
    "worktree_root",
    "instructions",
    "effect_state",
    "occurred_at",
];

/// Build every Work definition from the canonical mounted operation order.
/// The operation descriptor remains the sole list: this match only selects the
/// request-specific schema description for each descriptor entry.
pub(super) fn work_definitions() -> Vec<ToolDefinition> {
    WorkOperation::ALL
        .into_iter()
        .map(|operation| match operation {
            WorkOperation::Snapshot => def_work_snapshot(),
            WorkOperation::Delta => def_work_delta(),
            WorkOperation::GenerateProposal => def_work_generate_proposal(),
            WorkOperation::Create => def_work_create(),
            WorkOperation::ReplanDependencies => def_work_replan_dependencies(),
            WorkOperation::ReviewProposal => def_work_review_proposal(),
            WorkOperation::AcceptProposal => def_work_accept_proposal(),
            WorkOperation::AdmitExecution => def_work_admit_execution(),
            WorkOperation::AttachRuntimeEvidence => def_work_attach_runtime_evidence(),
            WorkOperation::AcceptTask => def_work_accept_task(),
            WorkOperation::StartAttempt => def_work_start_attempt(),
            WorkOperation::Synthesize => def_work_synthesize(),
            WorkOperation::AttemptStatus => def_work_attempt_status(),
            WorkOperation::CancelAttempt => def_work_cancel_attempt(),
            WorkOperation::ResumeAttempts => def_work_resume_attempts(),
            WorkOperation::ListAttempts => def_work_list_attempts(),
            WorkOperation::HydrateArtifacts => def_work_hydrate_artifacts(),
            WorkOperation::Views => def_work_views(),
            WorkOperation::Topology => def_work_topology(),
            WorkOperation::PauseRun => def_work_pause_run(),
            WorkOperation::ResumeRun => def_work_resume_run(),
            WorkOperation::RunControl => def_work_run_control(),
            WorkOperation::PlacementPreflight => def_work_placement_preflight(),
            WorkOperation::AdmitPlacement => def_work_admit_placement(),
            WorkOperation::PlacementStatus => def_work_placement_status(),
            WorkOperation::ReleasePlacement => def_work_release_placement(),
        })
        .collect()
}

fn start_attempt_schema() -> Value {
    required_object_schema(start_attempt_properties(), START_ATTEMPT_REQUIRED)
}

/// The temporal mode a Work product graph read is answered under.
fn graph_read_mode() -> Value {
    json!({
        "description": "Which version of the Work product graph to read.",
        "oneOf": [
            {
                "type": "object",
                "properties": {"mode": {"const": "current"}},
                "required": ["mode"]
            },
            {
                "type": "object",
                "properties": {
                    "mode": {"const": "as_of"},
                    "valid_at": micros("The instant the graph was valid at.")
                },
                "required": ["mode", "valid_at"]
            },
            {
                "type": "object",
                "properties": {
                    "mode": {"const": "evolution"},
                    "from_valid_at": micros("Start of the validity window, inclusive."),
                    "through_valid_at": micros("End of the validity window.")
                },
                "required": ["mode", "from_valid_at", "through_valid_at"]
            },
            {
                "type": "object",
                "properties": {
                    "mode": {"const": "forensic"},
                    "from_observed_at": micros("Start of the observation window, inclusive."),
                    "through_observed_at": micros("End of the observation window.")
                },
                "required": ["mode", "from_observed_at", "through_observed_at"]
            }
        ]
    })
}

/// The relation subset one Work product graph read selects.
fn graph_read_selection() -> Value {
    json!({
        "description": "Which relations to read. `profile_owned_no_git` is an explicit no-Git selection, not an empty set that skips authorization.",
        "oneOf": [
            {
                "type": "object",
                "properties": {"selection": {"const": "profile_owned_no_git"}},
                "required": ["selection"]
            },
            {
                "type": "object",
                "properties": {
                    "selection": {"const": "relations"},
                    "relation_scopes": {
                        "type": "array",
                        "minItems": 1,
                        "items": {"type": "object"},
                        "description": "Authorized relation scopes, each an object with `kind` set to `project` (naming the registered project identity) or `repository` (naming the registered project and repository identities)."
                    }
                },
                "required": ["selection", "relation_scopes"]
            }
        ]
    })
}

/// The target a placement preflight or admission names.
fn placement_target() -> Value {
    let mut schema = required_object_schema(
        json!({
            "kind": {
                "type": "string",
                "enum": [
                    "no_managed_placement",
                    "clean_in_place",
                    "linked_worktree",
                    "isolated_clone"
                ],
                "description": "The placement shape: no managed checkout, the caller's own clean checkout, a linked worktree, or an isolated clone."
            },
            "root": {
                "type": ["string", "null"],
                "description": "Absolute root, required exactly for `linked_worktree` and `isolated_clone` and refused for the other kinds."
            },
            "in_place_acknowledged": {
                "type": "boolean",
                "description": "The caller states it accepts running in its own checkout. Required to be true for `clean_in_place`."
            },
            "network_free": {
                "type": "boolean",
                "description": "The placement declares it needs no network. Declared by the caller, never detected."
            }
        }),
        &["kind"],
    );
    schema["description"] = json!("The exact placement the run is asking for.");
    schema
}

fn run_control_reason() -> Value {
    json!({
        "type": "string",
        "enum": ["operator_request", "human_wait", "budget_exhausted", "recovery"],
        "description": "Why the transition happened, from a closed vocabulary a projection can read: an authorized operator asked, the run waits on a human, the budget ledger is exhausted, or recovery is reconciling."
    })
}

// ---------------------------------------------------------------------------
// Projection reads
// ---------------------------------------------------------------------------

pub(super) fn def_work_snapshot() -> ToolDefinition {
    def(
        "tracedecay_work_snapshot",
        "Read a Work projection snapshot",
        "Read one generation-bound page of the authorized Work projection: every task in scope with its dependencies, readiness, and version. Start here when you need the current shape of Work, then follow the snapshot's resume cursor with the delta read instead of re-reading the whole projection.",
        required_object_schema(json!({"page_size": projection_page_size()}), &["page_size"]),
    )
}

pub(super) fn def_work_delta() -> ToolDefinition {
    def(
        "tracedecay_work_delta",
        "Read Work projection changes since a cursor",
        "Read only the tasks that changed after a resume cursor from an earlier snapshot or delta page. Use this to stay current cheaply; a cursor minted under a superseded projection generation is refused as stale rather than silently answered from a different generation.",
        required_object_schema(
            json!({
                "cursor": projection_resume_cursor(),
                "page_size": projection_page_size()
            }),
            &["cursor", "page_size"],
        ),
    )
}

pub(super) fn def_work_generate_proposal() -> ToolDefinition {
    def(
        "tracedecay_work_generate_proposal",
        "Generate a Work proposal without changing anything",
        "Evaluate a task against the current Work version and return one explained proposal, changing nothing. Use it to see what the planner would decide next; the returned digest is what an acceptance must cite, so a stale or altered proposal cannot be accepted against a version that has moved.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "proposal_id": identity("Identity to mint this proposal under."),
                "live_git_evidence": {
                    "type": ["object", "null"],
                    "properties": {
                        "watermark": micros("How current the supplied Git evidence is."),
                        "digest": identity("Algorithm-tagged digest of the evidence frontier.")
                    },
                    "required": ["watermark", "digest"],
                    "description": "Optional live Git evidence frontier from the caller's own Git authority. It is never derived from Work history and never merged with the local frontier."
                },
                "occurred_at": occurred_at()
            }),
            &["task_id", "proposal_id", "occurred_at"],
        ),
    )
}

// ---------------------------------------------------------------------------
// Planning and admission
// ---------------------------------------------------------------------------

pub(super) fn def_work_create() -> ToolDefinition {
    def_rw(
        "tracedecay_work_create",
        "Create a Work task",
        "Record a new task in Work with its title and its dependency set. Use it to open a unit of work before anything is proposed, admitted, or executed against it.",
        required_object_schema(
            json!({
                "task_id": identity("Identity to create the task under. Creating the same identity twice is refused, not merged."),
                "title": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Human-readable title for the task."
                },
                "dependencies": dependencies(),
                "command_id": command_id(),
                "occurred_at": occurred_at()
            }),
            &["task_id", "title", "command_id", "occurred_at"],
        ),
    )
}

pub(super) fn def_work_replan_dependencies() -> ToolDefinition {
    def_rw(
        "tracedecay_work_replan_dependencies",
        "Replan a task's dependencies",
        "Replace a task's dependency set at an exact expected version. Use it when the plan around a task changes; the version guard refuses a replan computed against state that has since moved, so readiness is never recomputed from a stale graph.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "dependencies": dependencies(),
                "expected_version": expected_version(),
                "command_id": command_id(),
                "occurred_at": occurred_at()
            }),
            &["task_id", "expected_version", "command_id", "occurred_at"],
        ),
    )
}

pub(super) fn def_work_review_proposal() -> ToolDefinition {
    def_rw(
        "tracedecay_work_review_proposal",
        "Record a non-accepting proposal review",
        "Record that a generated proposal was rejected or superseded. Use it to close out a proposal without approving it — acceptance is a separate operation precisely so a review can never be collapsed into an approval.",
        required_object_schema(
            json!({
                "review": review_proposal_command(),
                "disposition": {
                    "type": "string",
                    "enum": ["rejected", "superseded"],
                    "description": "The non-accepting outcome: the proposal was rejected, or it was superseded by a later one."
                }
            }),
            &["review", "disposition"],
        ),
    )
}

pub(super) fn def_work_accept_proposal() -> ToolDefinition {
    def_rw(
        "tracedecay_work_accept_proposal",
        "Accept a Work proposal",
        "Accept a generated proposal so it becomes the task's plan. The cited digest binds acceptance to the exact evaluated decision content, so a proposal that was altered or generated against an older version is refused rather than accepted.",
        required_object_schema(json!({"review": review_proposal_command()}), &["review"]),
    )
}

pub(super) fn def_work_admit_execution() -> ToolDefinition {
    def_rw(
        "tracedecay_work_admit_execution",
        "Admit a task to execution",
        "Admit an accepted task to execution at an exact expected version. This is the gate every attempt is started behind: use it to move a task from planned to runnable.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "expected_version": expected_version(),
                "command_id": command_id(),
                "occurred_at": occurred_at()
            }),
            &["task_id", "expected_version", "command_id", "occurred_at"],
        ),
    )
}

pub(super) fn def_work_attach_runtime_evidence() -> ToolDefinition {
    def_rw(
        "tracedecay_work_attach_runtime_evidence",
        "Attach runtime evidence to a task",
        "Attach a runtime evidence reference — the run that produced it, its sealed evidence digest, and whether it is terminal — to a task. Use it to record what a run actually produced so a later acceptance can cite evidence instead of asserting an outcome.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "evidence": {
                    "type": "object",
                    "properties": {
                        "run_id": run_id(),
                        "evidence_digest": identity("Algorithm-tagged digest of the sealed evidence record."),
                        "terminal": {
                            "type": "boolean",
                            "description": "Whether this evidence is the run's terminal outcome rather than an interim observation."
                        }
                    },
                    "required": ["run_id", "evidence_digest", "terminal"],
                    "description": "The runtime evidence being attached."
                },
                "expected_version": expected_version(),
                "command_id": command_id(),
                "occurred_at": occurred_at()
            }),
            &[
                "task_id",
                "evidence",
                "expected_version",
                "command_id",
                "occurred_at",
            ],
        ),
    )
}

pub(super) fn def_work_accept_task() -> ToolDefinition {
    def_rw(
        "tracedecay_work_accept_task",
        "Accept a task as done",
        "Mark a task accepted at an exact expected version, closing it against the evidence already attached. Use it as the terminal transition once the work is finished and evidenced; it does not attach evidence of its own.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "expected_version": expected_version(),
                "command_id": command_id(),
                "occurred_at": occurred_at()
            }),
            &["task_id", "expected_version", "command_id", "occurred_at"],
        ),
    )
}

// ---------------------------------------------------------------------------
// Attempts
// ---------------------------------------------------------------------------

pub(super) fn def_work_start_attempt() -> ToolDefinition {
    def_rw(
        "tracedecay_work_start_attempt",
        "Start an admitted provider attempt",
        "Start one provider attempt against an admitted task's run. Every field is a typed fact — the resolved execution snapshot, the placement root, the commit and reference, the instructions, and the effect state — and there is no argv, environment entry, executable path, or shell string. The projection binding and admission facts are re-read from the canonical Work authority rather than trusted from the caller.",
        start_attempt_schema(),
    )
}

pub(super) fn def_work_synthesize() -> ToolDefinition {
    def_rw(
        "tracedecay_work_synthesize",
        "Admit a synthesis attempt over sibling attempts",
        "Admit one synthesis attempt that folds an ordered set of sibling attempts into a single cited result. The synthesis is started through the same admission machinery as any other attempt, and it is refused when no source contributed a citable artifact — there would be nothing it could truthfully cite.",
        required_object_schema(
            json!({
                "start": {
                    "type": "object",
                    "properties": start_attempt_properties(),
                    "required": START_ATTEMPT_REQUIRED,
                    "description": "The synthesis attempt's own admission facts, in the same shape as starting any other attempt."
                },
                "output_name": identity(
                    "The fan-out output name this synthesis belongs to, carried into the draft the workflow completion path verifies."
                ),
                "sources": {
                    "type": "array",
                    "minItems": 1,
                    "items": attempt_identity(),
                    "description": "The sibling attempts the synthesis consumes, in the order the caller wants them cited."
                }
            }),
            &["start", "output_name", "sources"],
        ),
    )
}

pub(super) fn def_work_attempt_status() -> ToolDefinition {
    def(
        "tracedecay_work_attempt_status",
        "Read one attempt's status",
        "Read the current state of one attempt by its task, run, and attempt identity: whether it is running, was cancelled, or has sealed terminal evidence. Use it to check a specific attempt you already know the identity of; use the attempt list to discover attempts.",
        attempt_identity(),
    )
}

pub(super) fn def_work_cancel_attempt() -> ToolDefinition {
    def_rw(
        "tracedecay_work_cancel_attempt",
        "Request cancellation of an attempt",
        "Ask for one in-flight attempt to be cancelled. The request identity makes the ask idempotent, and cancellation is completed through recovery and reported as an attempt state rather than assumed by the caller.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id(),
                "attempt_id": attempt_id(),
                "request_id": identity(
                    "Caller-minted cancellation request identity. Repeating it is the same request, not a second cancellation."
                ),
                "occurred_at": occurred_at()
            }),
            &[
                "task_id",
                "run_id",
                "attempt_id",
                "request_id",
                "occurred_at",
            ],
        ),
    )
}

pub(super) fn def_work_resume_attempts() -> ToolDefinition {
    def_rw(
        "tracedecay_work_resume_attempts",
        "Resume open attempts after a restart",
        "Fence every open attempt in the authorized scope onto a new epoch and report which now require recovery execution and which had an in-flight cancellation completed. Use it once after the runtime that owns the attempts restarts, not as a routine poll.",
        required_object_schema(json!({"occurred_at": occurred_at()}), &["occurred_at"]),
    )
}

pub(super) fn def_work_list_attempts() -> ToolDefinition {
    def(
        "tracedecay_work_list_attempts",
        "List Work attempts",
        "Read one page of attempts across the authorized scope in stable task, run, and attempt order, pinned to a verified Work topology generation. Use it to discover what has run. An authority with no Work at all is a distinct answer from an authorized but empty page.",
        required_object_schema(attempt_page_properties(), &["page_size"]),
    )
}

pub(super) fn def_work_hydrate_artifacts() -> ToolDefinition {
    def(
        "tracedecay_work_hydrate_artifacts",
        "Read attempt artifacts and sealed evidence",
        "Read one page of attempts together with the artifact references and terminal evidence they declared. Artifact bytes are never returned — references carry digest and byte length — so use it to collect and compare what attempts produced without fetching content.",
        required_object_schema(attempt_page_properties(), &["page_size"]),
    )
}

// ---------------------------------------------------------------------------
// Views and topology
// ---------------------------------------------------------------------------

pub(super) fn def_work_views() -> ToolDefinition {
    def(
        "tracedecay_work_views",
        "Read the Work product graph",
        "Read the Work product graph for an authorized relation selection, either as it is now, as of an instant, or across a validity or observation window. Use it for the relational view — Work joined to the products and relations it touches — where the projection reads give the flat task list.",
        required_object_schema(
            json!({
                "selection": graph_read_selection(),
                "mode": graph_read_mode(),
                "continuation": {
                    "type": ["string", "null"],
                    "description": "Opaque continuation from the previous page. Omit for the first page."
                },
                "observed_at": micros("When the caller is making this read, in microseconds since the Unix epoch.")
            }),
            &["selection", "mode", "observed_at"],
        ),
    )
}

pub(super) fn def_work_topology() -> ToolDefinition {
    def(
        "tracedecay_work_topology",
        "Read the Work execution topology",
        "Read one page of execution lanes: each distinct task and run pair with the attempts it carried and the durable placement it holds. Use it to see where Work is executing and which checkout each lane occupies; placement absence is reported as a state on the lane, never a dropped lane.",
        required_object_schema(attempt_page_properties(), &["page_size"]),
    )
}

// ---------------------------------------------------------------------------
// Run control
// ---------------------------------------------------------------------------

pub(super) fn def_work_pause_run() -> ToolDefinition {
    def_rw(
        "tracedecay_work_pause_run",
        "Pause a Work run",
        "Pause a run for a declared reason. The expected authority version makes the pause a compare-and-swap: omit it only when no control transition has ever been published for the run, and expect a conflict rather than an overwrite if the control state has moved.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id(),
                "reason": run_control_reason(),
                "expected_authority_version": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "The control authority version the caller read. Omit or null only when no control row has been published yet."
                },
                "occurred_at": occurred_at()
            }),
            &["task_id", "run_id", "reason", "occurred_at"],
        ),
    )
}

pub(super) fn def_work_resume_run() -> ToolDefinition {
    def_rw(
        "tracedecay_work_resume_run",
        "Resume a paused Work run",
        "Resume a paused run for a declared reason at the exact control authority version the caller read. Read the run control state first: resuming always cites a published control row, so the version is not optional here.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id(),
                "reason": run_control_reason(),
                "expected_authority_version": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "The control authority version the caller read. A mismatch is a conflict, never an overwrite."
                },
                "occurred_at": occurred_at()
            }),
            &[
                "task_id",
                "run_id",
                "reason",
                "expected_authority_version",
                "occurred_at",
            ],
        ),
    )
}

pub(super) fn def_work_run_control() -> ToolDefinition {
    def(
        "tracedecay_work_run_control",
        "Read a run's control state",
        "Read whether a run is uncontrolled — admitted and running under its admitted deadline with no control transition ever published — or sitting under a published pause or resume, with the authority version to cite when transitioning it. The two answers are distinct; use this before pausing or resuming.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id()
            }),
            &["task_id", "run_id"],
        ),
    )
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

pub(super) fn def_work_placement_preflight() -> ToolDefinition {
    def(
        "tracedecay_work_placement_preflight",
        "Check whether a run may take a placement",
        "Evaluate, without changing anything, whether a run may take a placement on a target, and report the exact blockers standing in the way — dirty tracked files, untracked data, unique commits, an active holder, unresolved effects, an unacknowledged receipt, a lost or stale authorization, or an unreadable target. Use it before admitting a placement and before treating a target as reusable.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id(),
                "target": placement_target(),
                "occurred_at": occurred_at()
            }),
            &["task_id", "run_id", "target", "occurred_at"],
        ),
    )
}

pub(super) fn def_work_admit_placement() -> ToolDefinition {
    def_rw(
        "tracedecay_work_admit_placement",
        "Admit a run's placement",
        "Admit a run's placement on a target: no managed checkout, the caller's acknowledged clean checkout, a linked worktree, or an isolated clone. Run the preflight first — an exclusive target already held by another admitted placement is refused. Retention eligibility recorded here is a schedule for a future cleanup preflight, not delete authority.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id(),
                "target": placement_target(),
                "retention_eligible_at": {
                    "type": ["integer", "null"],
                    "description": "When retention makes this placement eligible for a fresh cleanup preflight. Eligibility is not delete authority."
                },
                "occurred_at": occurred_at()
            }),
            &["task_id", "run_id", "target", "occurred_at"],
        ),
    )
}

pub(super) fn def_work_placement_status() -> ToolDefinition {
    def(
        "tracedecay_work_placement_status",
        "Read a run's placement",
        "Read the durable placement a run holds, including the target it occupies and the authority version to cite when releasing it. A run that never had a placement admitted is reported as absent — a state, not an empty placement.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id()
            }),
            &["task_id", "run_id"],
        ),
    )
}

pub(super) fn def_work_release_placement() -> ToolDefinition {
    def_rw(
        "tracedecay_work_release_placement",
        "Release a run's placement",
        "Release a run's placement at the exact authority version the caller read, so the target stops being held and can be claimed again. Use it when a run is finished with its checkout; releasing does not delete the target, and a version mismatch is refused as a conflict.",
        required_object_schema(
            json!({
                "task_id": task_id(),
                "run_id": run_id(),
                "expected_authority_version": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "The placement authority version the caller read. A mismatch is a conflict, never an overwrite."
                },
                "occurred_at": occurred_at()
            }),
            &[
                "task_id",
                "run_id",
                "expected_authority_version",
                "occurred_at",
            ],
        ),
    )
}
