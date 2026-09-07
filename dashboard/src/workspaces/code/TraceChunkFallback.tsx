import { ArrowLeft } from 'lucide-react';

import { displayName } from './hubs.ts';
import type { TraceFocus } from './TraceView.tsx';

/**
 * What stands in the list slot while the trace chunk is being fetched.
 *
 * Its geometry matches `TraceView`, but it never sketches a field or a numeric
 * reading: no neighbour request has been issued yet. The chosen focus is the
 * only domain fact already in hand.
 */
export function TraceChunkFallback({
  focus,
  onClose,
}: {
  focus: TraceFocus;
  onClose: () => void;
}) {
  return (
    <div className="flex h-full min-h-0 flex-col" data-testid="trace-chunk-fallback">
      <header className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <button
          type="button"
          onClick={onClose}
          className="flex shrink-0 items-center gap-1 text-2xs text-text-muted hover:text-text-primary focus-visible:text-text-primary"
        >
          <ArrowLeft aria-hidden size={12} />
          Back to spine
        </button>
        <span aria-hidden className="td-rule" />
        <h2 className="td-title min-w-0 truncate">
          <span className="text-text-muted">trace · </span>
          {displayName(focus)}
        </h2>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        <p
          role="status"
          className="px-2.5 py-2 text-2xs leading-relaxed text-state-loading"
        >
          loading the trace view — the code for this surface is still arriving. No
          call edge has been requested yet, so nothing here is an empty
          neighbourhood, a zero or a settled field.
        </p>
      </div>
    </div>
  );
}
