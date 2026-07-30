import { z } from 'zod';
import type { WireSchema } from './wireSchema.ts';
import { readOnlyScopeRefusal, type ReadOnlyScopeRefusal } from '../scope/store.ts';

/** Result for the legacy (pre-envelope) JSON endpoints. These are the
 * compatibility surfaces the old dashboard consumed; they return plain
 * payloads. Transport failures become truthful states, never exceptions.
 * As families migrate to DashboardEnvelopeV1, callers switch to
 * fetchEnvelope and this helper shrinks. */
export type LegacyResult<T> =
  | { outcome: 'ok'; data: T }
  /**
   * The source answered, in its own contract, that it cannot serve this.
   *
   * Distinct from `error`, which is this dashboard reporting a status code it
   * could not interpret. The canonical project routes reply 503 for
   * `missing_registry` and `registry_unavailable` and 404 for `not_found`, and
   * every one of those carries the generated payload with a `status`
   * discriminant and an `error` sentence. Reading only the status code threw
   * that away: three different, actionable conditions arrived as
   * `HTTP 503`/`HTTP 404`, and the payload branches written to render them
   * were unreachable because a non-2xx body never reached a caller.
   */
  | {
      outcome: 'unavailable';
      /** The transport status that carried it, kept for the log and the report. */
      httpStatus: number;
      /** The payload's own discriminant — `not_found`, `missing_registry`, … */
      status: string;
      /** The payload's `error`, when it sent one. */
      reason: string | null;
      data: T;
    }
  | { outcome: 'offline' }
  | { outcome: 'unauthorized' }
  | { outcome: 'denied' }
  | { outcome: 'error'; detail: string }
  | { outcome: 'unsupported_schema' };

/**
 * A write's result: every reading a read can produce, plus the one only a
 * write can.
 *
 * `read_only_scope` sits here rather than on `LegacyResult` because the
 * gateway raises it only for non-GET/HEAD requests. Putting it on the read
 * type would oblige every read consumer to handle a state its request cannot
 * reach, and an arm that can never be taken is an arm nobody keeps correct.
 * A write consumer, conversely, cannot forget it: the union is exhaustive and
 * the compiler says so.
 */
export type LegacyWriteResult<T> =
  | LegacyResult<T>
  | { outcome: 'read_only_scope'; refusal: ReadOnlyScopeRefusal };

/** Sentinel for a body that was not JSON at all, kept distinct from a body
 * that decoded to `null` — the second is a legal body that must still fail the
 * payload schema rather than being mistaken for a decode failure. */
const undecodable = Symbol('undecodable');

async function decodedBody(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    return undecodable;
  }
}

/** Reads the canonical failure discriminant out of an admitted 404/503 body.
 *
 * Gated on a non-`ok` string `status`, which is the marker the generated
 * payloads carry and an arbitrary error body does not. Several wire schemas
 * are open records that would accept any object, so without this the typed
 * path would swallow every 404 from every route and report a condition the
 * source never named. */
function canonicalFailure(body: unknown): { status: string; reason: string | null } | null {
  if (typeof body !== 'object' || body === null) return null;
  const record = body as Record<string, unknown>;
  const status = record['status'];
  if (typeof status !== 'string' || status === 'ok') return null;
  const error = record['error'];
  return { status, reason: typeof error === 'string' && error !== '' ? error : null };
}

async function readLegacyResponse<T>(
  url: string,
  schema: WireSchema<T>,
  init?: RequestInit,
): Promise<LegacyWriteResult<T>> {
  let response: Response;
  try {
    response = await fetch(url, { headers: { accept: 'application/json' }, ...init });
  } catch (err) {
    // A cancelled request is not a reading, and must not be laundered into
    // one. `fetch` rejects identically whether the network failed or this
    // dashboard aborted the request itself, and the caller that aborts is the
    // scope change: switching project cancels the previous project's in-flight
    // reads. Returning `offline` here would write a fabricated failure into
    // the abandoned scope's cache entry, so switching back would render a
    // daemon-is-down state for a request nobody ever heard the answer to.
    //
    // Rethrowing is also what React Query expects — it recognises the
    // cancellation and leaves the entry untouched rather than storing an
    // error — so the abandoned scope simply has no reading, which is true.
    if (init?.signal?.aborted === true) throw err;
    return { outcome: 'offline' };
  }
  // An authorization refusal is its own reading, not an error carrying a
  // status code. 401 means the daemon accepted no identity for this read and
  // 403 means it knows the identity and will not serve this scope — two
  // different next actions for the reader, and neither one is "retry".
  if (response.status === 401) return { outcome: 'unauthorized' };
  if (response.status === 403) return { outcome: 'denied' };
  // 405 from the project gateway is a scope refusal: this project is not the
  // active one, so it is served read-only. It is neither `denied` (the
  // identity is fine) nor a generic error (the remedy is specific and the
  // daemon states it), and unlike both it is fixed by changing scope.
  if (response.status === 405) {
    const refusal = readOnlyScopeRefusal(await decodedBody(response));
    if (refusal) return { outcome: 'read_only_scope', refusal };
    // A 405 this dashboard cannot account for. The request was refused and
    // that is all that is known, so it stays a plain error rather than
    // borrowing the read-only explanation.
    return { outcome: 'error', detail: 'HTTP 405' };
  }
  // The two statuses the canonical routes use to report a condition rather
  // than a fault: 503 for a registry that is missing or cannot be opened, 404
  // for an id it does not hold. Both carry the generated payload.
  if (response.status === 404 || response.status === 503) {
    const body = await decodedBody(response);
    const canonical = body === undecodable ? null : canonicalFailure(body);
    if (canonical !== null) {
      const parsed = schema.safeParse(body);
      // A body that names its condition but that this build cannot otherwise
      // read is a build mismatch, and says so, rather than being reported as
      // a typed payload it is not.
      if (!parsed.success) return { outcome: 'unsupported_schema' };
      return {
        outcome: 'unavailable',
        httpStatus: response.status,
        status: canonical.status,
        reason: canonical.reason,
        data: parsed.data,
      };
    }
    // No canonical discriminant: an ordinary 404 or 503 from anywhere else in
    // the stack, including a proxy. Nothing typed to report.
    return { outcome: 'error', detail: `HTTP ${response.status}` };
  }
  if (!response.ok) {
    return { outcome: 'error', detail: `HTTP ${response.status}` };
  }
  const body = await decodedBody(response);
  if (body === undecodable) return { outcome: 'unsupported_schema' };
  const parsed = schema.safeParse(body);
  if (!parsed.success) return { outcome: 'unsupported_schema' };
  return { outcome: 'ok', data: parsed.data };
}

/**
 * Read a legacy endpoint.
 *
 * A caller that passes a mutating `init` gets the refusal folded into `error`,
 * carrying the daemon's own sentence — truthful, but without the arm a control
 * needs to disable itself. Writes should use {@link fetchLegacyWrite}.
 */
export async function fetchLegacy<T>(
  url: string,
  schema: WireSchema<T>,
  init?: RequestInit,
): Promise<LegacyResult<T>> {
  const result = await readLegacyResponse(url, schema, init);
  return result.outcome === 'read_only_scope'
    ? { outcome: 'error', detail: result.refusal.detail }
    : result;
}

/** Write to a legacy endpoint, keeping the scope refusal as its own outcome. */
export function fetchLegacyWrite<T>(
  url: string,
  schema: WireSchema<T>,
  init: RequestInit,
): Promise<LegacyWriteResult<T>> {
  return readLegacyResponse(url, schema, init);
}

/** Loose object schema for legacy payloads we render generically. */
export const AnyObject = z.record(z.string(), z.unknown());
export type AnyObj = z.infer<typeof AnyObject>;

/* ---- typed slices of the legacy surfaces the workspaces consume ---- */

// `ProjectSchema`/`ProjectsSchema` — every field optional, the collection
// optional — had no callers; `/api/projects` is the generated
// `ProjectsPayloadV1Schema`, where `status` discriminates the reading and
// `projects`/`active_project_id` are required and nullable rather than absent.
// Deleted rather than kept as a spare, because a schema whose every field is
// optional accepts a body that says nothing, and the one field it would have
// been reached for — `active_project_id`, the authority behind `scopeWritable`
// — it could not express at all.

export const LcmOverviewSchema = AnyObject;
export const LcmSessionsSchema = AnyObject;
export const MemoryOverviewSchema = AnyObject;
export const GraphOverviewSchema = AnyObject;
export const SavingsOverviewSchema = AnyObject;
export const AutomationStatusSchema = AnyObject;
export const SettingsSchema = AnyObject;
