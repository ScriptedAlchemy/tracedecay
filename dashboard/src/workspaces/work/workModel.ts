import type { WorkProjection, WorkProjectionCoverageV1 } from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

/**
 * What a Work projection says about itself.
 *
 * Every stage below is read off a field the contract actually carries —
 * `accepted_proposal`, `task_accepted`, `execution_admitted`, and whether the
 * graph version's runtime projection contains a terminal attempt. Nothing here
 * is a lane.
 *
 * That distinction is the whole point. This page previously promised a Kanban
 * board with triage, ready, running, blocked, review and done columns, and
 * `WorkProjection` has no status field to put a task in any of them. Drawing
 * those columns would mean choosing a lane per task by rule, and the rule — not
 * the daemon — would be deciding what "blocked" meant. The progression below is
 * the one the contract does encode, so a task's stage is a reading rather than
 * an interpretation.
 */
export type WorkStage =
  | 'proposal_open'
  | 'proposal_accepted'
  | 'task_accepted'
  | 'execution_admitted'
  | 'evidence_terminal';

export const WORK_STAGES: readonly WorkStage[] = [
  'proposal_open',
  'proposal_accepted',
  'task_accepted',
  'execution_admitted',
  'evidence_terminal',
];

/** Read in reverse: the furthest gate a task has passed is its stage. The
 * fields are cumulative in the domain — execution is not admitted before the
 * task is accepted — so the first match walking back is the true one. */
export function workStage(projection: WorkProjection, terminal = false): WorkStage {
  if (terminal) return 'evidence_terminal';
  if (projection.execution_admitted) return 'execution_admitted';
  if (projection.task_accepted) return 'task_accepted';
  if (projection.accepted_proposal !== null) return 'proposal_accepted';
  return 'proposal_open';
}

export function stageLabel(stage: WorkStage): string {
  switch (stage) {
    case 'proposal_open':
      return 'Proposal open';
    case 'proposal_accepted':
      return 'Proposal accepted';
    case 'task_accepted':
      return 'Task accepted';
    case 'execution_admitted':
      return 'Execution admitted';
    case 'evidence_terminal':
      return 'Terminal evidence';
    default: {
      const unhandled: never = stage;
      return unhandled;
    }
  }
}

/**
 * The state a stage reads as.
 *
 * `evidence_terminal` is `ready` because the daemon has recorded a terminal
 * result for it; everything earlier is `partial`, since the task exists and has
 * not finished. Nothing here is `complete_zero_findings` — that would say the
 * task produced nothing, which is not something a stage can tell us.
 */
export function stageState(stage: WorkStage): DomainStateKind {
  return stage === 'evidence_terminal' ? 'ready' : 'partial';
}

/**
 * Which of the seven commands this build may offer for one task.
 *
 * Gated on the projection's own fields so a control is only drawn where the
 * daemon could act on it — offering "admit execution" for a task whose proposal
 * is still open would be a button whose only possible outcome is a refusal.
 *
 * `create` is absent because it does not act on an existing task.
 */
export function availableCommands(projection: WorkProjection): readonly WorkCommandKind[] {
  const stage = workStage(projection);
  const commands: WorkCommandKind[] = ['replan_dependencies'];
  if (stage === 'proposal_open') {
    commands.push('review_proposal', 'accept_proposal');
  }
  if (!projection.task_accepted) commands.push('accept_task');
  if (projection.task_accepted && !projection.execution_admitted) commands.push('admit_execution');
  if (projection.execution_admitted) commands.push('attach_runtime_evidence');
  return commands;
}

export type WorkCommandKind =
  | 'replan_dependencies'
  | 'review_proposal'
  | 'accept_proposal'
  | 'accept_task'
  | 'admit_execution'
  | 'attach_runtime_evidence';

/**
 * Whether this build can assemble a command, as distinct from whether the
 * daemon would accept one.
 *
 * `availableCommands` answers the domain question: has the task reached the
 * gate this command acts on. This answers the dashboard's own question: are the
 * command's inputs anywhere in a generated read model.
 *
 * Three of the seven fail that test, and they fail it the same way. Reviewing a
 * proposal needs `proposal_id` and `proposal_digest`; accepting one needs the
 * same; attaching evidence needs `run_id` and `evidence_digest`. `WorkProjection`
 * carries `accepted_proposal` — the proposal already chosen — and a list of
 * evidence already attached, and no contract in this build enumerates the
 * pending proposals or the runs. A control for them could only ask an operator
 * to type an opaque digest, or mint one, and a minted digest is a fabricated
 * authority record aimed at the daemon's own audit trail.
 *
 * So they are named and explained rather than drawn. The gap is in the read
 * model, not in the command surface, and saying which is what lets it be closed.
 */
export function commandBlocked(kind: WorkCommandKind): string | undefined {
  switch (kind) {
    case 'review_proposal':
    case 'accept_proposal':
      return 'no generated contract lists the pending proposals, so this build has no proposal identity or digest to send';
    case 'attach_runtime_evidence':
      return 'no generated contract lists runs or their evidence digests, so this build has nothing to attach';
    case 'replan_dependencies':
    case 'accept_task':
    case 'admit_execution':
      return undefined;
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/**
 * How much of the board this page is actually showing.
 *
 * `returned` and `total` come from the daemon. A capped or partial reading is
 * reported as `partial` and never rounded up to a complete board, because the
 * difference between "these are the tasks" and "these are some of the tasks" is
 * the difference this surface exists to keep.
 */
export function coverageReading(coverage: WorkProjectionCoverageV1): {
  state: DomainStateKind;
  detail: string;
} {
  switch (coverage.state) {
    case 'complete':
      return {
        state: coverage.returned === 0 ? 'complete_zero_findings' : 'ready',
        detail: `${coverage.returned} of ${coverage.total}`,
      };
    case 'capped':
      return {
        state: 'partial',
        detail: `${coverage.returned} of ${coverage.total}, capped at ${coverage.cap}`,
      };
    case 'partial':
      return { state: 'partial', detail: `${coverage.returned} of ${coverage.total}` };
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}
