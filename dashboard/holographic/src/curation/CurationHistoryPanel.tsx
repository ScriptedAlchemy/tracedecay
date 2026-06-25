import { Button } from "../sdk";
import { Spinner } from "../Spinner";
import { AutomationConfigSection } from "./AutomationConfigSection";
import { AutomationRunsSection } from "./AutomationRunsSection";
import { CurrentPreviewSection } from "./CurrentPreviewSection";
import { FactProposalsSection } from "./FactProposalsSection";
import { ManagedSkillsSection } from "./ManagedSkillsSection";
import { MemoryOperationsSection } from "./MemoryOperationsSection";
import { RunHistorySection } from "./RunHistorySection";
import { SchedulerStatusSection } from "./SchedulerStatusSection";
import { SnapshotsSection } from "./SnapshotsSection";
import type { AutomationRunTask } from "./automationTasks";
import type { ConfigFieldErrors, SecondsField, TaskField } from "./configTypes";
import type {
  AutomationSchedulerStatusResponse,
  AutomationTaskConfig,
  FactProposalRecord,
  ManagedSkill,
  MemoryAutomationConfig,
  MemoryAutomationConfigPatch,
  MemoryAutomationRunArtifactPayloadResponse,
  MemoryAutomationRunArtifactsResponse,
  MemoryAutomationRunRecord,
  MemoryCurateResponse,
  MemoryCuratorStatusResponse,
  MemoryOplogEvent,
  SkillImprovementRecommendation,
  SkillStaleRecommendation,
  SkillUsageSummary,
} from "../types";

interface CurationHistoryPanelProps {
  report: MemoryCurateResponse | null;
  previewSavedAt: string | null;
  previewStale: boolean;
  previewStaleReason: string;
  actionsLength: number;
  actionCounts: Array<[string, number]>;
  diagnosticCounts: Array<[string, number]>;
  isPlan: boolean;
  status: MemoryCuratorStatusResponse | null;
  statusLoading: boolean;
  statusError: string;
  oplog: MemoryOplogEvent[];
  oplogError: string;
  automationRuns: MemoryAutomationRunRecord[];
  automationRunsError: string;
  automationRunActioning: AutomationRunTask | null;
  automationRunError: string;
  automationRunArtifacts: MemoryAutomationRunArtifactsResponse | null;
  automationRunArtifact: MemoryAutomationRunArtifactPayloadResponse | null;
  automationRunArtifactLoading: string | null;
  automationRunArtifactError: string;
  factProposals: FactProposalRecord[];
  factProposalsLoading: boolean;
  factProposalsError: string;
  factProposalActioning: string | null;
  managedSkills: ManagedSkill[];
  selectedManagedSkillId: string | null;
  selectedManagedSkill: ManagedSkill | null;
  selectedUsage: SkillUsageSummary | null;
  selectedRecommendation: SkillStaleRecommendation | null;
  selectedImprovementRecommendation: SkillImprovementRecommendation | null;
  managedSkillsLoading: boolean;
  managedSkillsError: string;
  managedSkillActioning: string | null;
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
  taskFieldError: (task: AutomationRunTask, field: TaskField) => string | undefined;
  loadStatus: () => void;
  loadOplog: () => void;
  loadAutomationRuns: () => void;
  loadSchedulerStatus: (force?: boolean) => void;
  loadAutomationRunArtifact: (runId: string, kind: string) => void;
  loadFactProposals: (force?: boolean) => void;
  loadManagedSkills: (force?: boolean) => void;
  loadManagedSkill: (skillId: string) => void;
  runAutomationTask: (task: AutomationRunTask) => void;
  runFactProposalAction: (action: "apply" | "reject", proposalId: string) => void;
  runManagedSkillAction: (action: string, skillId: string) => void;
  setSchedulerPaused: (paused: boolean) => void;
  updateConfigDraft: (patch: MemoryAutomationConfigPatch) => void;
  updateConfigTaskDraft: (task: AutomationRunTask, patch: Partial<AutomationTaskConfig>) => void;
  updateTaskSeconds: (task: AutomationRunTask, key: SecondsField, value: string) => void;
  resetConfigDraft: () => void;
  resetConfigToDefaults: () => Promise<void>;
  saveConfigDraft: () => Promise<void>;
}

export function CurationHistoryPanel({
  report,
  previewSavedAt,
  previewStale,
  previewStaleReason,
  actionsLength,
  actionCounts,
  diagnosticCounts,
  isPlan,
  status,
  statusLoading,
  statusError,
  oplog,
  oplogError,
  automationRuns,
  automationRunsError,
  automationRunActioning,
  automationRunError,
  automationRunArtifacts,
  automationRunArtifact,
  automationRunArtifactLoading,
  automationRunArtifactError,
  factProposals,
  factProposalsLoading,
  factProposalsError,
  factProposalActioning,
  managedSkills,
  selectedManagedSkillId,
  selectedManagedSkill,
  selectedUsage,
  selectedRecommendation,
  selectedImprovementRecommendation,
  managedSkillsLoading,
  managedSkillsError,
  managedSkillActioning,
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
  loadOplog,
  loadAutomationRuns,
  loadSchedulerStatus,
  loadAutomationRunArtifact,
  loadFactProposals,
  loadManagedSkills,
  loadManagedSkill,
  runAutomationTask,
  runFactProposalAction,
  runManagedSkillAction,
  setSchedulerPaused,
  updateConfigDraft,
  updateConfigTaskDraft,
  updateTaskSeconds,
  resetConfigDraft,
  resetConfigToDefaults,
  saveConfigDraft,
}: CurationHistoryPanelProps) {
  return (
    <div
      role="tabpanel"
      id="curation-panel-history"
      aria-labelledby="curation-tab-history"
      className="flex flex-1 min-h-0 flex-col gap-3 overflow-y-auto overflow-x-hidden pr-1"
    >
      <div className="flex min-w-0 items-center justify-between gap-2 shrink-0">
        <div className="min-w-0">
          <div className="text-xs font-medium text-foreground">
            Curator Status
          </div>
          <div className="text-[11px] text-text-tertiary">
            Scheduler state, last run summary, and recent snapshots.
          </div>
        </div>
        <Button
          size="xs"
          ghost
          disabled={statusLoading}
          onClick={() => {
            loadStatus();
            loadSchedulerStatus(true);
            loadOplog();
            loadAutomationRuns();
            loadFactProposals(true);
            loadManagedSkills(true);
          }}
          className="shrink-0 gap-2"
        >
          {statusLoading ? <Spinner /> : null}
          Refresh
        </Button>
      </div>
      {statusError ? (
        <div className="border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive shrink-0">
          {statusError}
        </div>
      ) : null}
      {status ? (
        <>
          <RunHistorySection status={status} />
          <SchedulerStatusSection
            status={schedulerStatus}
            loading={schedulerStatusLoading}
            error={schedulerStatusError}
            actioning={Boolean(schedulerActioning)}
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
            paused={status.state.paused}
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
          <FactProposalsSection
            proposals={factProposals}
            loading={factProposalsLoading}
            error={factProposalsError}
            actioning={factProposalActioning}
            onRefresh={() => loadFactProposals(true)}
            onAction={runFactProposalAction}
          />
          <ManagedSkillsSection
            skills={managedSkills}
            selectedSkillId={selectedManagedSkillId}
            selectedSkill={selectedManagedSkill}
            selectedUsage={selectedUsage}
            selectedRecommendation={selectedRecommendation}
            selectedImprovementRecommendation={selectedImprovementRecommendation}
            loading={managedSkillsLoading}
            error={managedSkillsError}
            actioning={managedSkillActioning}
            onRefresh={() => loadManagedSkills(true)}
            onLoadSkill={loadManagedSkill}
            onAction={runManagedSkillAction}
          />
          <SnapshotsSection snapshots={status.snapshots} />
        </>
      ) : null}
      <AutomationRunsSection
        runs={automationRuns}
        error={automationRunsError}
        artifacts={automationRunArtifacts}
        artifact={automationRunArtifact}
        artifactLoading={automationRunArtifactLoading}
        artifactError={automationRunArtifactError}
        onLoadArtifact={loadAutomationRunArtifact}
      />
      <MemoryOperationsSection events={oplog} error={oplogError} />
      <CurrentPreviewSection
        report={report}
        previewSavedAt={previewSavedAt}
        previewStale={previewStale}
        previewStaleReason={previewStaleReason}
        actionsLength={actionsLength}
        actionCounts={actionCounts}
        diagnosticCounts={diagnosticCounts}
        isPlan={isPlan}
      />
    </div>
  );
}
