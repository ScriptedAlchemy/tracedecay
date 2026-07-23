import { StateChip } from '../../ui/StateChip';
import { useEventStreamState } from '../../data/sse/useEvents.tsx';

/** One-line status strip: daemon connection, live progress, receipts.
 * Liveness is the real /api/events stream state — never simulated. */
export function StatusStrip() {
  const { state } = useEventStreamState();
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
      <div className="flex-1" />
      <span className="text-2xs text-text-muted tabular">PR14 build-out</span>
    </footer>
  );
}
