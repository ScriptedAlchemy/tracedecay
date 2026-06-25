import type { ChangeEvent } from "react";
import { RotateCcw, Save } from "lucide-react";

import { Button, Input } from "../sdk";
import { Spinner } from "../Spinner";
import type {
  AutomationTaskConfig,
  MemoryAutomationConfig,
  MemoryAutomationConfigPatch,
  SelectableAutomationBackend,
} from "../types";
import { AutomationTaskConfigRow, ConfigFieldError } from "./AutomationTaskConfigRow";
import { AUTOMATION_TASKS, type AutomationRunTask } from "./automationTasks";
import type { ConfigFieldErrors, SecondsField, TaskField } from "./configTypes";
import { MetadataRow } from "./MetadataRow";

function ConfigCheckbox({
  label,
  ariaLabel,
  checked,
  error,
  onCheckedChange,
}: {
  label: string;
  ariaLabel: string;
  checked: boolean;
  error?: string;
  onCheckedChange: (checked: boolean) => void;
}) {
  return (
    <div className="grid gap-1">
      <label className="flex items-center gap-2 text-xs text-text-secondary">
        <input
          aria-label={ariaLabel}
          type="checkbox"
          checked={checked}
          onChange={(event) => onCheckedChange(event.currentTarget.checked)}
        />
        {label}
      </label>
      <ConfigFieldError message={error} />
    </div>
  );
}

export function AutomationConfigSection({
  configDraft,
  configLoading,
  configSaving,
  configResetting,
  configError,
  configFieldErrors,
  configDirty,
  backendUnavailable,
  backendUnavailableReason,
  automationRunActioning,
  automationRunError,
  paused,
  activeAutomationStatus,
  automationTaskCanRun,
  automationTaskTitle,
  automationTaskLabel,
  taskFieldError,
  runAutomationTask,
  updateConfigDraft,
  updateConfigTaskDraft,
  updateTaskSeconds,
  resetConfigDraft,
  resetConfigToDefaults,
  saveConfigDraft,
}: {
  configDraft: MemoryAutomationConfig | null;
  configLoading: boolean;
  configSaving: boolean;
  configResetting: boolean;
  configError: string;
  configFieldErrors: ConfigFieldErrors;
  configDirty: boolean;
  backendUnavailable: boolean;
  backendUnavailableReason: string;
  automationRunActioning: AutomationRunTask | null;
  automationRunError: string;
  paused: boolean;
  activeAutomationStatus: (task: AutomationRunTask) => string | undefined;
  automationTaskCanRun: (task: AutomationRunTask) => boolean;
  automationTaskTitle: (task: AutomationRunTask) => string;
  automationTaskLabel: (task: AutomationRunTask) => string;
  taskFieldError: (task: AutomationRunTask, field: TaskField) => string | undefined;
  runAutomationTask: (task: AutomationRunTask) => void;
  updateConfigDraft: (patch: MemoryAutomationConfigPatch) => void;
  updateConfigTaskDraft: (task: AutomationRunTask, patch: Partial<AutomationTaskConfig>) => void;
  updateTaskSeconds: (task: AutomationRunTask, key: SecondsField, value: string) => void;
  resetConfigDraft: () => void;
  resetConfigToDefaults: () => Promise<void>;
  saveConfigDraft: () => Promise<void>;
}) {
  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="flex items-center justify-between gap-2">
        <div className="text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
          Automation config
        </div>
        {configLoading ? <Spinner /> : null}
      </div>
      {configError ? (
        <div className="mt-2 border border-destructive/30 bg-destructive/10 px-2 py-1 text-xs text-destructive">
          {configError}
        </div>
      ) : null}
      {backendUnavailable ? (
        <div className="mt-2 border border-warning/30 bg-warning/10 px-2 py-1 text-xs text-warning">
          {backendUnavailableReason}
        </div>
      ) : null}
      {automationRunError ? (
        <div className="mt-2 border border-destructive/30 bg-destructive/10 px-2 py-1 text-xs text-destructive">
          {automationRunError}
        </div>
      ) : null}
      {configDraft ? (
        <div className="mt-2 grid gap-2">
          <div className="grid gap-2 sm:grid-cols-2">
            <ConfigCheckbox
              label="Enable automation"
              ariaLabel="Enable automation"
              checked={configDraft.enabled}
              onCheckedChange={(enabled) => updateConfigDraft({ enabled })}
            />
            <ConfigCheckbox
              label="Require approval"
              ariaLabel="Require dashboard approval"
              checked={configDraft.require_dashboard_approval}
              error={configFieldErrors.require_dashboard_approval}
              onCheckedChange={(require_dashboard_approval) =>
                updateConfigDraft({ require_dashboard_approval })
              }
            />
          </div>
          <div className="grid gap-2 sm:grid-cols-2">
            <label className="grid gap-1 text-xs text-text-secondary">
              Backend
              <select
                aria-label="Backend"
                className="h-8 border border-border bg-background px-2 text-xs"
                value={configDraft.backend}
                onChange={(event) =>
                  updateConfigDraft({
                    backend: event.currentTarget.value as SelectableAutomationBackend,
                  })
                }
              >
                <option value="disabled">Disabled</option>
                <option value="codex_app_server">Codex app server</option>
              </select>
              <ConfigFieldError message={configFieldErrors.backend} />
            </label>
            <label className="grid gap-1 text-xs text-text-secondary">
              Host mode
              <select
                aria-label="Host mode"
                className="h-8 border border-border bg-background px-2 text-xs"
                value={configDraft.host_mode}
                onChange={(event) =>
                  updateConfigDraft({
                    host_mode: event.currentTarget.value as typeof configDraft.host_mode,
                  })
                }
              >
                <option value="standalone">Standalone</option>
                <option value="delegated_host">Delegated host</option>
              </select>
              <ConfigFieldError message={configFieldErrors.host_mode} />
            </label>
          </div>
          <div className="grid gap-2 sm:grid-cols-[1fr_8rem_8rem]">
            <label className="grid gap-1 text-xs text-text-secondary">
              Model
              <Input
                aria-label="Model"
                value={configDraft.model ?? ""}
                onChange={(event: ChangeEvent<HTMLInputElement>) =>
                  updateConfigDraft({ model: event.currentTarget.value || null })
                }
              />
            </label>
            <label className="grid gap-1 text-xs text-text-secondary">
              Timeout
              <Input
                aria-label="Timeout seconds"
                type="number"
                min={1}
                value={configDraft.timeout_secs}
                onChange={(event: ChangeEvent<HTMLInputElement>) =>
                  updateConfigDraft({
                    timeout_secs: Math.max(1, Number(event.currentTarget.value) || 1),
                  })
                }
              />
              <ConfigFieldError message={configFieldErrors.timeout_secs} />
            </label>
            <label className="grid gap-1 text-xs text-text-secondary">
              Scheduler tick
              <Input
                aria-label="Scheduler tick seconds"
                type="number"
                min={1}
                value={configDraft.scheduler_tick_secs}
                onChange={(event: ChangeEvent<HTMLInputElement>) =>
                  updateConfigDraft({
                    scheduler_tick_secs: Math.max(1, Number(event.currentTarget.value) || 1),
                  })
                }
              />
              <ConfigFieldError message={configFieldErrors.scheduler_tick_secs} />
            </label>
          </div>
          <div className="grid gap-2 sm:grid-cols-2">
            <label className="grid gap-1 text-xs text-text-secondary">
              Max tokens
              <Input
                aria-label="Max tokens"
                type="number"
                min={1}
                value={configDraft.max_tokens ?? ""}
                onChange={(event: ChangeEvent<HTMLInputElement>) =>
                  updateConfigDraft({
                    max_tokens: event.currentTarget.value
                      ? Math.max(1, Number(event.currentTarget.value) || 1)
                      : null,
                  })
                }
              />
              <ConfigFieldError message={configFieldErrors.max_tokens} />
            </label>
            <label className="grid gap-1 text-xs text-text-secondary">
              Temperature
              <Input
                aria-label="Temperature"
                type="number"
                min={0}
                step={0.1}
                value={configDraft.temperature ?? ""}
                onChange={(event: ChangeEvent<HTMLInputElement>) =>
                  updateConfigDraft({
                    temperature: event.currentTarget.value
                      ? Math.max(0, Number(event.currentTarget.value) || 0)
                      : null,
                  })
                }
              />
              <ConfigFieldError message={configFieldErrors.temperature} />
            </label>
          </div>
          {AUTOMATION_TASKS.map((descriptor) => (
            <div key={descriptor.id}>
              <AutomationTaskConfigRow
                descriptor={descriptor}
                config={configDraft.tasks[descriptor.id]}
                actioningTask={automationRunActioning}
                activeStatus={activeAutomationStatus(descriptor.id)}
                canRun={automationTaskCanRun(descriptor.id)}
                runTitle={automationTaskTitle(descriptor.id)}
                runLabel={automationTaskLabel(descriptor.id)}
                fieldError={taskFieldError}
                onTaskPatch={updateConfigTaskDraft}
                onTaskSecondsPatch={updateTaskSeconds}
                onRun={runAutomationTask}
              />
            </div>
          ))}
          <div className="grid gap-2 sm:grid-cols-2">
            <ConfigCheckbox
              label="Auto-apply memory ops"
              ariaLabel="Auto-apply memory ops"
              checked={configDraft.auto_apply_memory_ops}
              error={configFieldErrors.auto_apply_memory_ops}
              onCheckedChange={(auto_apply_memory_ops) =>
                updateConfigDraft({ auto_apply_memory_ops })
              }
            />
            <ConfigCheckbox
              label="Auto-enable skills"
              ariaLabel="Auto-enable skills"
              checked={configDraft.auto_enable_skills}
              error={configFieldErrors.auto_enable_skills}
              onCheckedChange={(auto_enable_skills) => updateConfigDraft({ auto_enable_skills })}
            />
          </div>
          <div className="flex items-center justify-end gap-2 pt-1">
            <Button
              size="xs"
              outlined
              disabled={!configDirty || configSaving || configResetting}
              onClick={resetConfigDraft}
            >
              Discard edits
            </Button>
            <Button
              size="xs"
              outlined
              disabled={configSaving || configResetting}
              onClick={() => {
                void resetConfigToDefaults().catch(() => {});
              }}
              className="gap-1.5"
            >
              {configResetting ? <Spinner /> : <RotateCcw className="h-3.5 w-3.5" />}
              Reset defaults
            </Button>
            <Button
              size="xs"
              disabled={!configDirty || configSaving || configResetting}
              onClick={() => {
                void saveConfigDraft().catch(() => {});
              }}
              className="gap-1.5"
            >
              {configSaving ? <Spinner /> : <Save className="h-3.5 w-3.5" />}
              Save config
            </Button>
          </div>
        </div>
      ) : null}
      <MetadataRow label="Paused" value={paused ? "yes" : "no"} />
    </div>
  );
}
