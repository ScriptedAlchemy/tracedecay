import type { DomainStateKind } from '../../ui/StateChip.tsx';

/**
 * What Work owes, and what this build is able to honour.
 *
 * Work is the canonical task graph and the execution runtime over it. None of it
 * can be drawn yet: `DashboardContractCatalogV1` carries no Work payload, so the
 * generated contracts module every dashboard read is validated against holds no
 * Work read model, no Work command and no task-activity stream.
 *
 * The rows below are therefore a derivation of plan scope, not a reading of
 * backend state. The only claim they make about the running daemon is the one
 * this build can prove about itself — that it carries no contract able to
 * represent them. `authority.test.ts` holds that proof and fails the moment a
 * Work contract is generated, so this ledger cannot outlive its own truth.
 */

/** Why one Work surface is withheld. Each value is a different missing piece of
 * the wire boundary, and they are not interchangeable: a projection needs a
 * read model, an action needs a command, a live view needs a stream. */
export type WithheldReason = 'read_model_absent' | 'command_absent' | 'stream_absent';

export interface WithheldSurface {
  readonly id: string;
  /** The projection, command or stream, named as the design authority names it. */
  readonly name: string;
  /** What it would draw once its contract exists — the shape of the absence,
   * so a reviewer can see what is missing rather than only that something is. */
  readonly draws: string;
  /** The generated contract it reads or writes through. */
  readonly requires: string;
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
    default: {
      const unhandled: never = reason;
      return unhandled;
    }
  }
}

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
        reason: 'read_model_absent',
      },
      {
        id: 'dag',
        name: 'Dependency DAG',
        draws: 'gating edges, cycles and supersession, kept distinct from the branch stack',
        requires: 'WorkProjection',
        reason: 'read_model_absent',
      },
      {
        id: 'timeline',
        name: 'Timeline',
        draws: 'one task’s event order at a chosen graph version',
        requires: 'WorkEvent',
        reason: 'read_model_absent',
      },
      {
        id: 'causal',
        name: 'Causal',
        draws: 'declared baselines, and correlation narrowed where causation is not claimed',
        requires: 'WorkProjection',
        reason: 'read_model_absent',
      },
      {
        id: 'critical-path',
        name: 'Critical path',
        draws: 'the gating chain through the accepted graph, with its blockers',
        requires: 'WorkProjection',
        reason: 'read_model_absent',
      },
      {
        id: 'workload',
        name: 'Workload and capacity',
        draws: 'active and deferred work, requested against actual concurrency, queue and defer reasons, deadlines and budgets',
        requires: 'WorkProjection',
        reason: 'read_model_absent',
      },
      {
        id: 'executor',
        name: 'Executor and model',
        draws: 'recommendation evidence beside the route actually taken, without prompts or credentials',
        requires: 'WorkProjection',
        reason: 'read_model_absent',
      },
      {
        id: 'repository',
        name: 'Repository and delivery',
        draws: 'the repository, worktree and exact commits a task requires and produces',
        requires: 'WorkProjection',
        reason: 'read_model_absent',
      },
      {
        id: 'evidence',
        name: 'Evidence',
        draws: 'TaskId-rooted bounded evidence with its coverage, unknowns and anchors',
        requires: 'WorkProjection',
        reason: 'read_model_absent',
      },
      {
        id: 'history',
        name: 'History',
        draws: 'current, as-of, evolution and forensic reads of one TaskId',
        requires: 'WorkEvent',
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
        requires: 'WorkCommand',
        reason: 'command_absent',
      },
      {
        id: 'proposal-review',
        name: 'Review a proposal',
        draws: 'accept, reject or supersede an explained proposal under graph and evidence CAS',
        requires: 'WorkProposal',
        reason: 'command_absent',
      },
      {
        id: 'admission',
        name: 'Admit execution',
        draws: 'a separate admission naming scope, provider, grants, budget and deadline',
        requires: 'ExecutionAdmission',
        reason: 'command_absent',
      },
      {
        id: 'run-control',
        name: 'Control a run',
        draws: 'cancel, resume and restart against a fenced lease and attempt',
        requires: 'RunControl',
        reason: 'command_absent',
      },
      {
        id: 'acceptance',
        name: 'Accept an outcome',
        draws: 'sealed terminal evidence, accepted, rejected or replanned as its own step',
        requires: 'TerminalEvidence',
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
        reason: 'stream_absent',
      },
    ],
  },
] as const;

/** The single wire boundary a Work payload has to enter through. Named on the
 * surface because "the contract is missing" is only actionable if the reader
 * knows which catalog is missing it. */
export const WIRE_AUTHORITY = 'DashboardContractCatalogV1';
