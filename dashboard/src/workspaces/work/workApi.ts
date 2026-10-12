import type { WireSchema } from '../../data/query/wireSchema.ts';
import {
  ApplicationProblemEnvelopeSchema,
  ResolvedScopeSchema,
  type ApplicationProblemKind,
  type ApplicationProblemRecord,
  type ResolvedScope,
} from '../../contracts/generated.ts';
import {
  APPLICATION_HTTP_BINDINGS,
  applicationProblemStatusMatches,
  decodeApplicationHttpSuccess,
  isHttpProblemEnvelope,
  type OperationTermination,
  type ApplicationHttpOperationId,
} from '../../contracts/application.ts';
import { decodeJsonBody, isRecord, UNDECODABLE } from '../../data/query/responseBody.ts';
import { readOnlyScopeRefusal } from '../../data/scope/store.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

export interface WorkRoute<Request, Response> {
  readonly operation: ApplicationHttpOperationId;
  readonly path: string;
  readonly request: WireSchema<Request>;
  readonly response: WireSchema<Response>;
}

export type WorkResult<T> =
  | { readonly outcome: 'value'; readonly value: T; readonly scope?: ResolvedScope }
  | {
      readonly outcome: 'refused';
      readonly state: DomainStateKind;
      readonly detail: string;
      readonly problem?: ApplicationProblemRecord;
      readonly termination?: OperationTermination;
    };

const TERMINATION_STATE: Readonly<Partial<Record<OperationTermination, DomainStateKind>>> = {
  cancelled: 'cancelled',
  timed_out: 'timed_out',
  failed: 'error',
  partial: 'partial',
  effect_unknown: 'unknown',
};

const PROBLEM_STATE: Readonly<Record<ApplicationProblemKind, DomainStateKind>> = {
  invalid_request: 'error',
  not_found_or_not_authorized: 'denied',
  conflict: 'conflicting',
  stale: 'stale',
  partial_effect: 'partial',
  unsupported: 'unsupported',
  unavailable: 'unavailable',
  reset_required: 'unavailable',
  execution_failed: 'error',
  saturated: 'unavailable',
  cancelled: 'cancelled',
  timed_out: 'timed_out',
};

function refused(state: DomainStateKind, detail: string): WorkResult<never> {
  return { outcome: 'refused', state, detail };
}

export function workResultInvalidatesReads(result: WorkResult<unknown>): boolean {
  if (result.outcome === 'value') return true;
  if (result.termination !== undefined) return true;
  const problem = result.problem;
  return (
    problem !== undefined &&
    (problem.committed_receipt !== null || problem.retry === 'after_revalidate')
  );
}

function decodeSuccess<Response>(
  route: WorkRoute<unknown, Response>,
  body: unknown,
): WorkResult<Response> {
  if (!isRecord(body) || body.kind !== 'success') {
    return refused('unsupported_schema', 'the response envelope is not the shape this build reads');
  }
  let payload: unknown;
  let envelopeScope: unknown;
  let termination: OperationTermination;
  try {
    const envelope = decodeApplicationHttpSuccess(route.operation, body.value, (value) => value);
    payload = envelope.outcome.value.payload;
    envelopeScope = envelope.scope;
    termination = envelope.outcome.value.execution.termination;
  } catch {
    return refused('unsupported_schema', `the response envelope does not answer ${route.operation}`);
  }
  if (payload === null) {
    return {
      outcome: 'refused',
      state: TERMINATION_STATE[termination] ?? 'unavailable',
      detail: `the daemon answered ${termination} without a value`,
      termination,
    };
  }
  const parsed = route.response.safeParse(payload);
  if (!parsed.success) {
    return refused('unsupported_schema', `the payload does not satisfy ${route.operation}`);
  }
  const scope = ResolvedScopeSchema.safeParse(envelopeScope);
  if (!scope.success) {
    return refused('unsupported_schema', 'the response envelope carries no valid resolved scope');
  }
  return { outcome: 'value', value: parsed.data, scope: scope.data };
}

function decodeProblem(
  route: WorkRoute<unknown, unknown>,
  status: number,
  body: unknown,
): WorkResult<never> | undefined {
  if (!isRecord(body) || body.kind !== 'problem' || !isRecord(body.value)) return undefined;
  const { binding_id: bindingId, ...envelope } = body.value;
  const parsed = ApplicationProblemEnvelopeSchema.safeParse(envelope);
  if (
    !parsed.success ||
    !isHttpProblemEnvelope(body.value) ||
    !applicationProblemStatusMatches(status, parsed.data.problem.kind)
  ) {
    return refused('unsupported_schema', 'the daemon returned a problem this build cannot read');
  }
  const problem = parsed.data.problem;
  const unnamedPreAdmission = bindingId === undefined && problem.terminality === 'pre_admission';
  const concealedDenial = problem.kind === 'not_found_or_not_authorized';
  const namesThisRoute = APPLICATION_HTTP_BINDINGS[route.operation].bindingIds.some(
    (id) => id === bindingId,
  );
  const bindingAllowed = concealedDenial ? bindingId === undefined : unnamedPreAdmission || namesThisRoute;
  if (!bindingAllowed) {
    return refused('unsupported_schema', `the problem does not answer ${route.operation}`);
  }
  return { outcome: 'refused', state: PROBLEM_STATE[problem.kind], detail: problem.message, problem };
}

function gatewayRefusal(status: number, body: unknown): WorkResult<never> {
  const detail = isRecord(body) && typeof body.detail === 'string' ? body.detail : `HTTP ${status}`;
  switch (status) {
    case 404:
      return refused('denied', detail);
    case 503:
      return refused('unavailable', detail);
    default:
      return refused('error', detail);
  }
}

export async function callWork<Request, Response>(
  route: WorkRoute<Request, Response>,
  request: Request,
  url: string,
  signal?: AbortSignal,
): Promise<WorkResult<Response>> {
  const encoded = route.request.safeParse(request);
  if (!encoded.success) {
    return refused('error', `the request does not satisfy ${route.operation}`);
  }

  let response: Response_;
  try {
    response = await fetch(url, {
      method: 'POST',
      headers: { accept: 'application/json', 'content-type': 'application/json' },
      body: JSON.stringify(encoded.data),
      signal,
    });
  } catch (err) {
    if (signal?.aborted === true) throw err;
    return refused('offline', 'the daemon could not be reached');
  }

  if (response.status === 401) {
    return refused('unauthorized', 'the dashboard session is not authorized');
  }
  if (response.status === 403) {
    return refused('denied', 'the dashboard origin was refused');
  }

  const body = await decodeJsonBody(response, signal);
  if (response.ok) {
    if (body === UNDECODABLE) {
      return refused('unsupported_schema', 'the daemon returned a body that is not JSON');
    }
    return decodeSuccess(route, body);
  }
  const locked = response.status === 405 ? readOnlyScopeRefusal(body) : null;
  if (locked !== null) return refused('locked', locked.detail);
  return decodeProblem(route, response.status, body) ?? gatewayRefusal(response.status, body);
}

/** Aliased so the `Response` type is not shadowed by the generic parameter. */
type Response_ = globalThis.Response;
