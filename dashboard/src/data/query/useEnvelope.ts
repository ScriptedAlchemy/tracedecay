import { useQuery } from '@tanstack/react-query';

import { fetchEnvelope, type EnvelopeResult } from './envelope.ts';
import { scopedQueryKey, scopedUrl, useScope } from '../scope/store.ts';
import type { WireSchema } from './wireSchema.ts';

/** Scoped read hook for every dashboard route that serves DashboardEnvelopeV1. */
export function useEnvelope<T>(
  key: readonly unknown[],
  url: string,
  schema: WireSchema<T>,
  options?: { enabled?: boolean; staleTime?: number },
) {
  const scope = useScope((s) => s.scope);
  const target = scopedUrl(scope, url);
  return useQuery<EnvelopeResult<T>>({
    queryKey: scopedQueryKey(scope, key, url),
    queryFn: ({ signal }) => fetchEnvelope(target, schema, { signal }),
    refetchInterval: false,
    staleTime: options?.staleTime ?? 60_000,
    enabled: options?.enabled ?? true,
  });
}

/** Inner payload after envelope acceptance; absent for a blocked read. */
export function envelopePayload<T>(result: EnvelopeResult<T> | undefined): T | undefined {
  return result?.outcome === 'envelope' ? result.envelope.payload : undefined;
}
