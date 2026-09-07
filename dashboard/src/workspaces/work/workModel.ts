import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkTaskCoverage, WorkTaskView } from './workProductView.ts';

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
export function workStage(projection: WorkTaskView, terminal = false): WorkStage {
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
 * How much of the board this page is actually showing.
 *
 * The current product graph is one complete, immutable head rather than a
 * paginated projection. `returned` and `total` are therefore identical; the
 * explicit count still distinguishes an empty graph from an unread one.
 */
export function coverageReading(coverage: WorkTaskCoverage): {
  state: DomainStateKind;
  detail: string;
} {
  return {
    state: coverage.returned === 0 ? 'complete_zero_findings' : 'ready',
    detail: `${coverage.returned} of ${coverage.total}`,
  };
}
