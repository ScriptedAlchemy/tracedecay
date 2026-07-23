import { StateChip } from '../../ui/StateChip';

/** One-line status strip: daemon connection, live progress, receipts.
 * Wired to the SSE reducer's liveness once the /api/events stream lands;
 * until then it truthfully reports offline. */
export function StatusStrip() {
  return (
    <footer
      className="flex h-7 shrink-0 items-center gap-3 border-t border-edge-subtle bg-surface-1 px-3"
      aria-label="Status"
    >
      <StateChip kind="offline" detail="event stream not connected" />
      <div className="flex-1" />
      <span className="text-2xs text-text-muted tabular">PR14 build-out</span>
    </footer>
  );
}
