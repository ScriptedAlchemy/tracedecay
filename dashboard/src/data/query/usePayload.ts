import { useQuery } from '@tanstack/react-query';
import { fetchPayload, type PayloadResult } from './payload.ts';
import type { QueryActivityDescriptor } from './activity.ts';
import type { WireSchema } from './wireSchema.ts';
import { scopedQueryKey, scopedUrl, useScope } from '../scope/store.ts';

interface PayloadQueryOptions {
  readonly refetchInterval?: number | false;
  readonly staleTime?: number;
  readonly enabled?: boolean;
  readonly activity?: QueryActivityDescriptor;
}

export function usePayload<T>(
  key: readonly unknown[],
  url: string,
  schema: WireSchema<T>,
  options?: PayloadQueryOptions,
) {
  const scope = useScope((s) => s.scope);
  const target = scopedUrl(scope, url);
  return useQuery<PayloadResult<T>>({
    queryKey: [...scopedQueryKey(scope, key, url)],
    // React Query's signal, threaded through so a read this dashboard has
    // stopped waiting for actually stops. Two callers abandon reads: a scope
    // change (a scoped read's key carries its project, so every one of them is
    // replaced at once) and an SSE invalidation, which cancels the in-flight
    // refetch by default. Without the signal both abandoned the PROMISE while
    // the request ran to completion, so a burst of registry events queued a
    // request per event against a route none of the answers would be read from.
    queryFn: ({ signal }) => fetchPayload(target, schema, { signal }),
    meta:
      options?.activity === undefined
        ? undefined
        : { dashboard: { activity: options.activity } },
    // Heavy stores make some payload queries expensive; default to
    // fetch-on-mount only so stacked refetches never starve the daemon.
    refetchInterval: options?.refetchInterval ?? false,
    staleTime: options?.staleTime ?? 60_000,
    enabled: options?.enabled ?? true,
  });
}
