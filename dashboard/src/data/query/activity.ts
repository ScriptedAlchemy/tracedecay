import {
  useQueryClient,
  type QueryClient,
  type QueryKey,
} from '@tanstack/react-query';
import { useMemo, useSyncExternalStore } from 'react';
import { create } from 'zustand';

export interface QueryActivityDescriptor {
  readonly id: string;
  readonly label: string;
  readonly cancelable: boolean;
}

export interface ActiveQueryActivity extends QueryActivityDescriptor {
  readonly queryHash: string;
  readonly queryKey: QueryKey;
}

interface DashboardQueryMeta {
  readonly dashboard?: {
    readonly activity?: QueryActivityDescriptor;
  };
}

function activityFromMeta(meta: unknown): QueryActivityDescriptor | null {
  if (typeof meta !== 'object' || meta === null) return null;
  const dashboard = (meta as DashboardQueryMeta).dashboard;
  const activity = dashboard?.activity;
  if (
    activity === undefined ||
    typeof activity.id !== 'string' ||
    activity.id.length === 0 ||
    typeof activity.label !== 'string' ||
    activity.label.length === 0 ||
    typeof activity.cancelable !== 'boolean'
  ) {
    return null;
  }
  return activity;
}

/** Observes only queries that explicitly supplied dashboard activity metadata.
 * Ordinary background cache work stays silent instead of acquiring a guessed
 * label or a cancel control it never promised to honor. */
export function useActiveQueryActivities(): readonly ActiveQueryActivity[] {
  const client = useQueryClient();
  const cache = client.getQueryCache();
  const activeSignature = useSyncExternalStore(
    (onStoreChange) => cache.subscribe(onStoreChange),
    () =>
      cache
        .getAll()
        .filter(
          (query) =>
            query.state.fetchStatus === 'fetching' && activityFromMeta(query.meta) !== null,
        )
        .map((query) => query.queryHash)
        .join('\u0000'),
    () => '',
  );

  return useMemo(() => {
    const active: ActiveQueryActivity[] = [];
    for (const query of cache.getAll()) {
      if (query.state.fetchStatus !== 'fetching') continue;
      const descriptor = activityFromMeta(query.meta);
      if (descriptor === null) continue;
      active.push({
        ...descriptor,
        queryHash: query.queryHash,
        queryKey: query.queryKey,
      });
    }
    return active;
  }, [activeSignature, cache]);
}

interface CancellationState {
  readonly lastCancellation: QueryActivityDescriptor | null;
  recordCancellation: (activity: QueryActivityDescriptor) => void;
}

export const useQueryCancellation = create<CancellationState>((set) => ({
  lastCancellation: null,
  recordCancellation: (activity) => set({ lastCancellation: activity }),
}));

/** Cancels the exact cache entry represented by the displayed activity. The
 * control never appears for descriptors that did not declare cancellation. */
export async function cancelQueryActivity(
  client: QueryClient,
  activity: ActiveQueryActivity,
): Promise<boolean> {
  if (!activity.cancelable) return false;
  await client.cancelQueries({ queryKey: activity.queryKey, exact: true });
  useQueryCancellation.getState().recordCancellation(activity);
  return true;
}
