import { useQuery } from '@tanstack/react-query';
import { fetchLegacy, type LegacyResult } from './legacy.ts';
import type { WireSchema } from './wireSchema.ts';
import { scopeKey, scopedUrl, useScope } from '../scope/store.ts';

export function useLegacy<T>(
  key: readonly unknown[],
  url: string,
  schema: WireSchema<T>,
  options?: { refetchInterval?: number | false; staleTime?: number; enabled?: boolean },
) {
  const scope = useScope((s) => s.scope);
  const target = scopedUrl(scope, url);
  return useQuery<LegacyResult<T>>({
    queryKey: [...key, scopeKey(scope)],
    queryFn: () => fetchLegacy(target, schema),
    // Heavy stores make some legacy queries expensive; default to
    // fetch-on-mount only so stacked refetches never starve the daemon.
    refetchInterval: options?.refetchInterval ?? false,
    staleTime: options?.staleTime ?? 60_000,
    enabled: options?.enabled ?? true,
  });
}
