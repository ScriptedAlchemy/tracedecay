/**
 * Reader for the graph-structure routes (plan 11b Surfaces 1–2).
 *
 * These five routes — `call-chain`, `strata`, and the three node-scoped reads
 * `node/{id}/facts|tests|sessions` — are the only endpoints in the dashboard
 * that ship a *measurement-grade* wire contract: every one returns
 * `DashboardEnvelopeV1<StructureReadV1<T>>`, where the inner union is
 *
 *   measured    the producer ran and this is the reading
 *   unmeasured  the producer did not run, and `reason`/`detail` say why
 *   failed      the producer ran and errored, `retryable` says whether to retry
 *
 * That union is the reason these routes are worth consuming carefully. Most of
 * this dashboard has to *infer* absence, which is why so many surfaces carry an
 * "unverified — the legacy response cannot distinguish zero from failure"
 * caption. Here the wire says which one it is, so nothing has to be inferred
 * and nothing may be flattened: collapsing `unmeasured` into an empty
 * measurement would manufacture exactly the false zero those captions exist to
 * avoid.
 *
 * Transport sits *outside* that union — a route that never answered has no
 * `status` at all — so this module widens it to four cases rather than folding
 * a network failure into `failed`, which would misreport an unreachable daemon
 * as a producer error.
 */
import { useQuery } from '@tanstack/react-query';
import type { z } from 'zod';

import { fetchEnvelope } from './envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../scope/store.ts';
import type { DashboardDomainStateV1 } from '../../contracts/generated.ts';

/** One structure read, transport included, with nothing collapsed. */
export type StructureResult<T> =
  | { outcome: 'measured'; measurement: T }
  | { outcome: 'unmeasured'; reason: string; detail: string }
  | { outcome: 'failed'; code: string; detail: string; retryable: boolean }
  | { outcome: 'transport'; state: DashboardDomainStateV1; detail?: string };

/** The shape every `StructureReadV1<T>` alias in the generated barrel takes.
 * Declared structurally so this reader works for all five without naming the
 * generated aliases individually — they are `StructureReadV1`, `…V12`, `…V13`,
 * `…V14`, `…V15`, and that numbering is an artifact of schemars deduplication
 * rather than anything a caller should have to know. */
type StructureReadWire<T> =
  | { status: 'measured'; measurement: T }
  | { status: 'unmeasured'; reason: string; detail: string }
  | { status: 'failed'; code: string; detail: string; retryable: boolean };

export async function fetchStructure<T>(
  url: string,
  schema: z.ZodType<StructureReadWire<T>>,
): Promise<StructureResult<T>> {
  const result = await fetchEnvelope(url, schema);
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

/**
 * A scoped structure read.
 *
 * `enabled` is threaded through rather than left to the caller to fake with an
 * empty URL: these routes are node-scoped, and issuing `node//facts` while a
 * selection is still null would produce a genuine 404 that this surface would
 * then be obliged to report as a real unmeasured reading.
 */
export function useStructure<T>(
  key: readonly unknown[],
  url: string,
  schema: z.ZodType<StructureReadWire<T>>,
  options?: { enabled?: boolean },
) {
  const scope = useScope((s) => s.scope);
  return useQuery({
    queryKey: [...key, scopeKey(scope)],
    queryFn: () => fetchStructure(scopedUrl(scope, url), schema),
    enabled: options?.enabled ?? true,
    staleTime: 60_000,
  });
}

/** One line naming why a reading is absent, in the caller's own terms.
 *
 * Kept here beside the union so every structure surface words absence the same
 * way. `unmeasured` prints the producer's own `reason` because that string is
 * the closest thing the wire has to an explanation a reader can act on. */
export function absenceReason<T>(result: StructureResult<T>): string | null {
  switch (result.outcome) {
    case 'measured':
      return null;
    case 'unmeasured':
      return `${result.reason} — ${result.detail}`;
    case 'failed':
      return `${result.code} — ${result.detail}${result.retryable ? ' (retryable)' : ''}`;
    case 'transport':
      return result.detail ? `${result.state} — ${result.detail}` : result.state;
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
