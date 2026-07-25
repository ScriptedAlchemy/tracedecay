import type { ReactNode } from 'react';
import { useEventStreamState } from '../../data/sse/useEvents.tsx';
import { cn } from '../../ui/cn';

/** Telemetry bar for the real `/api/events` connection state. */
export function StatusStrip() {
  const { state } = useEventStreamState();
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
