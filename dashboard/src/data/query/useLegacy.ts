import { useQuery } from '@tanstack/react-query';
import type { z } from 'zod';
import { fetchLegacy, type LegacyResult } from './legacy.ts';

export function useLegacy<T>(key: readonly unknown[], url: string, schema: z.ZodType<T>) {
  return useQuery<LegacyResult<T>>({
    queryKey: key,
    queryFn: () => fetchLegacy(url, schema),
    refetchInterval: 30_000,
  });
}
