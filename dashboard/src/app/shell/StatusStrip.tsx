import { useSyncExternalStore, type ReactNode } from 'react';
import { useEventStreamState, useEventsConnection } from '../../data/sse/useEvents.tsx';
import { cn } from '../../ui/cn';

/** Telemetry bar: the daemon link, live progress, and retained receipts, read
 * out as divided instrument cells rather than a sentence. Liveness is the real
 * /api/events stream state — never simulated — and the lamp is the one element
 * in the shell permitted to move (reduced motion pins it lit). */
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
  const link =
    state === 'live'
      ? { value: 'live', tone: 'bg-state-ready', live: true }
      : state === 'connecting'
        ? { value: 'sync', tone: 'bg-state-loading', live: true }
        : { value: 'down', tone: 'bg-state-offline', live: false };
  return (
    <footer
      className="flex h-8 shrink-0 items-stretch border-t border-edge-subtle bg-surface-1"
      aria-label="Status"
    >
      <Cell label="Link">
        <span
          aria-hidden
          className={cn('size-1.5 shrink-0', link.tone, link.live && 'td-signal')}
        />
        <span className="td-value text-2xs uppercase" role="status">
          {link.value}
        </span>
      </Cell>
      <Cell label="Receipts">
        <span className="td-value text-2xs" data-cell="numeric">
          {String(receipts).padStart(3, '0')}
        </span>
        {latestLabel ? (
          <span className="max-w-64 truncate text-3xs text-text-muted">{latestLabel}</span>
        ) : null}
      </Cell>
      <span aria-hidden className="flex-1 border-r border-edge-subtle" />
      <Cell label="Build">
        <span className="td-value text-2xs text-text-secondary">PR14</span>
      </Cell>
    </footer>
  );
}

function Cell({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 shrink-0 items-center gap-2 border-r border-edge-subtle px-3">
      <span className="td-legend">{label}</span>
      <span className="flex min-w-0 items-center gap-1.5">{children}</span>
    </div>
  );
}
