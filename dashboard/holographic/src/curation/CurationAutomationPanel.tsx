import { AutomationConfigSection } from "./AutomationConfigSection";
import { AutomationRunsSection } from "./AutomationRunsSection";
import { CurationTabPanel } from "./CurationTabPanel";
import { SchedulerStatusSection } from "./SchedulerStatusSection";
import type { AutomationRunTask } from "./automationTasks";
import type { ConfigFieldErrors, SecondsField, TaskField } from "./configTypes";
import type {
  AutomationSchedulerStatusResponse,
  AutomationTaskConfig,
  MemoryAutomationConfig,
  MemoryAutomationConfigPatch,
  MemoryAutomationRunArtifactPayloadResponse,
  MemoryAutomationRunArtifactsResponse,
  MemoryAutomationRunRecord,
  MemoryCuratorStatusResponse,
} from "../types";

interface CurationAutomationPanelProps {
  status: MemoryCuratorStatusResponse | null;
  statusLoading: boolean;
  statusError: string;
  automationRuns: MemoryAutomationRunRecord[];
  automationRunsError: string;
  automationRunActioning: AutomationRunTask | null;
  automationRunError: string;
  automationRunArtifacts: MemoryAutomationRunArtifactsResponse | null;
  automationRunArtifact: MemoryAutomationRunArtifactPayloadResponse | null;
  automationRunArtifactLoading: string | null;
  automationRunArtifactError: string;
  configDraft: MemoryAutomationConfig | null;
  configLoading: boolean;
  configSaving: boolean;
  configResetting: boolean;
  configError: string;
  configFieldErrors: ConfigFieldErrors;
  schedulerStatus: AutomationSchedulerStatusResponse | null;
  schedulerStatusLoading: boolean;
  schedulerStatusError: string;
  schedulerActioning: boolean;
  configDirty: boolean;
  backendUnavailable: boolean;
  backendUnavailableReason: string;
  activeAutomationStatus: (task: AutomationRunTask) => string | undefined;
  automationTaskCanRun: (task: AutomationRunTask) => boolean;
  automationTaskTitle: (task: AutomationRunTask) => string;
  automationTaskLabel: (task: AutomationRunTask) => string;
  taskFieldError: (
    task: AutomationRunTask,
    field: TaskField,
  ) => string | undefined;
  loadStatus: () => void;
  loadAutomationRuns: () => void;
  loadSchedulerStatus: (force?: boolean) => void;
  loadAutomationRunArtifact: (runId: string, kind: string) => void;
  runAutomationTask: (task: AutomationRunTask) => void;
  setSchedulerPaused: (paused: boolean) => void;
  updateConfigDraft: (patch: MemoryAutomationConfigPatch) => void;
  updateConfigTaskDraft: (
    task: AutomationRunTask,
    patch: Partial<AutomationTaskConfig>,
  ) => void;
  updateTaskSeconds: (
    task: AutomationRunTask,
    key: SecondsField,
    value: string,
  ) => void;
  resetConfigDraft: () => void;
  resetConfigToDefaults: () => Promise<void>;
  saveConfigDraft: () => Promise<void>;
}

export function CurationAutomationPanel({
  status,
  statusLoading,
  statusError,
  automationRuns,
  automationRunsError,
  automationRunActioning,
  automationRunError,
  automationRunArtifacts,
  automationRunArtifact,
  automationRunArtifactLoading,
  automationRunArtifactError,
  configDraft,
  configLoading,
  configSaving,
  configResetting,
  configError,
  configFieldErrors,
  schedulerStatus,
  schedulerStatusLoading,
  schedulerStatusError,
  schedulerActioning,
  configDirty,
  backendUnavailable,
  backendUnavailableReason,
  activeAutomationStatus,
  automationTaskCanRun,
  automationTaskTitle,
  automationTaskLabel,
  taskFieldError,
  loadStatus,
  loadAutomationRuns,
  loadSchedulerStatus,
  loadAutomationRunArtifact,
  runAutomationTask,
  setSchedulerPaused,
  updateConfigDraft,
  updateConfigTaskDraft,
  updateTaskSeconds,
  resetConfigDraft,
  resetConfigToDefaults,
  saveConfigDraft,
}: CurationAutomationPanelProps) {
  return (
    <CurationTabPanel
      tab="automation"
      title="Automation"
      subtitle="Scheduler, task config, manual runs, and the run ledger."
      refreshing={statusLoading}
      onRefresh={() => {
        loadStatus();
        loadSchedulerStatus(true);
        loadAutomationRuns();
      }}
      error={statusError}
    >
      <SchedulerStatusSection
        status={schedulerStatus}
        loading={schedulerStatusLoading}
        error={schedulerStatusError}
        actioning={schedulerActioning}
        onSetPaused={setSchedulerPaused}
      />
      <AutomationConfigSection
        configDraft={configDraft}
        configLoading={configLoading}
        configSaving={configSaving}
        configResetting={configResetting}
        configError={configError}
        configFieldErrors={configFieldErrors}
        configDirty={configDirty}
        backendUnavailable={backendUnavailable}
        backendUnavailableReason={backendUnavailableReason}
        automationRunActioning={automationRunActioning}
        automationRunError={automationRunError}
        paused={Boolean(status?.state.paused)}
        activeAutomationStatus={activeAutomationStatus}
        automationTaskCanRun={automationTaskCanRun}
        automationTaskTitle={automationTaskTitle}
        automationTaskLabel={automationTaskLabel}
        taskFieldError={taskFieldError}
        runAutomationTask={runAutomationTask}
        updateConfigDraft={updateConfigDraft}
        updateConfigTaskDraft={updateConfigTaskDraft}
        updateTaskSeconds={updateTaskSeconds}
        resetConfigDraft={resetConfigDraft}
        resetConfigToDefaults={resetConfigToDefaults}
        saveConfigDraft={saveConfigDraft}
      />
      <AutomationRunsSection
        runs={automationRuns}
        error={automationRunsError}
        artifacts={automationRunArtifacts}
        artifact={automationRunArtifact}
        artifactLoading={automationRunArtifactLoading}
        artifactError={automationRunArtifactError}
        onLoadArtifact={loadAutomationRunArtifact}
      />
    </CurationTabPanel>
  );
}
