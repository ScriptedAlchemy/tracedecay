import {
  APPLICATION_HTTP_BINDINGS,
  type ApplicationHttpOperationId,
} from '../contracts/application.ts';
import type {
  ApplicationProblemKind,
  ApplicationProblemRecord,
  ResolvedScope,
} from '../contracts/generated.ts';

export const FIXTURE_SCOPE: ResolvedScope = {
  project_id: 'project.work',
  repository_id: 'repository.work',
  worktree_id: 'worktree.work',
  reference: null,
  scope_digest: 'sha256:scope',
};

const EXECUTION = {
  started_at: 1,
  ended_at: 2,
  effective_deadline: null,
  cancellation: null,
  budget: { units_consumed: 0, bytes_consumed: 0, elapsed_micros: 0 },
  termination: 'completed',
};

export function applicationSuccess(
  operation: ApplicationHttpOperationId,
  payload: unknown,
  options: { scope?: unknown; requestId?: string; bindingId?: string; pendingEffect?: boolean } = {},
): { kind: 'success'; value: Record<string, unknown> } {
  const binding = APPLICATION_HTTP_BINDINGS[operation];
  const outcome =
    binding.receipt === 'durable_effect'
      ? {
          outcome: 'effect',
          value: {
            effect_id: 'effect.fixture',
            effect_class: 'administrative',
            idempotency_key: 'idempotency.fixture',
            authority: {},
            expected_state: 'sha256:expected',
            execution: options.pendingEffect ? { ...EXECUTION, termination: 'effect_unknown' } : EXECUTION,
            reconciliation: options.pendingEffect ? 'pending' : 'reconciled',
            receipt: options.pendingEffect ? { outcome: 'effect_unknown', committed_state: null } : {},
            payload,
          },
        }
      : {
          outcome: 'evidence',
          value: {
            temporal: {},
            authority: {},
            evidence_authorities: [],
            coverage: {},
            omissions: [],
            scores: [],
            contributions: [],
            page: {
              sort_contract_id: 'sort.fixture',
              sort_revision: 1,
              total: null,
              returned: 0,
              cursor: null,
              expires_at: null,
            },
            execution: EXECUTION,
            payload,
          },
        };
  return {
    kind: 'success',
    value: {
      binding_id: options.bindingId ?? binding.bindingIds[0],
      contract: {
        schema_id: binding.resultSchema.schemaId,
        schema_revision: binding.resultSchema.revision,
      },
      request_id: options.requestId ?? 'request.fixture',
      scope: options.scope ?? FIXTURE_SCOPE,
      outcome,
    },
  };
}

/** What the daemon's own constructor attaches to each problem kind, so a
 * fixture satisfies the canonical problem invariants unless a test overrides
 * a field to break one. */
const PROBLEM_DEFAULTS: Readonly<Record<ApplicationProblemKind, Partial<ApplicationProblemRecord>>> = {
  invalid_request: { legal_actions: ['correct_request'] },
  not_found_or_not_authorized: {},
  conflict: { retry: 'after_revalidate', legal_actions: ['refresh'] },
  stale: { retry: 'after_revalidate', legal_actions: ['refresh'] },
  partial_effect: { terminality: 'admitted_terminal', legal_actions: ['reconcile'] },
  unsupported: {},
  unavailable: { unavailable_classification: 'authority' },
  execution_failed: {
    terminality: 'admitted_terminal',
    execution_failure_classification: 'permanent',
    legal_actions: ['contact_administrator'],
  },
  reset_required: { terminality: 'admitted_terminal', legal_actions: ['reset'] },
  saturated: { retry: 'after_delay', retry_after_millis: 250, legal_actions: ['retry'] },
  cancelled: { cancellation_stage: 'before_admission' },
  timed_out: { cancellation_stage: 'before_admission' },
};

const RETRY_SCOPE = {
  never: null,
  same_request: 'same_request',
  after_delay: 'same_request',
  after_revalidate: 'fresh_request',
  after_reconcile: 'same_operation',
} as const;

export function applicationProblem(
  kind: ApplicationProblemKind,
  options: {
    binding?: ApplicationHttpOperationId;
    problem?: Partial<ApplicationProblemRecord>;
  } = {},
): { kind: 'problem'; value: Record<string, unknown> } {
  const fields = { ...PROBLEM_DEFAULTS[kind], ...options.problem };
  const retry = fields.retry ?? 'never';
  const withoutDiagnostic =
    kind === 'not_found_or_not_authorized' || kind === 'cancelled' || kind === 'timed_out';
  const problem: ApplicationProblemRecord = {
    revision: 1,
    kind,
    code: `fixture.${kind}`,
    message: `fixture ${kind} problem`,
    diagnostic: withoutDiagnostic ? null : { code: `fixture.${kind}`, message: `fixture ${kind} problem` },
    detail: null,
    committed_receipt: null,
    owning_layer: 'application',
    terminality: 'pre_admission',
    retryable: retry !== 'never',
    retry,
    retry_scope: RETRY_SCOPE[retry],
    retry_after_millis: null,
    cancellation_stage: null,
    unavailable_classification: null,
    execution_failure_classification: null,
    request_id: 'request.fixture',
    trace_id: 'trace.fixture',
    details: [],
    legal_actions: [],
    coverage: null,
    ...fields,
  };
  return {
    kind: 'problem',
    value: {
      ...(options.binding === undefined
        ? {}
        : { binding_id: APPLICATION_HTTP_BINDINGS[options.binding].bindingIds[0] }),
      contract: { schema_id: 'schema.application.problem', schema_revision: 1 },
      request_id: problem.request_id,
      problem,
    },
  };
}
