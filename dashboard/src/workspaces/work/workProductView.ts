import type { WorkGraphReadV1 } from '../../contracts/index.ts';
import type { WorkResult } from './workApi.ts';

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
  const projections = entry.graph.items.map((item): WorkTaskView => {
    const applicableReplan = entry.graph.relation_replan_decisions.find(
      (decision) =>
        decision.disposition === 'accepted' &&
        decision.proposal.task_id === item.input.task_id &&
        decision.proposal.based_on_version + 1 === entry.graph.version,
    )?.proposal;
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
    };
  });

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
    },
  };
}
