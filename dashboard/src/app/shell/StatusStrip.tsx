import { useSyncExternalStore } from 'react';
import { StateChip } from '../../ui/StateChip';
import { useEventStreamState, useEventsConnection } from '../../data/sse/useEvents.tsx';

/** One-line status strip: daemon connection, live progress, receipts.
 * Liveness is the real /api/events stream state — never simulated. */
export function StatusStrip() {
  const { state } = useEventStreamState();
  const connection = useEventsConnection();
  // Retained operation receipts (plan 11: background operation receipts stay
  // visible; the reducer preserves them across reconnects). The strip shows
  // the most recent one; the count carries the rest.
  const receipts = useSyncExternalStore(
    (notify) => (connection ? connection.subscribe(notify) : () => {}),
    () => (connection ? connection.reducer.getRetainedReceipts().length : 0),
  );
  const latest = connection?.reducer.getRetainedReceipts().at(-1);
  const latestLabel =
    latest && typeof latest.payload === 'object' && latest.payload !== null
      ? String(
          (latest.payload as Record<string, unknown>)['operation'] ??
            (latest.payload as Record<string, unknown>)['operation_id'] ??
            JSON.stringify(latest.stream),
        )
      : latest
        ? String(JSON.stringify(latest.stream))
        : undefined;
  return (
    <footer
      className="flex h-7 shrink-0 items-center gap-3 border-t border-edge-subtle bg-surface-1 px-3"
      aria-label="Status"
    >
      {state === 'live' ? (
        <StateChip kind="ready" detail="event stream live" />
      ) : state === 'connecting' ? (
        <StateChip kind="loading" detail="connecting to event stream" />
      ) : (
        <StateChip kind="offline" detail="event stream not connected" />
      )}
      {receipts > 0 ? (
        <span className="tabular truncate text-2xs text-text-secondary" role="status">
          {receipts} operation {receipts === 1 ? 'receipt' : 'receipts'}
          {latestLabel ? (
            <span className="text-text-muted"> · latest {latestLabel}</span>
          ) : null}
        </span>
      ) : null}
      <div className="flex-1" />
      <span className="text-2xs text-text-muted tabular">PR14 build-out</span>
    </footer>
  );
}