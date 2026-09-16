import { useQuery } from '@tanstack/react-query';

import { fetchEnvelope, type EnvelopeResult } from './envelope.ts';
import type { QueryActivityDescriptor } from './activity.ts';
import { scopedQueryKey, scopedUrl, useScope } from '../scope/store.ts';
import type { WireSchema } from './wireSchema.ts';

/**
 * Refusals the daemon issues for a moment, not for the request: a second
 * reader arrived while another held the generation. One read a moment later
 * is the honest answer; a refusal that survives the retries is shown as such.
 */
const ADMISSION_RETRY_DELAYS_MS = [150, 400, 900] as const;

/** Scoped read hook for every dashboard route that serves DashboardEnvelopeV1. */
export function useEnvelope<T>(
  key: readonly unknown[],
  url: string,
  schema: WireSchema<T>,
  options?: {
    enabled?: boolean;
    staleTime?: number;
    activity?: QueryActivityDescriptor;
    /** Transport details (omission reasons) worth one more read a moment later. */
    retryOnDetail?: readonly string[];
  },
) {
  const scope = useScope((s) => s.scope);
  const target = scopedUrl(scope, url);
  const retryOnDetail = options?.retryOnDetail;
  return useQuery<EnvelopeResult<T>>({
    queryKey: scopedQueryKey(scope, key, url),
    queryFn: async ({ signal }) => {
      let result = await fetchEnvelope(target, schema, { signal });
      for (const delay of ADMISSION_RETRY_DELAYS_MS) {
        if (
          retryOnDetail === undefined ||
          result.outcome !== 'transport' ||
          result.detail === undefined ||
          !retryOnDetail.includes(result.detail)
        ) {
          break;
        }
        await new Promise((resolve) => setTimeout(resolve, delay));
        if (signal.aborted) break;
        result = await fetchEnvelope(target, schema, { signal });
      }
      return result;
    },
    meta:
      options?.activity === undefined
        ? undefined
        : { dashboard: { activity: options.activity } },
    refetchInterval: false,
    staleTime: options?.staleTime ?? 60_000,
    enabled: options?.enabled ?? true,
  });
}

/** Inner payload after envelope acceptance; absent for a blocked read. */
export function envelopePayload<T>(result: EnvelopeResult<T> | undefined): T | undefined {
  return result?.outcome === 'envelope' ? result.envelope.payload : undefined;
}
