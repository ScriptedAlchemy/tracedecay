import type { RefObject } from "react";
import { ScrollText } from "lucide-react";

import { Spinner } from "../Spinner";
import type { MemoryCuratorActivityEvent } from "../types";

function activityTone(level?: string) {
  switch ((level || "info").toLowerCase()) {
    case "success":
      return "text-success";
    case "warning":
      return "text-warning";
    case "error":
      return "text-destructive";
    default:
      return "text-text-secondary";
  }
}

function formatActivityTime(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return "--:--:--";
  try {
    return d.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return "--:--:--";
  }
}

function activityStatus(events: MemoryCuratorActivityEvent[], loading: boolean): string {
  const latest = events[events.length - 1];
  if (!latest) return "idle";
  if (latest.phase === "stale") return "stale";
  if (loading) return "live";
  if (latest.phase === "finish") return "complete";
  if (latest.phase === "lock") return "skipped";
  return "recent";
}

function activityStatusClass(status: string): string {
  switch (status) {
    case "live":
      return "border-success/30 bg-success/10 text-success";
    case "stale":
      return "border-warning/30 bg-warning/10 text-warning";
    case "complete":
      return "border-primary/30 bg-primary/10 text-primary";
    default:
      return "border-border bg-muted/30 text-text-tertiary";
  }
}

export function ActivityScroller({
  events,
  loading,
  error,
  scrollRef,
}: {
  events: MemoryCuratorActivityEvent[];
  loading: boolean;
  error: string;
  scrollRef: RefObject<HTMLDivElement>;
}) {
  const status = activityStatus(events, loading);
  const stale = status === "stale";
  return (
    <div className="flex min-h-0 flex-1 flex-col border border-border bg-background/40">
      <div className="flex items-center justify-between gap-2 border-b border-border px-3 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <ScrollText className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <span className="text-xs font-medium text-foreground">Live Activity</span>
        </div>
        <div className="flex shrink-0 items-center gap-2 text-[11px] text-text-tertiary">
          {loading && !stale ? <Spinner /> : null}
          <span
            className={`rounded border px-1.5 py-0.5 uppercase tracking-[0.08em] ${activityStatusClass(status)}`}
          >
            {status}
          </span>
          <span>{events.length} events</span>
        </div>
      </div>
      {error ? (
        <div className="border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {error}
        </div>
      ) : null}
      {stale ? (
        <div className="border-b border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
          The last curator run stopped reporting activity. Refresh or start a new preview to resume from a fresh run.
        </div>
      ) : null}
      <div
        ref={scrollRef}
        className="min-h-[12rem] flex-1 overflow-y-auto overflow-x-hidden p-3 font-mono-ui text-xs"
      >
        {events.length === 0 ? (
          <div className="text-text-tertiary">
            Start a preview or apply run to watch curator activity here.
          </div>
        ) : (
          <div className="flex flex-col gap-1.5">
            {events.map((event, index) => (
              <div
                key={`${event.ts}-${index}`}
                className="grid grid-cols-[4.5rem_5.5rem_minmax(0,1fr)] gap-2"
              >
                <span className="text-text-tertiary">{formatActivityTime(event.ts)}</span>
                <span className="truncate uppercase tracking-[0.08em] text-text-tertiary">
                  {event.phase}
                </span>
                <span className={`min-w-0 break-words ${activityTone(event.level)}`}>
                  {event.message}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
