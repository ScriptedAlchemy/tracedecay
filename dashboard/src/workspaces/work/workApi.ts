import type { z } from 'zod';
import { ResolvedScopeSchema, type ResolvedScope } from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

/**
 * The wire for the twelve canonical Work routes.
 *
 * Every request body and every response payload on this surface is a generated
 * contract; nothing here declares a payload shape of its own. What this module
 * does own is the step between them — the daemon's application envelope, which
 * the dashboard's own codegen does not emit.
 *
 * That gap is the one thing to understand before reading further. Every other
 * dashboard surface talks to a route returning `DashboardEnvelopeV1<T>`, a
 * generated type decoded by `data/query/envelope.ts`. The Work routes are
 * mounted differently: `src/dashboard/mod.rs` nests them straight onto the
 * application router, so they answer with the application's own
 * `HttpJsonEnvelope<T>` (`crates/tracedecay-api/src/lib.rs`) — a `kind`/`value`
 * union wrapping an outcome packet whose `payload` field holds the contract the
 * dashboard actually asked for. None of that wrapper is in the dashboard's
 * contract catalog, so there is no generated schema to decode it with.
 *
 * Rather than restate the wrapper as a second wire format, this module treats it
 * as structure to walk and never as a shape to trust: it reaches for the payload
 * and hands that and the envelope's resolved scope to their generated schemas.
 * If either moves, the caller is told `unsupported_schema`. The failure is a
 * truthful refusal rather than a fabricated read, which is the property that
 * matters while the wrapper itself remains uncontracted.
 */

/** A route the daemon actually mounts, with the contracts on either side of it.
 *
 * Named for the operation id the backend registers (`operation.work.views`
 * and so on) so a route here can be checked against
 * `src/dashboard/work_api.rs` by eye. */
export interface WorkRoute<Request, Response> {
  readonly operation: string;
  readonly path: string;
  readonly request: z.ZodType<Request>;
  readonly response: z.ZodType<Response>;
}

/** What a Work call produced: the contract, or a reason there is no contract.
 *
 * There is no third case. A refusal always carries a domain state and a
 * sentence, so no caller can render an absence as an empty success. */
export type WorkResult<T> =
  | { readonly outcome: 'value'; readonly value: T; readonly scope?: ResolvedScope }
  | { readonly outcome: 'refused'; readonly state: DomainStateKind; readonly detail: string };

/**
 * The daemon's problem taxonomy, read from the status line.
 *
 * `application_problem_status` (`crates/tracedecay-api/src/http.rs`) maps each
 * `ApplicationProblemKind` onto a status code, and that mapping is the part of
 * the problem contract the dashboard can rely on without a generated schema for
 * the problem record itself. Reading the status rather than the body is the
 * deliberate choice: the body would tell us more — which of conflict and stale
 * a 409 was, the retry directive, the legal actions — but only by hand-modelling
 * `ApplicationProblemRecord`, and a hand-modelled problem record that drifted
 * would misreport why a command failed.
 *
 * 409 is therefore reported as `conflicting` for both of its causes. Both mean
 * the same thing to someone holding a stale `expected_version`: read again
 * before retrying.
 */
export function workRefusal(status: number): { state: DomainStateKind; detail: string } {
  switch (status) {
    case 400:
      return { state: 'error', detail: 'the daemon rejected the request as invalid' };
    case 404:
      // The daemon deliberately conflates "no such task" with "not yours" so
      // that probing for a binding cannot reveal whether it exists. Reporting
      // it as `denied` keeps that conflation instead of guessing which it was.
      return { state: 'denied', detail: 'not found, or not authorized for this actor' };
    case 405:
      return { state: 'locked', detail: 'this scope will not accept the write' };
    case 409:
      return { state: 'conflicting', detail: 'the task moved since it was read' };
    case 422:
      return { state: 'unsupported', detail: 'the daemon does not support this request' };
    case 429:
      return { state: 'unavailable', detail: 'the daemon is saturated' };
    case 408:
      return { state: 'cancelled', detail: 'the daemon cancelled the request' };
    case 504:
      return { state: 'timed_out', detail: 'the daemon timed out' };
    case 503:
      return { state: 'unavailable', detail: 'the Work runtime is unavailable' };
    default:
      return { state: 'error', detail: `HTTP ${status}` };
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function workResolvedScope(body: unknown): ResolvedScope | undefined {
  if (!isRecord(body) || body.kind !== 'success' || !isRecord(body.value)) return undefined;
  const parsed = ResolvedScopeSchema.safeParse(body.value.scope);
  return parsed.success ? parsed.data : undefined;
}

/**
 * Walk the application envelope to the contract inside it.
 *
 * `{kind:"success", value:{…, outcome:{outcome:"evidence"|"effect", value:{…,
 * payload}}}}`. Reads succeed as evidence and commands as effects; both put the
 * contract in the same place, so the outcome tag is checked for presence and not
 * branched on.
 *
 * `undefined` means the envelope was not the shape described above. It is
 * distinct from a `payload` of `null`, which is the daemon saying the operation
 * carried no value — that is returned as `null` and refused by the caller rather
 * than quietly becoming an empty read.
 */
export function workPayload(body: unknown): { found: true; payload: unknown } | { found: false } {
  if (!isRecord(body) || body.kind !== 'success' || !isRecord(body.value)) return { found: false };
  const outcome = body.value.outcome;
  if (!isRecord(outcome) || typeof outcome.outcome !== 'string' || !isRecord(outcome.value)) {
    return { found: false };
  }
  if (!('payload' in outcome.value)) return { found: false };
  return { found: true, payload: outcome.value.payload };
}

/**
 * Call one Work route.
 *
 * The request is validated against its generated schema before it is sent, so a
 * malformed command is a local failure rather than a 400 the user has to
 * interpret. The response payload is validated against its generated schema
 * before it is returned, so nothing downstream can receive a value this build's
 * contracts do not describe.
 */
export async function callWork<Request, Response>(
  route: WorkRoute<Request, Response>,
  request: Request,
  url: string,
  init?: RequestInit,
): Promise<WorkResult<Response>> {
  const encoded = route.request.safeParse(request);
  if (!encoded.success) {
    return {
      outcome: 'refused',
      state: 'error',
      detail: `the request does not satisfy ${route.operation}`,
    };
  }

  let response: Response_;
  try {
    response = await fetch(url, {
      method: 'POST',
      headers: { accept: 'application/json', 'content-type': 'application/json' },
      body: JSON.stringify(encoded.data),
      ...init,
    });
  } catch {
    return { outcome: 'refused', state: 'offline', detail: 'the daemon could not be reached' };
  }

  if (!response.ok) {
    const refusal = workRefusal(response.status);
    return { outcome: 'refused', state: refusal.state, detail: refusal.detail };
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return {
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the daemon returned a body that is not JSON',
    };
  }

  const found = workPayload(body);
  if (!found.found) {
    return {
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the response envelope is not the shape this build reads',
    };
  }
  if (found.payload === null) {
    return {
      outcome: 'refused',
      state: 'unavailable',
      detail: 'the daemon answered without a value',
    };
  }

  const parsed = route.response.safeParse(found.payload);
  if (!parsed.success) {
    return {
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: `the payload does not satisfy ${route.operation}`,
    };
  }
  const scope = workResolvedScope(body);
  if (scope === undefined) {
    return {
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the response envelope carries no valid resolved Work scope',
    };
  }
  return { outcome: 'value', value: parsed.data, scope };
}

/** Aliased so the `Response` type is not shadowed by the generic parameter. */
type Response_ = globalThis.Response;
