import { Button } from "../sdk";
import { Spinner } from "../Spinner";
import { schedulerTaskLabel } from "./automationTasks";
import type { AutomationSchedulerStatusResponse } from "../types";

export function SchedulerStatusSection({
  status,
  loading,
  error,
  actioning,
  onSetPaused,
}: {
  status: AutomationSchedulerStatusResponse | null;
  loading: boolean;
  error: string;
  actioning: boolean;
  onSetPaused: (paused: boolean) => void;
}) {
  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
            Scheduler
          </div>
          <div className="mt-0.5 text-xs text-text-secondary">
            {status?.status ?? "unknown"}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {loading ? <Spinner /> : null}
          <Button
            size="xs"
            outlined
            disabled={actioning}
            onClick={() => onSetPaused(!status?.paused)}
            className="gap-2"
          >
            {actioning ? <Spinner /> : null}
            {status?.paused ? "Resume" : "Pause"}
          </Button>
        </div>
      </div>
      {error ? (
        <div className="mt-2 border border-destructive/30 bg-destructive/10 px-2 py-1 text-xs text-destructive">
          {error}
        </div>
      ) : null}
      {status?.tasks?.length ? (
        <div className="mt-2 grid gap-1.5">
          {status.tasks.map((task) => (
            <div
              key={task.task}
              className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2 border border-border/60 bg-background/40 px-2 py-1.5 text-xs"
            >
              <span className="min-w-0 truncate text-text-secondary">
                {schedulerTaskLabel(task.task)}
              </span>
              <span
                className={`rounded-sm border px-1.5 py-0.5 text-[10px] uppercase tracking-[0.08em] ${
                  task.due
                    ? "border-success/30 bg-success/10 text-success"
                    : "border-border bg-muted/30 text-text-tertiary"
                }`}
              >
                {task.due ? "due" : (task.skip_reason ?? "skipped")}
              </span>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
