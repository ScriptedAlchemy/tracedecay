import type {
  WorkGraphReadV1,
  WorkGraphVersionEntryV1,
  WorkItemV1,
  WorkLegalActionV1,
  WorkTimelineLaneV1,
} from '../../contracts/index.ts';
import type { WorkResult } from './workApi.ts';

/**
 * The typed lifecycle state the Work authority projects for one task.
 *
 * `WorkKanbanProjectionV1` is derived by the daemon from the immutable graph
 * version and its runtime projection (`work_product_projection.rs::lane`). The
 * browser reads the lane it was handed; it never chooses one. A task the bundle
 * did not card is an absence with its own reason, not a `todo`.
 */
export type WorkTaskLane =
  | { readonly kind: 'projected'; readonly lane: WorkTimelineLaneV1 }
  | { readonly kind: 'uncarded' };

/** One recorded handoff, reduced to the two actors it names. */
export interface WorkTaskHandoffView {
  readonly handoffId: string;
  readonly fromActor: string;
  readonly toActor: string;
  readonly handedOffAt: number;
}

/** One task as rendered by the Work workspace, projected from one exact
 * `WorkProductGraphV1` version. This is a local view model, not a second wire
 * contract. */
export interface WorkTaskView {
  readonly accepted_proposal: string | null;
  readonly acceptance_evidence_required: boolean;
  readonly dependencies: readonly string[];
  readonly execution_admitted: boolean;
  /** The product graph does not publish a per-task event count. */
  readonly history_len: number | null;
  /** An accepted replan is applicable only on the graph version immediately
   * after its decision. Later versions prove that proposal is no longer a
   * legal mutation against the current head. */
  readonly relation_replan: {
    readonly proposal_id: string;
    readonly dependencies: readonly string[];
    readonly informational_relations: readonly string[];
    readonly causal_candidates: readonly string[];
  } | null;
  readonly task_accepted: boolean;
  readonly task_id: string;
  readonly title: string;
  readonly version: number;

  /** The authority's projected lane for this task, or its typed absence. */
  readonly lane: WorkTaskLane;
  /** The actions the authority's kanban card lists as legal. Informational:
   * the daemon still adjudicates every prepared command. */
  readonly legal_actions: readonly WorkLegalActionV1[];
  /** Declared effort, an integer the plan wrote down. It is not a duration. */
  readonly effort: number;
  readonly hierarchy: {
    readonly initiative_id: string;
    readonly plan_id: string;
    readonly milestone_id: string;
  };
  /** Soft relations the plan declares: informational relations name related
   * tasks without gating them; causal candidates nominate a possible cause. */
  readonly informational_relations: readonly string[];
  readonly causal_candidates: readonly string[];
  readonly created_at: number;
  readonly updated_at: number;
  readonly scheduled_at: number | null;
  readonly deadline: number | null;
  readonly accepted_at: number | null;
  readonly execution_admitted_at: number | null;
  readonly archived_at: number | null;
  readonly acceptance_criteria_count: number;
  readonly accepted_attempts_count: number;
  readonly evidence_links_count: number;
  readonly handoffs: readonly WorkTaskHandoffView[];
}

export interface WorkTaskCoverage {
  readonly state: 'complete';
  readonly returned: number;
  readonly total: number;
}

/** The fields shared by every Work camera, all derived from the same current
 * product-graph entry. */
export interface WorkProductView {
  readonly coverage: WorkTaskCoverage;
  readonly generation_id: string;
  readonly projections: readonly WorkTaskView[];
  readonly sequence: number;
  /** The immutable graph version and the event sequence that verified it. */
  readonly graph_version: number;
  readonly event_sequence: number;
  readonly observed_at: number;
}

function taskLane(
  entry: WorkGraphVersionEntryV1,
  taskId: string,
): { lane: WorkTaskLane; legal: readonly WorkLegalActionV1[] } {
  const card = entry.projections.kanban.cards.find((candidate) => candidate.task_id === taskId);
  if (card === undefined) return { lane: { kind: 'uncarded' }, legal: [] };
  return { lane: { kind: 'projected', lane: card.lane }, legal: card.legal_actions };
}

function taskView(entry: WorkGraphVersionEntryV1, item: WorkItemV1): WorkTaskView {
  const applicableReplan = entry.graph.relation_replan_decisions.find(
    (decision) =>
      decision.disposition === 'accepted' &&
      decision.proposal.task_id === item.input.task_id &&
      decision.proposal.based_on_version + 1 === entry.graph.version,
  )?.proposal;
  const { lane, legal } = taskLane(entry, item.input.task_id);
  return {
    accepted_proposal: item.accepted_proposal,
    acceptance_evidence_required: item.input.acceptance_criteria.some(
      (criterion) => criterion.evidence_required,
    ),
    dependencies: item.input.dependencies,
    execution_admitted: item.execution_admitted_at !== null,
    history_len: null,
    relation_replan:
      applicableReplan === undefined
        ? null
        : {
            proposal_id: applicableReplan.proposal_id,
            dependencies: applicableReplan.dependencies,
            informational_relations: applicableReplan.informational_relations,
            causal_candidates: applicableReplan.causal_candidates,
          },
    task_accepted: item.accepted_at !== null,
    task_id: item.input.task_id,
    title: item.input.title,
    version: entry.graph.version,
    lane,
    legal_actions: legal,
    effort: item.input.effort,
    hierarchy: item.input.hierarchy,
    informational_relations: item.input.informational_relations,
    causal_candidates: item.input.causal_candidates,
    created_at: item.input.created_at,
    updated_at: item.input.updated_at,
    scheduled_at: item.input.scheduled_at,
    deadline: item.input.deadline,
    accepted_at: item.accepted_at,
    execution_admitted_at: item.execution_admitted_at,
    archived_at: item.archived_at,
    acceptance_criteria_count: item.input.acceptance_criteria.length,
    accepted_attempts_count: item.accepted_attempts.length,
    evidence_links_count: item.evidence_links.length,
    handoffs: item.handoffs.map((handoff) => ({
      handoffId: handoff.handoff_id,
      fromActor: handoff.from_actor,
      toActor: handoff.to_actor,
      handedOffAt: handoff.handed_off_at,
    })),
  };
}

/** Reduce the current product graph to the local camera model without
 * inventing data the product authority does not publish. */
export function currentWorkProductView(
  result: WorkResult<WorkGraphReadV1> | undefined,
): WorkResult<WorkProductView> | undefined {
  if (result === undefined || result.outcome === 'refused') return result;
  if (result.value.mode !== 'current') {
    return {
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the current Work view received a non-current product graph',
    };
  }

  const entry = result.value.snapshot;
  const projections = entry.graph.items.map((item) => taskView(entry, item));

  return {
    outcome: 'value',
    scope: result.scope,
    value: {
      coverage: {
        state: 'complete',
        returned: projections.length,
        total: projections.length,
      },
      generation_id: entry.runtime.generation_id,
      projections,
      sequence: entry.runtime.sequence,
      graph_version: entry.graph.version,
      event_sequence: entry.verified_version.event_sequence,
      observed_at: entry.observed_at,
    },
  };
}
