import type {
  WorkAttemptListCoverageV1,
  WorkAttemptStateV1,
  WorkCancellationStateV1,
  WorkEffectStateV1,
  WorkProviderRouteV1,
  WorkRecoveryStateV1,
  WorkTerminalEvidenceV1,
} from '../contracts/index.ts';

/**
 * Attempt-list pages for the Work tests, shaped the way the daemon shapes them.
 *
 * One builder for both the model tests and the DOM tests, so the wire fixture a
 * page is rendered from and the object the derivations are asserted against
 * cannot drift apart into two different ideas of what an attempt looks like.
 *
 * Everything is returned unparsed. Callers prove it: the model tests run it
 * through `WorkAttemptListV1Schema`, and the DOM tests put it on the wire where
 * `callWork` parses it with the same schema. A fixture that stopped satisfying
 * the contract therefore fails both, rather than quietly passing one.
 */

export function workRoute(providerId: string, routeId: string): WorkProviderRouteV1 {
  return { provider_id: providerId, route_id: routeId };
}

const BINDING = {
  accepted_proposal: 'proposal-1',
  generation_id: 'generation-7',
  sequence: 12,
  work_version: 4,
};

const EXECUTION_SNAPSHOT = {
  approval: 'never',
  backend: 'codex_cli',
  configuration_revision_id: 'revision-1',
  configuration_snapshot_id: 'configuration-1',
  credential_references: [],
  deadline: 1_700_000_000_000_000,
  effective_behavior_digest: 'digest-behavior',
  egress: 'deny',
  environment_allowlist: [],
  executable: { artifact_digest: 'digest-executable', executable_id: 'codex' },
  fallback: { kind: 'disabled' },
  filesystem: 'read_only',
  limits: {
    max_concurrency: 1,
    max_input_tokens: 1,
    max_output_tokens: 1,
    max_protocol_bytes: 1,
    max_stderr_bytes: 1,
    max_stdout_bytes: 1,
  },
  model: 'model-1',
  protocol: 'codex_exec_json',
  resolution_provenance_digest: 'digest-resolution',
  route: workRoute('codex', 'route-primary'),
  sandbox: 'required',
  topology_policy_digest: 'digest-topology',
};

export interface WorkAttemptSpec {
  readonly taskId: string;
  readonly runId: string;
  readonly attemptId: string;
  readonly state?: WorkAttemptStateV1;
  readonly requested?: WorkProviderRouteV1;
  /** Omit for "ran where it was asked to"; `null` for an unobserved route. */
  readonly actual?: WorkProviderRouteV1 | null;
  readonly recovery?: WorkRecoveryStateV1;
  readonly cancellation?: WorkCancellationStateV1;
  /** Omit for a plain success; `null` for an attempt that has not terminated. */
  readonly terminal?: WorkTerminalEvidenceV1 | null;
  /** The effect class the execution envelope was admitted under. Defaults to
   * `observational`, which is the fixture's harmless case; the Plan 26
   * accounting tests override it because `compound_non_repeatable` is the
   * eligible denominator a duplicate-effect adjudication would run over. */
  readonly effectState?: WorkEffectStateV1;
  /** Where the execution envelope pinned this attempt. Every field defaults to
   * the fixture's single-worktree placement; the topology lens tests override
   * them to spread attempts across worktrees and refs. */
  readonly placement?: {
    readonly worktreeId?: string;
    readonly worktreeRoot?: string;
    readonly repositoryId?: string;
    readonly commit?: string;
    /** Omit for the fixture default of `null` (no ref recorded). */
    readonly reference?: string | null;
  };
}

export function workTerminal(outcome: WorkTerminalEvidenceV1['outcome'], observedAt: number) {
  return { evidence_digest: `digest-${observedAt}`, observed_at: observedAt, outcome };
}

export function workAttempt(spec: WorkAttemptSpec) {
  const identity = { attempt_id: spec.attemptId, run_id: spec.runId, task_id: spec.taskId };
  const requested = spec.requested ?? workRoute('codex', 'route-primary');
  return {
    actual_route: spec.actual === undefined ? requested : spec.actual,
    artifacts: [],
    cancellation: spec.cancellation ?? { state: 'none' },
    execution: {
      attempt_identity: identity,
      cancellation_generation: 0,
      commit: spec.placement?.commit ?? 'commit-1',
      effect_state: spec.effectState ?? 'observational',
      execution_snapshot: EXECUTION_SNAPSHOT,
      instructions: 'run the task',
      operation: 'operation.work.start_attempt',
      project_id: 'project',
      projection_binding: BINDING,
      reference: spec.placement?.reference ?? null,
      repository_id: spec.placement?.repositoryId ?? 'repository',
      worktree_id: spec.placement?.worktreeId ?? 'worktree',
      worktree_root: spec.placement?.worktreeRoot ?? '/w/main',
    },
    identity,
    lease: { epoch: 1, lease_id: `lease-${spec.attemptId}` },
    progress: null,
    projection_binding: BINDING,
    recovery: spec.recovery ?? { state: 'fresh' },
    requested_route: requested,
    state: spec.state ?? 'succeeded',
    terminal: spec.terminal === undefined ? workTerminal('succeeded', 1_000) : spec.terminal,
  };
}

/** A `listed` attempt-list payload. */
export function workAttemptList(
  attempts: readonly unknown[],
  coverage: WorkAttemptListCoverageV1 = { coverage: 'complete', returned: attempts.length },
) {
  return {
    attempts,
    coverage,
    state: 'listed',
    topology: { generation: 'generation-7', task_count: 6 },
  };
}
