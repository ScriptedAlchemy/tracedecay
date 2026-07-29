import type { ReactNode } from 'react';
import {
  useEventStreamState,
  useProjectionSync,
  type ProjectionSync,
} from '../../data/sse/useEvents.tsx';
import { cn } from '../../ui/cn';

/**
 * What the transport is doing, and separately whether the data behind it has
 * been reconciled.
 *
 * These are two facts and the strip reports them as two, because a healthy
 * socket says nothing about whether the projection is current: a canonical
 * refresh can reject and leave the client knowingly behind while the link stays
 * `live`. Reading `live` alone in that state would be the strip claiming
 * freshness it has no evidence for.
 */
function feedReading(sync: ProjectionSync): {
  value: string;
  tone: string;
  detail: string | null;
} {
  switch (sync.kind) {
    case 'synced':
      return { value: 'synced', tone: 'bg-state-ready', detail: null };
    case 'resyncing':
      return { value: 'resyncing', tone: 'bg-state-loading', detail: null };
    case 'stale':
      return {
        value: 'stale',
        tone: 'bg-state-stale',
        detail: sync.reason === null ? 'a refresh is owed' : `a refresh is owed: ${sync.reason}`,
      };
    case 'failed':
      return {
        value: 'resync failed',
        tone: 'bg-state-error',
        detail: `the projection is behind and the refresh rejected: ${sync.reason}`,
      };
    case 'unmounted':
      return { value: 'no stream', tone: 'bg-state-offline', detail: null };
    default: {
      const unhandled: never = sync;
      return unhandled;
    }
  }
}

/** Telemetry bar for the real `/api/events` connection state. */
export function StatusStrip() {
  const { state } = useEventStreamState();
  const feed = feedReading(useProjectionSync());
  const link =
    state === 'live'
      ? { value: 'live', tone: 'bg-state-ready', live: true }
      : state === 'connecting'
        ? { value: 'connecting', tone: 'bg-state-loading', live: true }
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
      <Cell label="Feed">
        <span aria-hidden className={cn('size-1.5 shrink-0', feed.tone)} />
        <span className="td-value text-2xs uppercase" role="status">
          {feed.value}
        </span>
        {feed.detail !== null && (
          // The state is carried by the word, not the swatch; the reason is the
          // one thing a reader needs to know that the word cannot hold.
          <span className="td-value min-w-0 truncate text-3xs normal-case text-text-muted">
            {feed.detail}
          </span>
        )}
      </Cell>
      <span aria-hidden className="flex-1 border-r border-edge-subtle" />
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
