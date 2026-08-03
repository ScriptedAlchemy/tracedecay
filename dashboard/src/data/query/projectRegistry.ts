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
import {
  ProjectContextPayloadV1Schema,
  ProjectsPayloadV1Schema,
  type ProjectContextPayloadV1,
} from '../../contracts/generated.ts';
import { useLegacy } from './useLegacy.ts';
import type { LegacyResult } from './legacy.ts';
import type { RegistryReading } from '../scope/store.ts';

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

/**
 * `GET /api/projects` — the listing.
 *
 * Truncated by default (the daemon clamps `limit` to 250 and defaults it to
 * 100), so this answers "some of the registry" and callers must not read a
 * missing id as an absent project. {@link useProjectEntry} is the bounded way
 * to ask about one.
 */
export function useProjectRegistry(options?: { enabled?: boolean }) {
  return useLegacy(projectRegistryListKey, '/api/projects', ProjectsPayloadV1Schema, options);
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
  return useLegacy(
    projectRegistryEntryKey(projectId ?? ''),
    `/api/projects/${encodeURIComponent(projectId ?? '')}`,
    ProjectContextPayloadV1Schema,
    { ...options, enabled: (options?.enabled ?? true) && projectId !== null },
  );
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
  result: LegacyResult<ProjectContextPayloadV1> | undefined,
): RegistryReading {
  if (!result) return { state: 'unknown' };
  switch (result.outcome) {
    case 'ok':
      return result.data.status === 'ok'
        ? {
            state: 'measured',
            // Nullable on the wire. A body that carried no project record
            // measured nothing about the name, so the claim stands unconfirmed
            // rather than being replaced by an id.
            label: result.data.project?.label ?? null,
            // Likewise `is_active`: absent means the answer did not say, which
            // is not the same as saying no.
            isActive: result.data.is_active ?? null,
          }
        : { state: 'unknown' };
    case 'unavailable':
      // The route's own discriminants. `not_found` is the registry answering
      // about this id; the rest are the registry itself being unavailable,
      // which measures nothing about the project.
      return result.status === PROJECT_NOT_FOUND
        ? { state: 'absent', reason: result.reason }
        : { state: 'unknown' };
    case 'offline':
    case 'unauthorized':
    case 'denied':
    case 'error':
    case 'unsupported_schema':
      return { state: 'unknown' };
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

/** Appends the source's own sentence to a state word, when it sent one.
 *
 * The generated payloads carry `error` alongside every non-ok `status`, and it
 * is the only part that says *which* registry, *which* path, or *what* failed.
 * Dropping it left every failure reading the same. */
function withReason(state: string, reason: string | null | undefined): string {
  return reason ? `${state} · ${reason}` : state;
}

/**
 * Why the displayed name is not one the registry confirmed, or `null` when it
 * is.
 *
 * An annotation rather than a replacement label: the name comes from the
 * reconciled scope on every surface, and this says what is known about it. A
 * reader looking at an unconfirmed name has to be able to see that it is
 * unconfirmed, or the middle state presents as settled.
 */
export function registryAnnotation(
  result: LegacyResult<ProjectContextPayloadV1> | undefined,
): string | null {
  if (!result) return 'resolving';
  switch (result.outcome) {
    case 'ok':
      // A 200 whose body is not `ok` is not a shape the route produces — every
      // failing status it reports comes with a 4xx/5xx — so this is a daemon
      // this build does not agree with, and it says so with the status it was
      // actually given rather than guessing which condition it meant.
      if (result.data.status !== 'ok') return `unexpected registry status: ${result.data.status}`;
      return result.data.project ? null : withReason('unconfirmed', result.data.error);
    case 'unavailable':
      // The daemon's own discriminant and sentence, verbatim. It distinguishes
      // "no such project" from "the registry could not be opened", which is
      // the difference between a dead link and a broken install.
      return withReason(
        result.status === PROJECT_NOT_FOUND ? 'not in registry' : 'registry unavailable',
        result.reason,
      );
    case 'offline':
      return 'registry offline';
    case 'unauthorized':
      return 'registry unauthorized';
    case 'denied':
      return 'registry denied';
    case 'error':
      return 'unconfirmed · registry error';
    case 'unsupported_schema':
      return 'unsupported registry schema';
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
