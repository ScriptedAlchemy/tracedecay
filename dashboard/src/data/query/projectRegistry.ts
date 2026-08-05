/**
 * The project registry, as one authority.
 *
 * Four surfaces read the registry — the scope bar, the command palette, Remote
 * Brain's overview and its scoped project panel — and each had invented its own
 * query key. Only one of those keys (`['projects']`) was the key the SSE
 * `project_registry_changed` invalidation names, so a project rename or an
 * active-project switch refreshed Remote Brain and left the scope bar showing
 * the old answer indefinitely: it has no poll, and nothing else was going to
 * refetch it. The scope bar is where activation is reconciled, so the stale one
 * was the one the write controls depend on.
 *
 * Every key here is rooted at {@link PROJECT_REGISTRY_ROOT}, and the SSE
 * invalidation names that root. React Query matches keys by prefix, so one
 * invalidation reaches the list and every per-project entry without having to
 * enumerate them — and a key added later is covered by construction rather than
 * by remembering to add it to the event handler.
 */
import { useQuery } from '@tanstack/react-query';
import {
  DashboardEnvelopeV1Schema,
  ProjectContextPayloadV1Schema,
  ProjectsPayloadV1Schema,
  type DashboardDomainStateV1,
  type DashboardEnvelopeV1,
  type ProjectContextPayloadV1,
  type ProjectsPayloadV1,
} from '../../contracts/generated.ts';
import { readOnlyScopeRefusal, scopedQueryKey, useScope } from '../scope/store.ts';
import type { RegistryReading } from '../scope/store.ts';
import type { WireSchema } from './wireSchema.ts';

/** The prefix every registry query key starts with, and the one the daemon's
 * `project_registry_changed` invalidation names. */
export const PROJECT_REGISTRY_ROOT = 'projects';

/** `status` on a 404 from `GET /api/projects/{id}`: the registry was read and
 * holds no project under that id. Verbatim from `src/dashboard/projects.rs`. */
export const PROJECT_NOT_FOUND = 'not_found';

/** The whole-registry listing. */
export const projectRegistryListKey = [PROJECT_REGISTRY_ROOT, 'list'] as const;

/** One project, resolved by id. */
export function projectRegistryEntryKey(projectId: string): readonly string[] {
  return [PROJECT_REGISTRY_ROOT, 'entry', projectId];
}

/** What an SSE registry change invalidates: the root, so it reaches the listing
 * and every entry at once. */
export const projectRegistryInvalidationKey = [PROJECT_REGISTRY_ROOT] as const;

/** Result of a registry fetch. Transport failures become domain states; 404/503
 * answers that still carry a decoded envelope stay typed refusals, not generic
 * HTTP errors — the payload's own `status` is what distinguishes them. */
export type ProjectRegistryResult<T> =
  | { outcome: 'envelope'; envelope: DashboardEnvelopeV1<T> }
  | { outcome: 'transport'; state: DashboardDomainStateV1; detail?: string }
  | {
      outcome: 'source_unavailable';
      httpStatus: number;
      envelope: DashboardEnvelopeV1<T>;
    };

const undecodable = Symbol('undecodable');

async function decodedBody(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    return undecodable;
  }
}

/**
 * `GET /api/projects` or `GET /api/projects/{id}` — envelope-only.
 *
 * On 200 the body must be `DashboardEnvelopeV1<T>`. On 404/503 the body must
 * still be that envelope with a non-`ok` payload; bare payloads are not
 * accepted here.
 */
export async function fetchProjectRegistry<T>(
  url: string,
  payloadSchema: WireSchema<T>,
  init?: RequestInit,
): Promise<ProjectRegistryResult<T>> {
  let response: Response;
  try {
    response = await fetch(url, { headers: { accept: 'application/json' }, ...init });
  } catch (err) {
    if (init?.signal?.aborted === true) throw err;
    return { outcome: 'transport', state: 'offline' };
  }
  if (response.status === 405) {
    const refusal = readOnlyScopeRefusal(await decodedBody(response));
    if (refusal) {
      return { outcome: 'transport', state: 'locked', detail: refusal.detail };
    }
    return { outcome: 'transport', state: 'error', detail: 'HTTP 405' };
  }
  if (response.status === 401) return { outcome: 'transport', state: 'unauthorized' };
  if (response.status === 403) return { outcome: 'transport', state: 'denied' };

  const body = await decodedBody(response);
  if (body === undecodable) {
    return { outcome: 'transport', state: 'unsupported_schema' };
  }
  const parsed = DashboardEnvelopeV1Schema(payloadSchema).safeParse(body);
  if (!parsed.success) {
    return { outcome: 'transport', state: 'unsupported_schema' };
  }
  const envelope = parsed.data as DashboardEnvelopeV1<T>;
  if (response.ok) {
    return { outcome: 'envelope', envelope };
  }
  if (response.status === 404 || response.status === 503) {
    const status = (envelope.payload as { status?: unknown }).status;
    if (typeof status === 'string' && status !== 'ok') {
      return { outcome: 'source_unavailable', httpStatus: response.status, envelope };
    }
  }
  return { outcome: 'transport', state: 'error', detail: `HTTP ${response.status}` };
}

/**
 * `GET /api/projects` — the listing.
 *
 * Truncated by default (the daemon clamps `limit` to 250 and defaults it to
 * 100), so this answers "some of the registry" and callers must not read a
 * missing id as an absent project. {@link useProjectEntry} is the bounded way
 * to ask about one.
 */
export function useProjectRegistry(options?: { enabled?: boolean }) {
  const scope = useScope((s) => s.scope);
  const url = '/api/projects';
  return useQuery<ProjectRegistryResult<ProjectsPayloadV1>>({
    queryKey: scopedQueryKey(scope, projectRegistryListKey, url),
    queryFn: ({ signal }) => fetchProjectRegistry(url, ProjectsPayloadV1Schema, { signal }),
    refetchInterval: false,
    staleTime: 60_000,
    enabled: options?.enabled ?? true,
  });
}

/**
 * `GET /api/projects/{id}` — one project, exactly.
 *
 * A single row, so it is bounded regardless of how many projects are
 * registered, and it answers for a project whose graph is not mounted. It
 * carries both facts the scope needs — the canonical `label` and `is_active`,
 * which the daemon computes against the same `active_project_id` that decides
 * whether a write is accepted — which is why reconciliation asks this rather
 * than searching the listing.
 */
export function useProjectEntry(projectId: string | null, options?: { enabled?: boolean }) {
  const scope = useScope((s) => s.scope);
  const url = `/api/projects/${encodeURIComponent(projectId ?? '')}`;
  return useQuery<ProjectRegistryResult<ProjectContextPayloadV1>>({
    queryKey: scopedQueryKey(scope, projectRegistryEntryKey(projectId ?? ''), url),
    queryFn: ({ signal }) => fetchProjectRegistry(url, ProjectContextPayloadV1Schema, { signal }),
    refetchInterval: false,
    staleTime: 60_000,
    enabled: (options?.enabled ?? true) && projectId !== null,
  });
}

/** The inner payload when the registry answered with a decoded envelope. */
export function projectRegistryPayload<T>(
  result: ProjectRegistryResult<T> | undefined,
): T | undefined {
  if (!result) return undefined;
  if (result.outcome === 'envelope') return result.envelope.payload;
  if (result.outcome === 'source_unavailable') return result.envelope.payload;
  return undefined;
}

/**
 * What the registry established about the selected project.
 *
 * Three outcomes, because the route reports three different things.
 *
 * `status: "ok"` is a measurement. A 404 `not_found` is also a measurement, of
 * the opposite fact: the registry was read and holds no project under this id.
 * Everything else — the registry missing or unopenable (503), a transport
 * failure, an unreadable body — is `unknown`, and deliberately so, because the
 * two mistakes available here are not symmetric. Claiming a measurement would
 * let a failed read discard a label that may well be right and withdraw a
 * write that would have been accepted; `unknown` keeps the best-known name,
 * says it is unconfirmed, and settles nothing until an answer arrives.
 */
export function registryReading(
  result: ProjectRegistryResult<ProjectContextPayloadV1> | undefined,
): RegistryReading {
  const payload = projectRegistryPayload(result);
  if (!result || payload === undefined) return { state: 'unknown' };
  switch (result.outcome) {
    case 'source_unavailable':
      return payload.status === PROJECT_NOT_FOUND
        ? { state: 'absent', reason: payload.error ?? null }
        : { state: 'unknown' };
    case 'envelope':
      return payload.status === 'ok'
        ? {
            state: 'measured',
            label: payload.project?.label ?? null,
            isActive: payload.is_active ?? null,
          }
        : { state: 'unknown' };
    case 'transport':
      return { state: 'unknown' };
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

/** Appends the source's own sentence to a state word, when it sent one. */
function withReason(state: string, reason: string | null | undefined): string {
  return reason ? `${state} · ${reason}` : state;
}

/**
 * Why the displayed name is not one the registry confirmed, or `null` when it
 * is.
 */
export function registryAnnotation(
  result: ProjectRegistryResult<ProjectContextPayloadV1> | undefined,
): string | null {
  const payload = projectRegistryPayload(result);
  if (!result) return 'resolving';
  switch (result.outcome) {
    case 'envelope':
      if (payload?.status !== 'ok') {
        return payload?.status
          ? `unexpected registry status: ${payload.status}`
          : 'unconfirmed';
      }
      return payload.project ? null : withReason('unconfirmed', payload.error);
    case 'source_unavailable':
      return withReason(
        payload?.status === PROJECT_NOT_FOUND ? 'not in registry' : 'registry unavailable',
        payload?.error,
      );
    case 'transport':
      switch (result.state) {
        case 'offline':
          return 'registry offline';
        case 'unauthorized':
          return 'registry unauthorized';
        case 'denied':
          return 'registry denied';
        case 'unsupported_schema':
          return 'unsupported registry schema';
        case 'locked':
          return withReason('registry locked', result.detail);
        case 'error':
          return 'unconfirmed · registry error';
        default:
          return 'unconfirmed';
      }
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
