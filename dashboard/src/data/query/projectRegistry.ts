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

/** Result of a registry fetch. A decoded registry envelope is authoritative
 * regardless of the HTTP status it travelled with: registry outcomes live in
 * the payload's `status`, while HTTP remains authoritative for write and
 * authorization refusals, malformed bodies, and failures with no typed
 * registry outcome. */
export type ProjectRegistryResult<T> =
  | { outcome: 'envelope'; envelope: DashboardEnvelopeV1<T> }
  | { outcome: 'transport'; state: DashboardDomainStateV1; detail?: string };

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
 * A body is accepted only when it is `DashboardEnvelopeV1<T>`. The envelope's
 * known registry outcomes (`not_found`, `missing_registry`, and
 * `registry_unavailable`) are preserved across a 200 or an older non-2xx
 * transport. A non-typed non-2xx answer remains an error.
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
  if (response.ok || typedRegistryOutcome(envelope.payload)) {
    return { outcome: 'envelope', envelope };
  }
  return { outcome: 'transport', state: 'error', detail: `HTTP ${response.status}` };
}

function typedRegistryOutcome(payload: unknown): boolean {
  if (typeof payload !== 'object' || payload === null) return false;
  const status = (payload as { status?: unknown }).status;
  return status === PROJECT_NOT_FOUND || isRegistryUnavailableStatus(status);
}

function isRegistryUnavailableStatus(status: unknown): boolean {
  return status === 'missing_registry' || status === 'registry_unavailable';
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
  return result?.outcome === 'envelope' ? result.envelope.payload : undefined;
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
  if (payload.status === PROJECT_NOT_FOUND) {
    return { state: 'absent', reason: payload.error ?? null };
  }
  if (payload.status === 'ok') {
    return {
      state: 'measured',
      label: payload.project?.label ?? null,
      isActive: payload.is_active ?? null,
    };
  }
  return { state: 'unknown' };
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
  if (payload?.status === PROJECT_NOT_FOUND) {
    return withReason('not in registry', payload.error);
  }
  if (isRegistryUnavailableStatus(payload?.status)) {
    return withReason('registry unavailable', payload?.error);
  }
  switch (result.outcome) {
    case 'envelope':
      if (payload?.status !== 'ok') {
        return payload?.status
          ? `unexpected registry status: ${payload.status}`
          : 'unconfirmed';
      }
      return payload.project ? null : withReason('unconfirmed', payload.error);
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
