/**
 * Doctor storage findings, as one authority.
 *
 * Two surfaces read `/api/storage/findings`: Observatory's findings section and
 * the nav rail's app-wide Doctor dot. Both spelled the same key and the same
 * URL by hand, and then disagreed about the poll — 30 seconds in Observatory,
 * 60 in the rail.
 *
 * A shared key makes that disagreement worse than duplication. React Query
 * keeps one entry per key and refetches it on the shortest interval any mounted
 * observer asks for, so the rail's 60 seconds was never what happened: with
 * Observatory open the shared entry polled at 30, and with it closed at 60. The
 * rail's stated period described neither case, and nothing in either file said
 * the other existed.
 *
 * So the key, the route, the contract, and the period are decided here, once,
 * and both callers take what this returns.
 */
import { StorageFindingsPayloadV1Schema } from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from './envelope.ts';
import { scopeKey, scopedUrl, useScope, type DashboardScope } from '../scope/store.ts';
import { useQuery } from '@tanstack/react-query';
import type { StorageFindingsPayloadV1 } from '../../contracts/generated.ts';

/** The route, named once. Scope rewrites it; nothing else may spell it. */
export const STORAGE_FINDINGS_URL = '/api/storage/findings';

/**
 * How often the shared entry re-reads.
 *
 * Doctor findings are a retention sweep, not a live stream — no SSE family
 * invalidates them, so this poll is the only thing that moves them. Thirty
 * seconds is the shorter of the two periods the callers used to ask for, and
 * therefore the one that was already in effect whenever both were mounted.
 */
export const STORAGE_FINDINGS_REFETCH_MS = 30_000;

/** The cache entry, one per scope. */
export function storageFindingsKey(scope: DashboardScope): readonly string[] {
  return ['storage', 'findings', scopeKey(scope)];
}

/**
 * `GET /api/storage/findings` for the active scope.
 *
 * Answers an {@link EnvelopeResult}: a transport outcome is a state to render,
 * never an exception and never a fabricated empty report.
 */
export function useStorageFindings() {
  const scope = useScope((s) => s.scope);
  return useQuery<EnvelopeResult<StorageFindingsPayloadV1>>({
    queryKey: storageFindingsKey(scope),
    queryFn: () => fetchEnvelope(scopedUrl(scope, STORAGE_FINDINGS_URL), StorageFindingsPayloadV1Schema),
    refetchInterval: STORAGE_FINDINGS_REFETCH_MS,
  });
}
