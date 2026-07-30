import type { DomainStateKind } from '../../ui/StateChip.tsx';

/**
 * What Work owes, and what this build is able to honour.
 *
 * Work is the canonical task graph and the execution runtime over it. Almost
 * none of it can be drawn yet: `DashboardContractCatalogV1` carries no Work
 * payload, so the generated contracts module every dashboard read is validated
 * against holds no Work read model and no Work command. The one exception is
 * the task-activity stream, which is carried by the daemon's event union rather
 * than by that catalog and so is reachable without one.
 *
 * The rows below are therefore a derivation of plan scope, not a reading of
 * backend state. The only claim they make about the running daemon is the one
 * this build can prove about itself — that it carries no contract able to
 * represent them. `authority.test.ts` holds that proof and fails the moment a
 * Work contract is generated, so this ledger cannot outlive its own truth.
 */

/** Why one Work surface cannot be read yet. Each value is a different missing
 * piece of the wire boundary, and they are not interchangeable: a projection
 * needs a read model, an action needs a command, a live view needs a stream.
 *
 * `runtime_not_mounted` is the one that is not an absence. The piece exists and
 * this build consumes it; what is missing is anything for it to act on. It is a
 * partial rather than an unsupported state, because reporting a working
 * subscription as unsupported would understate the build as badly as the
 * reverse would overstate it. */
export type WithheldReason =
  | 'read_model_absent'
  | 'command_absent'
  | 'stream_absent'
  | 'runtime_not_mounted';

export interface WithheldSurface {
  readonly id: string;
  /** The projection, command or stream, named as the design authority names it. */
  readonly name: string;
  /** What it would draw once its contract exists — the shape of the absence,
   * so a reviewer can see what is missing rather than only that something is. */
  readonly draws: string;
  /** The generated contract it reads or writes through, named the way the design
   * authority names it. A label for the reader, not a lookup key. */
  readonly requires: string;
  /**
   * The schemars type names whose arrival would satisfy this row, matched by
   * prefix so a `V1` suffix and the `Schema` const beside it both count.
   *
   * Explicit, because the design name and the implemented name are not the same
   * string: the row labelled `WorkProjection` is served by
   * `WorkProjectionSnapshotV1`, and the one labelled `ExecutionAdmission` by
   * `AdmitExecutionCommand`. Deriving the key from `requires` would watch for
   * names nothing will ever emit, and a gate that cannot see its contract arrive
   * is worse than no gate — it would leave this page claiming absence over live
   * data.
   *
   * Every name here is one that exists in the workspace today. Watching a
   * plausible spelling that no crate defines buys nothing and costs the same
   * silence as watching nothing: the deleted `WorkSnapshotV1` and `WorkDeltaV1`
   * were watched for exactly that reason until `1fc31a865` proved they were
   * gone.
   */
  readonly watches: readonly string[];
  readonly reason: WithheldReason;
}

export interface WithheldGroup {
  readonly id: string;
  readonly legend: string;
  readonly surfaces: readonly WithheldSurface[];
}

export interface WithheldPresentation {
  /** The domain state the row carries. `unsupported_schema` is the read side:
   * a payload this build could not represent even if it arrived. `unsupported`
   * is the write and stream side: nothing to call at all. */
  readonly state: DomainStateKind;
  readonly summary: string;
}

export function withheldPresentation(reason: WithheldReason): WithheldPresentation {
  switch (reason) {
    case 'read_model_absent':
      return { state: 'unsupported_schema', summary: 'no generated read model' };
    case 'command_absent':
      return { state: 'unsupported', summary: 'no generated command' };
    case 'stream_absent':
      return { state: 'unsupported', summary: 'no registered stream' };
    case 'runtime_not_mounted':
      return { state: 'partial', summary: 'subscribed, with no projection to refetch' };
    default: {
      const unhandled: never = reason;
      return unhandled;
    }
  }
}

/**
 * The names a landed read contract will arrive under.
 *
 * There is one spelling for each, and these are it: `crates/tracedecay-domain/
 * src/work_read.rs` defines `WorkProjectionSnapshotV1` and
 * `WorkProjectionDeltaV1`, both generation-bound and carrying their own sequence
 * and coverage. The competing `WorkSnapshotV1` and `WorkDeltaV1` were deleted by
 * `1fc31a865`, so watching them would be watching for a type no crate can emit.
 *
 * Held as prefixes rather than exact names so a `V1`, a later revision, the
 * request type beside each and the zod schema const all count as the same
 * arrival. Deliberately not the bare `WorkProjection`: it is the per-task domain
 * type every one of these wraps, so watching it would collapse every projection
 * row onto whichever contract landed first.
 */
const SNAPSHOT_NAMES = ['WorkProjectionSnapshot'] as const;
const DELTA_NAMES = ['WorkProjectionDelta'] as const;

/**
 * The runtime aggregate, from the canonical runtime contracts.
 *
 * `WorkAttemptV1` is the one payload the execution rows all read: it carries the
 * lease fence, the requested route beside the route actually taken, progress,
 * artifacts, cancellation and recovery state, and the terminal evidence. Rows
 * that need only one of those also watch the leaf, since a leaf can be
 * registered before the aggregate is.
 */
const ATTEMPT_NAMES = ['WorkAttempt'] as const;

/**
 * Everything Work is made of.
 *
 * Held as data rather than markup so the ledger, the tests and the wired
 * surfaces that replace them enumerate one set once. As each contract lands,
 * its row leaves this list and becomes a read.
 */
export const WITHHELD_WORK: readonly WithheldGroup[] = [
  {
    id: 'projections',
    legend: 'projections over the canonical graph',
    surfaces: [
      {
        id: 'kanban',
        name: 'Kanban',
        draws: 'triage, ready, running, blocked, review and done lanes derived from immutable history',
        requires: 'WorkProjection',
        watches: SNAPSHOT_NAMES,
        reason: 'read_model_absent',
      },
      {
        id: 'dag',
        name: 'Dependency DAG',
        draws: 'gating edges, cycles and supersession, kept distinct from the branch stack',
        requires: 'WorkProjection',
        watches: SNAPSHOT_NAMES,
        reason: 'read_model_absent',
      },
      {
        id: 'timeline',
        name: 'Timeline',
        draws: 'one task’s event order at a chosen graph version',
        requires: 'WorkEvent',
        // Both names, because the match is anchored: `WorkEvent` no longer
        // subsumes the `WorkEventKind` union beside it, and the row should fire
        // whether the ordered event or its discriminant lands first.
        watches: [...DELTA_NAMES, 'WorkEvent', 'WorkEventKind'],
        reason: 'read_model_absent',
      },
      {
        id: 'causal',
        name: 'Causal',
        draws: 'declared baselines, and correlation narrowed where causation is not claimed',
        requires: 'WorkProjection',
        watches: SNAPSHOT_NAMES,
        reason: 'read_model_absent',
      },
      {
        id: 'critical-path',
        name: 'Critical path',
        draws: 'the gating chain through the accepted graph, with its blockers',
        requires: 'WorkProjection',
        watches: SNAPSHOT_NAMES,
        reason: 'read_model_absent',
      },
      {
        id: 'workload',
        name: 'Workload and capacity',
        draws: 'active and deferred work, requested against actual concurrency, queue and defer reasons, deadlines and budgets',
        requires: 'WorkProjection',
        watches: [...SNAPSHOT_NAMES, ...ATTEMPT_NAMES],
        reason: 'read_model_absent',
      },
      {
        id: 'executor',
        name: 'Executor and model',
        draws: 'recommendation evidence beside the route actually taken, without prompts or credentials',
        requires: 'WorkProviderRoute',
        watches: [...ATTEMPT_NAMES, 'WorkProviderRoute'],
        reason: 'read_model_absent',
      },
      {
        id: 'repository',
        name: 'Repository and delivery',
        draws: 'the repository, worktree and exact commits a task requires and produces',
        requires: 'WorkArtifactRef',
        watches: [...ATTEMPT_NAMES, 'WorkArtifactRef'],
        reason: 'read_model_absent',
      },
      {
        id: 'evidence',
        name: 'Evidence',
        draws: 'TaskId-rooted bounded evidence with its coverage, unknowns and anchors',
        requires: 'WorkProjectionCoverage',
        watches: [...SNAPSHOT_NAMES, 'WorkProjectionCoverage', 'WorkTerminalEvidence'],
        reason: 'read_model_absent',
      },
      {
        id: 'history',
        name: 'History',
        draws: 'current, as-of, evolution and forensic reads of one TaskId',
        requires: 'WorkEvent',
        watches: [...DELTA_NAMES, 'WorkProjectionResumeCursor', 'WorkEvent', 'WorkEventKind'],
        reason: 'read_model_absent',
      },
    ],
  },
  {
    id: 'commands',
    legend: 'commands, each separately authorized',
    surfaces: [
      {
        id: 'graph-change',
        name: 'Create and change work',
        draws: 'expected-version graph writes with idempotent receipts',
        requires: 'CreateWorkCommand',
        watches: ['CreateWorkCommand', 'ReplanDependenciesCommand', 'AcceptTaskCommand'],
        reason: 'command_absent',
      },
      {
        id: 'proposal-review',
        name: 'Review a proposal',
        draws: 'accept, reject or supersede an explained proposal under graph and evidence CAS',
        requires: 'GeneratedWorkProposal',
        watches: [
          'ReviewProposalCommand',
          'AcceptProposalCommand',
          'GeneratedWorkProposal',
          'GenerateProposalRequest',
        ],
        reason: 'command_absent',
      },
      {
        id: 'admission',
        name: 'Admit execution',
        draws: 'a separate admission naming scope, provider, grants, budget and deadline',
        requires: 'AdmitExecutionCommand',
        watches: ['AdmitExecutionCommand'],
        reason: 'command_absent',
      },
      {
        id: 'run-control',
        name: 'Control a run',
        draws: 'cancel, resume and restart against a fenced lease and attempt',
        requires: 'WorkCancellationRequest',
        // No longer design-only: the canonical runtime contracts carry
        // cancellation request, acknowledgement and escalation, recovery state,
        // and the lease fence every one of them is fenced against.
        // Named to the exact stems rather than the `WorkCancellation` family:
        // anchored matching aside, that shorter stem also reaches
        // `WorkCancellationRequestId`, and an identifier is not the payload
        // this row needs.
        watches: [
          ...ATTEMPT_NAMES,
          'WorkCancellationRequest',
          'WorkCancellationState',
          'WorkRecoveryState',
          'WorkLeaseFence',
        ],
        reason: 'command_absent',
      },
      {
        id: 'acceptance',
        name: 'Accept an outcome',
        draws: 'sealed terminal evidence, accepted, rejected or replanned as its own step',
        requires: 'WorkTerminalEvidence',
        watches: ['WorkTerminalEvidence', 'AttachRuntimeEvidenceCommand'],
        reason: 'command_absent',
      },
    ],
  },
  {
    id: 'streams',
    legend: 'live activity',
    surfaces: [
      {
        id: 'task-activity',
        name: 'Task activity',
        draws: 'lease, attempt and progress changes as they land, under a bounded monotone reducer',
        requires: 'task_activity',
        // A typed stream arrives as a variant of the daemon's event-kind union
        // rather than as a payload of its own, so the union is one of the two
        // things this row waits on: without it the dashboard cannot name any
        // stream to subscribe to, let alone this one. The other is the progress
        // payload such a stream would carry.
        watches: ['DashboardEventKind', 'WorkAttemptProgress'],
        // Not absent any more. The daemon enumerates the `task_activity` family
        // and emits it under a canonical stream name, and this build subscribes
        // to it, so the row reports a live subscription over a read model that
        // does not exist yet rather than a missing stream.
        reason: 'runtime_not_mounted',
      },
    ],
  },
] as const;

/** The single wire boundary a Work payload has to enter through. Named on the
 * surface because "the contract is missing" is only actionable if the reader
 * knows which catalog is missing it. */
export const WIRE_AUTHORITY = 'DashboardContractCatalogV1';
