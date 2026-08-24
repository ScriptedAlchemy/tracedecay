import { formatOplogTime } from "./format";
import { oplogDetailSummary } from "./historyFormat";
import type { MemoryOplogEvent } from "../types";

export function MemoryOperationsSection({
  events,
  error,
}: {
  events: MemoryOplogEvent[];
  error: string;
}) {
  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="mb-1 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
        Recent memory operations
      </div>
      {error ? <div className="text-xs text-destructive">{error}</div> : null}
      {events.length ? (
        <div className="flex flex-col gap-1">
          {events.map((event) => (
            <div
              key={event.id}
              className="grid grid-cols-[7.5rem_5.5rem_minmax(0,1fr)] gap-2 font-mono-ui text-xs"
            >
              <span className="text-text-tertiary">{formatOplogTime(event.ts)}</span>
              <span className="truncate uppercase tracking-[0.08em] text-text-secondary">
                {event.op}
              </span>
              <span className="min-w-0 break-all text-text-tertiary">
                {event.fact_id != null ? `#${event.fact_id} ` : ""}
                {oplogDetailSummary(event)}
              </span>
            </div>
          ))}
        </div>
      ) : (
        <div className="text-xs text-text-tertiary">
          No memory operations recorded yet.
        </div>
      )}
    </div>
  );
}
