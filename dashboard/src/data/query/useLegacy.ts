import { useQuery } from '@tanstack/react-query';
import type { z } from 'zod';
import { fetchLegacy, type LegacyResult } from './legacy.ts';

export function useLegacy<T>(
  key: readonly unknown[],
  url: string,
  schema: z.ZodType<T>,
  options?: { refetchInterval?: number | false; staleTime?: number; enabled?: boolean },
) {
  return useQuery<LegacyResult<T>>({
    queryKey: key,
    queryFn: () => fetchLegacy(url, schema),
    // Heavy stores make some legacy queries expensive; default to
    // fetch-on-mount only so stacked refetches never starve the daemon.
    refetchInterval: options?.refetchInterval ?? false,
    staleTime: options?.staleTime ?? 60_000,
    enabled: options?.enabled ?? true,
  });
}
