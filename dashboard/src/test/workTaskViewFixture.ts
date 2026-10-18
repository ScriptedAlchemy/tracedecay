import type { WorkTaskView } from '../workspaces/work/workProductView.ts';

/**
 * One `WorkTaskView` for the model tests, with every authority field given a
 * neutral default: no lane card, no relations, no gates passed, unit effort.
 *
 * The model tests build projections directly rather than through
 * `currentWorkProductView`, so the fields that view model adds must be
 * defaulted in one place or every test file grows its own copy of them.
 */
export function workTaskView(overrides: Partial<WorkTaskView> = {}): WorkTaskView {
  return {
    accepted_proposal: null,
    acceptance_evidence_required: false,
    dependencies: [],
    execution_admitted: false,
    history_len: 1,
    relation_replan: null,
    task_accepted: false,
    task_id: 'task',
    title: 'Task',
    version: 1,
    lane: { kind: 'uncarded' },
    legal_actions: [],
    effort: 1,
    hierarchy: { initiative_id: 'initiative-1', plan_id: 'plan-1', milestone_id: 'milestone-1' },
    informational_relations: [],
    causal_candidates: [],
    created_at: 1_800_000_000_000_000,
    updated_at: 1_800_000_000_000_000,
    scheduled_at: null,
    deadline: null,
    accepted_at: null,
    execution_admitted_at: null,
    archived_at: null,
    acceptance_criteria_count: 0,
    accepted_attempts_count: 0,
    evidence_links_count: 0,
    handoffs: [],
    ...overrides,
  };
}
