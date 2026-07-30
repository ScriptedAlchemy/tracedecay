import { useQuery } from '@tanstack/react-query';
import { fetchLegacy, type LegacyResult } from './legacy.ts';
import type { WireSchema } from './wireSchema.ts';
import { requestScopeKey, scopedUrl, useScope } from '../scope/store.ts';

export function useLegacy<T>(
  key: readonly unknown[],
  url: string,
  schema: WireSchema<T>,
  options?: { refetchInterval?: number | false; staleTime?: number; enabled?: boolean },
) {
  const scope = useScope((s) => s.scope);
  const target = scopedUrl(scope, url);
  return useQuery<LegacyResult<T>>({
    // Keyed by what the REQUEST carries, not by the scope it was made under.
    // `/api/projects` and `/api/dashboard` are never rewritten, so they are one
    // entry shared across scopes rather than a fresh copy per project.
    queryKey: [...key, requestScopeKey(scope, url)],
    // React Query's signal, threaded through so a read this dashboard has
    // stopped waiting for actually stops. Two callers abandon reads: a scope
    // change (a scoped read's key carries its project, so every one of them is
    // replaced at once) and an SSE invalidation, which cancels the in-flight
    // refetch by default. Without the signal both abandoned the PROMISE while
    // the request ran to completion, so a burst of registry events queued a
    // request per event against a route none of the answers would be read from.
    queryFn: ({ signal }) => fetchLegacy(target, schema, { signal }),
    // Heavy stores make some legacy queries expensive; default to
    // fetch-on-mount only so stacked refetches never starve the daemon.
    refetchInterval: options?.refetchInterval ?? false,
    staleTime: options?.staleTime ?? 60_000,
    enabled: options?.enabled ?? true,
  });
}
