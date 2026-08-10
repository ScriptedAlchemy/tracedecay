import type {
  WorkAttemptStateV1,
  WorkGraphTimelineCoverageV1,
  WorkRuntimeProjectionCoverageV1,
  WorkRelationReplanDecisionV1,
  WorkTimelineLaneV1,
} from '../contracts/index.ts';

/**
 * Work-product graph reads for the Work tests, shaped the way the daemon shapes
 * them.
 *
 * One builder for both the model tests and the DOM tests, so the wire fixture a
 * page is rendered from and the object the derivations are asserted against
 * cannot drift apart into two different ideas of what a graph version looks
 * like. The same arrangement `workAttemptFixture.ts` uses, for the same reason.
 *
 * Everything is returned unparsed. Callers prove it: the model tests run it
 * through `WorkGraphReadV1Schema`, and the DOM tests put it on the wire where
 * `callWork` parses it with the same schema. A fixture that stopped satisfying
 * the contract therefore fails both, rather than quietly passing one.
 *
 * The graph and the bundle are built from ONE task spec each. That coherence is
 * deliberate even though the dashboard never reads `entry.graph`: a fixture
 * whose bundle claimed three tasks over a graph holding none would be a place
 * for a derivation bug to hide behind a number nobody could trace back.
 */

export interface WorkGraphTaskSpec {
  readonly taskId: string;
  readonly title?: string;
  /** Declared effort. The authority sums these into `workload.total_effort`,
   * and this fixture does the same rather than letting the two drift. */
  readonly effort?: number;
  readonly createdAt?: number;
  readonly updatedAt?: number;
  readonly scheduledAt?: number | null;
  readonly deadline?: number | null;
  readonly dependencies?: readonly string[];
  /** Nominated causes. DECLARED data — an empty list is the plan declaring
   * none, which is the case these tests exist to keep separate from a read that
   * could not answer. */
  readonly causalCandidates?: readonly string[];
  readonly lane?: WorkTimelineLaneV1;
  /** Handoffs recorded on this task. Omitted means the task carries none,
   * which is a graph saying nothing was handed on — distinct from a read that
   * never landed, and the Agents handoff surface is built to keep the two
   * apart. `task_id` is filled from the spec so a fixture cannot record a
   * handoff against a task it does not belong to. */
  readonly handoffs?: readonly WorkHandoffSpec[];
}

export interface WorkHandoffSpec {
  readonly handoffId: string;
  readonly fromActor: string;
  readonly toActor: string;
  /** `UtcMicros`. */
  readonly handedOffAt: number;
  readonly evidenceFrontier?: readonly string[];
  readonly unknowns?: readonly string[];
}

export interface WorkRuntimeAttemptSpec {
  readonly attemptId: string;
  readonly taskId: string;
  readonly runId: string;
  readonly state?: WorkAttemptStateV1;
}

export interface WorkGraphVersionSpec {
  readonly tasks: readonly WorkGraphTaskSpec[];
  readonly version?: number;
  /** The instant the caller observed this version at — the clock every churn
   * reading is measured against. */
  readonly observedAt?: number;
  readonly validAt?: number;
  /** The chain the authority weighted, and what it weighed. Left to the task
   * order and the effort sum when omitted. */
  readonly criticalPath?: readonly string[];
  readonly criticalPathEffort?: number;
  /** Omitted means "the authority answered the runtime-gated figures": the
   * ready/running/blocked split and both concurrency counts. `null` is the
   * authority withholding them, which is what it does whenever runtime coverage
   * is anything but complete. */
  readonly readyEffort?: number | null;
  readonly runningEffort?: number | null;
  readonly blockedEffort?: number | null;
  readonly requestedConcurrency?: number | null;
  readonly actualConcurrency?: number | null;
  readonly runtimeAttempts?: readonly WorkRuntimeAttemptSpec[];
  readonly runtimeCoverage?: WorkRuntimeProjectionCoverageV1;
  readonly generationId?: string;
  readonly relationReplanDecisions?: readonly WorkRelationReplanDecisionV1[];
}

const DEFAULT_OBSERVED_AT = 1_800_000_000_000_000;
const HOUR_MICROS = 3_600_000_000;

function taskEffort(task: WorkGraphTaskSpec): number {
  return task.effort ?? 1;
}

function workItem(task: WorkGraphTaskSpec, observedAt: number) {
  return {
    accepted_at: null,
    accepted_attempts: [],
    accepted_criteria: {},
    accepted_proposal: null,
    accepted_route: null,
    archived_at: null,
    evidence_links: [],
    execution_admitted_at: null,
    handoffs: (task.handoffs ?? []).map((handoff) => ({
      evidence_frontier: [...(handoff.evidenceFrontier ?? [])],
      from_actor: handoff.fromActor,
      handed_off_at: handoff.handedOffAt,
      handoff_id: handoff.handoffId,
      task_id: task.taskId,
      to_actor: handoff.toActor,
      unknowns: [...(handoff.unknowns ?? [])],
    })),
    input: {
      acceptance_criteria: [],
      causal_candidates: [...(task.causalCandidates ?? [])],
      created_at: task.createdAt ?? observedAt - 48 * HOUR_MICROS,
      deadline: task.deadline ?? null,
      dependencies: [...(task.dependencies ?? [])],
      effort: taskEffort(task),
      hierarchy: {
        initiative_id: 'initiative-1',
        milestone_id: 'milestone-1',
        plan_id: 'plan-1',
      },
      informational_relations: [],
      scheduled_at: task.scheduledAt ?? null,
      task_id: task.taskId,
      title: task.title ?? task.taskId,
      updated_at: task.updatedAt ?? observedAt - 48 * HOUR_MICROS,
    },
  };
}

/** The runtime projection, restated identically on the entry and inside the
 * bundle — which is how the daemon serializes it, and the reason the dashboard
 * can read either one and get the same version. */
function runtimeProjection(spec: WorkGraphVersionSpec, version: number, observedAt: number) {
  return {
    attempts: (spec.runtimeAttempts ?? []).map((attempt) => ({
      identity: {
        attempt_id: attempt.attemptId,
        run_id: attempt.runId,
        task_id: attempt.taskId,
      },
      state: attempt.state ?? 'running',
    })),
    coverage: spec.runtimeCoverage ?? { coverage: 'complete' },
    generation_id: spec.generationId ?? 'generation-7',
    graph_version: version,
    observed_at: observedAt,
    sequence: 12,
  };
}

/** One `WorkGraphVersionEntryV1`: an immutable graph version and the whole
 * projection bundle derived from it at one observation instant. */
export function workGraphVersion(spec: WorkGraphVersionSpec) {
  const version = spec.version ?? 4;
  const observedAt = spec.observedAt ?? DEFAULT_OBSERVED_AT;
  const totalEffort = spec.tasks.reduce((sum, task) => sum + taskEffort(task), 0);
  const criticalPath = spec.criticalPath ?? spec.tasks.map((task) => task.taskId);
  const runtime = runtimeProjection(spec, version, observedAt);

  const gatingEdges = spec.tasks.flatMap((task) =>
    (task.dependencies ?? []).map((dependency) => ({
      dependency,
      dependent: task.taskId,
    })),
  );
  const candidateEdges = spec.tasks.flatMap((task) =>
    (task.causalCandidates ?? []).map((dependency) => ({
      dependency,
      dependent: task.taskId,
    })),
  );

  return {
    graph: {
      evidence: [],
      initiatives: [],
      items: spec.tasks.map((task) => workItem(task, observedAt)),
      milestones: [],
      plans: [],
      proposal_decisions: [],
      relation_replan_decisions: [...(spec.relationReplanDecisions ?? [])],
      version,
    },
    observed_at: observedAt,
    projected_at: observedAt,
    projections: {
      causal: { candidate_edges: candidateEdges, graph_version: version },
      critical_path: {
        graph_version: version,
        task_ids: [...criticalPath],
        total_effort:
          spec.criticalPathEffort ??
          spec.tasks
            .filter((task) => criticalPath.includes(task.taskId))
            .reduce((sum, task) => sum + taskEffort(task), 0),
      },
      dag: {
        gating_edges: gatingEdges,
        graph_version: version,
        task_ids: spec.tasks.map((task) => task.taskId),
      },
      graph_version: version,
      kanban: {
        cards: spec.tasks.map((task) => ({
          effort: taskEffort(task),
          lane: task.lane ?? 'todo',
          legal_actions: [],
          task_id: task.taskId,
        })),
        graph_version: version,
      },
      runtime,
      timeline: {
        entries: spec.tasks.map((task) => ({
          created_at: task.createdAt ?? observedAt - 48 * HOUR_MICROS,
          deadline: task.deadline ?? null,
          scheduled_at: task.scheduledAt ?? null,
          task_id: task.taskId,
          updated_at: task.updatedAt ?? observedAt - 48 * HOUR_MICROS,
        })),
        graph_version: version,
      },
      workload: {
        actual_concurrency:
          spec.actualConcurrency === undefined ? 1 : spec.actualConcurrency,
        blocked_effort: spec.blockedEffort === undefined ? 0 : spec.blockedEffort,
        graph_version: version,
        ready_effort: spec.readyEffort === undefined ? totalEffort : spec.readyEffort,
        requested_concurrency:
          spec.requestedConcurrency === undefined ? 2 : spec.requestedConcurrency,
        running_effort: spec.runningEffort === undefined ? 0 : spec.runningEffort,
        total_effort: totalEffort,
      },
    },
    runtime,
    valid_at: spec.validAt ?? observedAt,
    verified_version: {
      event_sequence: 12,
      graph_version: version,
      recovered_graph_digest: 'digest-graph',
      source_watermark: {},
    },
  };
}

const SCOPE = {
  owner_brain_id: 'brain-1',
  owner_profile_id: 'profile-1',
  selection: { selection: 'profile_owned_no_git' },
};

/** A `current` graph read: one version, no timeline, no coverage — the mode the
 * dashboard actually asks in. */
export function workGraphRead(spec: WorkGraphVersionSpec) {
  return {
    authorized_scope: SCOPE,
    mode: 'current',
    snapshot: workGraphVersion(spec),
  };
}

/** An `evolution` graph read: a timeline of versions and the coverage it was
 * read under. Present so the empty-timeline success — complete coverage over
 * zero returned entries — can be put on the wire as the daemon would send it. */
export function workGraphTimeline(
  versions: readonly WorkGraphVersionSpec[],
  coverage: WorkGraphTimelineCoverageV1 = {
    coverage: 'complete',
    returned: versions.length,
  },
) {
  return {
    authorized_scope: SCOPE,
    mode: 'evolution',
    timeline: { coverage, entries: versions.map(workGraphVersion) },
  };
}
