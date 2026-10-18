/** Structure reads preserve measured, unmeasured and producer-failed states
 * inside the dashboard envelope. Transport remains outside that union: an
 * unreachable route says nothing about whether its producer ran. */
import { useQuery } from '@tanstack/react-query';
import type { WireSchema } from './wireSchema.ts';

import { fetchEnvelope } from './envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../scope/store.ts';
import type { DashboardDomainStateV1 } from '../../contracts/generated.ts';

export type StructureResult<T> =
  | { outcome: 'measured'; measurement: T }
  | { outcome: 'unmeasured'; reason: string; detail: string }
  | { outcome: 'failed'; code: string; detail: string; retryable: boolean }
  | { outcome: 'transport'; state: DashboardDomainStateV1; detail?: string };

/** Common structure of the generated StructureRead aliases; their numeric
 * suffixes are schema deduplication details, not different transport rules. */
type StructureReadWire<T> =
  | { status: 'measured'; measurement: T }
  | { status: 'unmeasured'; reason: string; detail: string }
  | { status: 'failed'; code: string; detail: string; retryable: boolean };

export async function fetchStructure<T>(
  url: string,
  schema: WireSchema<StructureReadWire<T>>,
  init?: RequestInit,
): Promise<StructureResult<T>> {
  const result = await fetchEnvelope(url, schema, init);
  if (result.outcome === 'transport') {
    return result.detail === undefined
      ? { outcome: 'transport', state: result.state }
      : { outcome: 'transport', state: result.state, detail: result.detail };
  }
  const read = result.envelope.payload;
  switch (read.status) {
    case 'measured':
      return { outcome: 'measured', measurement: read.measurement };
    case 'unmeasured':
      return { outcome: 'unmeasured', reason: read.reason, detail: read.detail };
    case 'failed':
      return {
        outcome: 'failed',
        code: read.code,
        detail: read.detail,
        retryable: read.retryable,
      };
    default: {
      const exhaustive: never = read;
      return exhaustive;
    }
  }
}

/** Scoped and cancellable. An absent selection disables its node-scoped
 * route instead of issuing a fabricated node//facts request. */
export function useStructure<T>(
  key: readonly unknown[],
  url: string,
  schema: WireSchema<StructureReadWire<T>>,
  options?: { enabled?: boolean },
) {
  const scope = useScope((s) => s.scope);
  return useQuery({
    queryKey: [...key, scopeKey(scope)],
    queryFn: ({ signal }) => fetchStructure(scopedUrl(scope, url), schema, { signal }),
    enabled: options?.enabled ?? true,
    staleTime: 60_000,
  });
}

/** Preserve the producer's own reason when a reading is absent. */
export function absenceReason<T>(result: StructureResult<T>): string | null {
  switch (result.outcome) {
    case 'measured':
      return null;
    case 'unmeasured':
      return `${result.reason}, ${result.detail}`;
    case 'failed':
      return `${result.code}, ${result.detail}${result.retryable ? ' (retryable)' : ''}`;
    case 'transport':
      return result.detail ? `${result.state}, ${result.detail}` : result.state;
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
