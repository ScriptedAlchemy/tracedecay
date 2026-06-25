import type { ChangeEvent } from "react";
import { Power } from "lucide-react";

import { Button, Input } from "../sdk";
import { Spinner } from "../Spinner";
import type { AutomationTaskConfig } from "../types";
import type { AutomationRunTask, AutomationTaskDescriptor } from "./automationTasks";
import type { SecondsField, TaskField } from "./configTypes";

const SECONDS_FIELDS: Array<{ key: SecondsField; label: string }> = [
  { key: "interval_secs", label: "Interval seconds" },
  { key: "cooldown_secs", label: "Cooldown seconds" },
  { key: "min_idle_secs", label: "Idle seconds" },
  { key: "stale_lock_secs", label: "Stale lock seconds" },
];

export function ConfigFieldError({ message }: { message?: string }) {
  if (!message) return null;
  return <div className="text-[11px] leading-snug text-destructive">{message}</div>;
}

export function AutomationTaskConfigRow({
  descriptor,
  config,
  actioningTask,
  activeStatus,
  canRun,
  runTitle,
  runLabel,
  fieldError,
  onTaskPatch,
  onTaskSecondsPatch,
  onRun,
}: {
  descriptor: AutomationTaskDescriptor;
  config: AutomationTaskConfig;
  actioningTask: AutomationRunTask | null;
  activeStatus?: string;
  canRun: boolean;
  runTitle: string;
  runLabel: string;
  fieldError: (task: AutomationRunTask, field: TaskField) => string | undefined;
  onTaskPatch: (task: AutomationRunTask, patch: Partial<AutomationTaskConfig>) => void;
  onTaskSecondsPatch: (task: AutomationRunTask, key: SecondsField, value: string) => void;
  onRun: (task: AutomationRunTask) => void;
}) {
  const task = descriptor.id;
  const running = actioningTask === task || activeStatus === "running";

  return (
    <div className="grid gap-2 sm:grid-cols-2">
      <label className="flex items-center gap-2 text-xs text-text-secondary">
        <input
          aria-label={descriptor.enabledLabel}
          type="checkbox"
          checked={config.enabled}
          onChange={(event) =>
            onTaskPatch(task, {
              enabled: event.currentTarget.checked,
            })
          }
        />
        {descriptor.enabledLabel}
      </label>
      <div className="flex min-w-0 items-end gap-2">
        <label className="grid min-w-0 flex-1 gap-1 text-xs text-text-secondary">
          {descriptor.scheduleLabel}
          <Input
            aria-label={descriptor.scheduleLabel}
            value={config.schedule ?? ""}
            onChange={(event: ChangeEvent<HTMLInputElement>) =>
              onTaskPatch(task, {
                schedule: event.currentTarget.value || null,
              })
            }
          />
          <ConfigFieldError message={fieldError(task, "schedule")} />
        </label>
        <Button
          size="xs"
          outlined
          disabled={!canRun || !config.enabled}
          title={runTitle}
          onClick={() => onRun(task)}
          className="shrink-0 gap-1.5"
        >
          {running ? <Spinner /> : <Power className="h-3.5 w-3.5" />}
          {runLabel}
        </Button>
      </div>
      <div className="grid gap-2 sm:col-span-2 sm:grid-cols-4">
        {SECONDS_FIELDS.map(({ key, label }) => (
          <label key={key} className="grid gap-1 text-xs text-text-secondary">
            {label}
            <Input
              aria-label={`${descriptor.runAriaLabel} ${label.toLowerCase()}`}
              type="number"
              min={1}
              value={config[key] ?? ""}
              onChange={(event: ChangeEvent<HTMLInputElement>) =>
                onTaskSecondsPatch(task, key, event.currentTarget.value)
              }
            />
            <ConfigFieldError message={fieldError(task, key)} />
          </label>
        ))}
      </div>
    </div>
  );
}
