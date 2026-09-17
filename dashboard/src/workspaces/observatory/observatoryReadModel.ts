/**
 * `GET /api/observatory`, read once.
 *
 * Five Observatory surfaces decode this one route — the canonical
 * observations, adoption coverage and outcomes, retrieval quality, rejected
 * arguments, performance budgets, and performance comparisons. They used to
 * spell four different query-key heads for it, so React Query held four cache
 * entries and issued four requests for one set of bytes, and two panels could
 * explain two different watermarks of the same projector at the same time.
 *
 * The head lives here, once. Every reader of the route — the page-level hook
 * below and the section-level `CanonicalReadModelSection` — appends the same
 * scope token after it, so all of them share one entry and one watermark.
 */
import { ObservatoryReadModelV1Schema } from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';

export const OBSERVATORY_READ_MODEL_URL = '/api/observatory';

export const OBSERVATORY_READ_MODEL_KEY = ['observatory', 'read-model'] as const;

/** The period the Observatory read model is polled at — the same thirty
 * seconds the telemetry, findings, and Doctor reads use, so the overview's
 * one time context does not drift between authorities. Observations are a
 * sweep, not a stream; nothing invalidates them but this poll. */
export const OBSERVATORY_READ_MODEL_REFETCH_MS = 30_000;

export function useObservatoryReadModel() {
  return useEnvelope(
    OBSERVATORY_READ_MODEL_KEY,
    OBSERVATORY_READ_MODEL_URL,
    ObservatoryReadModelV1Schema,
    { staleTime: OBSERVATORY_READ_MODEL_REFETCH_MS, refetchInterval: OBSERVATORY_READ_MODEL_REFETCH_MS },
  );
}
