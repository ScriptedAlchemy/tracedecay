import { useQuery } from '@tanstack/react-query';
import { fetchLegacy, type LegacyResult } from './legacy.ts';
import type { QueryActivityDescriptor } from './activity.ts';
import type { WireSchema } from './wireSchema.ts';
import { requestScopeKey, scopedUrl, useScope, type DashboardScope } from '../scope/store.ts';

interface LegacyQueryOptions {
  readonly refetchInterval?: number | false;
  readonly staleTime?: number;
  readonly enabled?: boolean;
  readonly activity?: QueryActivityDescriptor;
}

/**
 * The cache entry a legacy read occupies, as one exported authority.
 *
 * Not an implementation detail of {@link useLegacy}: a mutation that writes the
 * server's re-read into a read's entry has to address the same key, and while
 * this construction was private the two drifted. The scheduler control built
 * its target as `[...key, scopeKey(scope)]` while the read it was writing into
 * was keyed `[...key, requestScopeKey(scope, url)]` — identical under a
 * selected project, and different under the all-projects default, where the
 * request is not rewritten and the token is `unscoped` rather than `all`. So a
 * pause that the daemon accepted was written to an entry nothing read, and the
 * panel went on showing the pre-click state.
 *
 * Exported so the writer derives the key rather than reconstructing it.
 */
export function legacyQueryKey(
  scope: DashboardScope,
  key: readonly unknown[],
  url: string,
): readonly unknown[] {
  // Keyed by what the REQUEST carries, not by the scope it was made under.
  // `/api/projects` and `/api/dashboard` are never rewritten, so they are one
  // entry shared across scopes rather than a fresh copy per project.
  return [...key, requestScopeKey(scope, url)];
}

export function useLegacy<T>(
  key: readonly unknown[],
  url: string,
  schema: WireSchema<T>,
  options?: LegacyQueryOptions,
) {
  const scope = useScope((s) => s.scope);
  const target = scopedUrl(scope, url);
  return useQuery<LegacyResult<T>>({
    queryKey: [...legacyQueryKey(scope, key, url)],
    // React Query's signal, threaded through so a read this dashboard has
    // stopped waiting for actually stops. Two callers abandon reads: a scope
    // change (a scoped read's key carries its project, so every one of them is
    // replaced at once) and an SSE invalidation, which cancels the in-flight
    // refetch by default. Without the signal both abandoned the PROMISE while
    // the request ran to completion, so a burst of registry events queued a
    // request per event against a route none of the answers would be read from.
    queryFn: ({ signal }) => fetchLegacy(target, schema, { signal }),
    meta:
      options?.activity === undefined
        ? undefined
        : { dashboard: { activity: options.activity } },
    // Heavy stores make some legacy queries expensive; default to
    // fetch-on-mount only so stacked refetches never starve the daemon.
    refetchInterval: options?.refetchInterval ?? false,
    staleTime: options?.staleTime ?? 60_000,
    enabled: options?.enabled ?? true,
  });
}
