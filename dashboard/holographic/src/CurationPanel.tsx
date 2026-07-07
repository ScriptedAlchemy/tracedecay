import {
  Bot,
  History,
  Inbox,
  ScrollText,
  Wand2,
} from "lucide-react";
import { Button, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import { Spinner } from "./Spinner";
import { ActivityScroller } from "./curation/ActivityScroller";
import { useCurationData, type CurationTab } from "./curation/useCurationData";
import { usePendingAutomationCounts } from "./curation/usePendingAutomationCounts";
import { api } from "./api";
import { CurationAutomationPanel } from "./curation/CurationAutomationPanel";
import { CurationProposalsPanel } from "./curation/CurationProposalsPanel";
import { CurationHistoryPanel } from "./curation/CurationHistoryPanel";
import {
  isActiveAutomationStatus,
  type AutomationRunTask,
} from "./curation/automationTasks";
import type { SecondsField, TaskField } from "./curation/configTypes";

export default function CurationPanel({
  onApplied,
}: {
  onApplied?: () => void;
}) {
  const {
    activeTab,
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
    managedSkillUsage,
    managedSkillRecommendations,
    managedSkillImprovementRecommendations,
    managedSkillsLoading,
    managedSkillsError,
    managedSkillActioning,
    managedSkillExports,
    activity,
    activityLoading,
    activityError,
    configResponse,
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
    activityRef,
    panelRef,
    setActiveTab,
    loadActivity,
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
    resetConfigDraft,
    resetConfigToDefaults,
    saveConfigDraft,
  } = useCurationData({ onApplied });

  const backendAvailability = configResponse?.backend_availability;
  const backendUnavailable =
    !configDirty &&
    configDraft?.backend === "codex_app_server" &&
    configDraft?.host_mode === "standalone" &&
    backendAvailability?.available === false;
  const backendUnavailableReason =
    backendAvailability?.reason ?? "Codex app-server backend is unavailable";
  const automationCanRun =
    Boolean(configDraft?.enabled) &&
    configDraft?.backend === "codex_app_server" &&
    configDraft?.host_mode === "standalone" &&
    !backendUnavailable &&
    !configDirty &&
    !configSaving &&
    !automationRunActioning;
  const activeAutomationStatus = (task: AutomationRunTask) =>
    automationRuns.find(
      (record) =>
        record.task === task && isActiveAutomationStatus(record.status),
    )?.status;
  const automationRunTitle = configDirty
    ? "Save automation config before running"
    : configDraft?.host_mode === "delegated_host"
      ? "Delegated host mode owns intelligence runs"
      : configDraft?.backend !== "codex_app_server"
        ? "Select the Codex app-server backend before running"
        : backendUnavailable
          ? backendUnavailableReason
          : !configDraft?.enabled
            ? "Enable automation before running"
            : "Run now";
  const automationTaskTitle = (task: AutomationRunTask) => {
    const status = activeAutomationStatus(task);
    if (status === "queued") return "Automation run is queued";
    if (status === "running") return "Automation run is running";
    return automationRunTitle;
  };
  const automationTaskLabel = (task: AutomationRunTask) => {
    const status = activeAutomationStatus(task);
    if (status === "queued") return "Queued";
    if (status === "running") return "Running";
    return "Run";
  };
  const automationTaskCanRun = (task: AutomationRunTask) =>
    automationCanRun && !activeAutomationStatus(task);
  const updateTaskSeconds = (
    task: AutomationRunTask,
    key: SecondsField,
    value: string,
  ) => {
    updateConfigTaskDraft(task, {
      [key]: value ? Math.max(1, Number(value) || 1) : null,
    });
  };
  const taskFieldError = (task: AutomationRunTask, field: TaskField) =>
    configFieldErrors[`${task}.${field}`];
  const selectedUsage = selectedManagedSkill
    ? managedSkillUsage[selectedManagedSkill.metadata.id]
    : null;
  const selectedRecommendation = selectedManagedSkill
    ? managedSkillRecommendations[selectedManagedSkill.metadata.id]
    : null;
  const selectedImprovementRecommendation = selectedManagedSkill
    ? managedSkillImprovementRecommendations[selectedManagedSkill.metadata.id]
    : null;
  // Same server-side count the outer Curation tab badge uses, so the two
  // badges can't disagree and no proposal/skill lists need loading up front.
  const pendingReviewCount = usePendingAutomationCounts(api);
  const proposalsLabel = pendingReviewCount
    ? `Proposals ${pendingReviewCount}`
    : "Proposals";
  const tabs: Array<{ id: CurationTab; label: string; Icon: typeof Wand2 }> = [
    { id: "automation", label: "Automation", Icon: Bot },
    { id: "proposals", label: proposalsLabel, Icon: Inbox },
    { id: "history", label: "History", Icon: History },
    { id: "activity", label: "Activity", Icon: ScrollText },
  ];

  return (
    <Card className="overflow-hidden flex flex-col max-h-[80vh] md:max-h-[46rem] min-w-0">
      <CardHeader className="flex flex-col sm:flex-row sm:items-center justify-between gap-2 shrink-0">
        <CardTitle className="flex items-center gap-2">
          <Wand2 className="h-4 w-4" />
          Curation
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 flex-1 min-h-0 overflow-hidden">
        <p className="text-xs text-text-tertiary shrink-0">
          Autonomous curation runs through automation. Use this panel for
          schedule, run history, proposals, and live activity.
        </p>

        <div
          ref={panelRef}
          role="tablist"
          aria-label="Curation views"
          className="grid grid-cols-4 gap-1 rounded-sm border border-border bg-secondary/30 p-1 shrink-0"
        >
          {tabs.map(({ id, label, Icon }) => {
            const active = activeTab === id;
            return (
              <button
                key={id}
                type="button"
                role="tab"
                id={`curation-tab-${id}`}
                aria-selected={active}
                aria-controls={`curation-panel-${id}`}
                tabIndex={0}
                onClick={() => setActiveTab(id)}
                className={`flex min-w-0 items-center justify-center gap-1.5 px-2 py-1.5 text-xs ${
                  active
                    ? "bg-background text-foreground shadow-sm ring-1 ring-inset ring-primary/60"
                    : "text-text-tertiary hover:text-text-secondary"
                }`}
              >
                <Icon className="h-3.5 w-3.5 shrink-0" />
                <span className="truncate">{label}</span>
              </button>
            );
          })}
        </div>

        {activeTab === "activity" ? (
          <div
            role="tabpanel"
            id="curation-panel-activity"
            aria-labelledby="curation-tab-activity"
            className="flex flex-1 min-h-0 flex-col gap-3"
          >
            <div className="flex min-w-0 items-center justify-between gap-2 shrink-0">
              <div className="min-w-0">
                <div className="text-xs font-medium text-foreground">
                  Curator Activity
                </div>
                <div className="text-[11px] text-text-tertiary">
                  Live phases from autonomous curator runs.
                </div>
              </div>
              <Button
                size="xs"
                ghost
                disabled={activityLoading}
                onClick={() => loadActivity(true)}
                className="shrink-0 gap-2"
              >
                {activityLoading ? <Spinner /> : null}
                Refresh
              </Button>
            </div>
            <ActivityScroller
              events={activity}
              loading={activityLoading}
              error={activityError}
              scrollRef={activityRef}
            />
          </div>
        ) : null}

        {activeTab === "automation" ? (
          <CurationAutomationPanel
            status={status}
            statusLoading={statusLoading}
            statusError={statusError}
            automationRuns={automationRuns}
            automationRunsError={automationRunsError}
            automationRunActioning={automationRunActioning}
            automationRunError={automationRunError}
            automationRunArtifacts={automationRunArtifacts}
            automationRunArtifact={automationRunArtifact}
            automationRunArtifactLoading={automationRunArtifactLoading}
            automationRunArtifactError={automationRunArtifactError}
            configDraft={configDraft}
            configLoading={configLoading}
            configSaving={configSaving}
            configResetting={configResetting}
            configError={configError}
            configFieldErrors={configFieldErrors}
            schedulerStatus={schedulerStatus}
            schedulerStatusLoading={schedulerStatusLoading}
            schedulerStatusError={schedulerStatusError}
            schedulerActioning={schedulerActioning}
            configDirty={configDirty}
            backendUnavailable={backendUnavailable}
            backendUnavailableReason={backendUnavailableReason}
            activeAutomationStatus={activeAutomationStatus}
            automationTaskCanRun={automationTaskCanRun}
            automationTaskTitle={automationTaskTitle}
            automationTaskLabel={automationTaskLabel}
            taskFieldError={taskFieldError}
            loadStatus={loadStatus}
            loadAutomationRuns={loadAutomationRuns}
            loadSchedulerStatus={loadSchedulerStatus}
            loadAutomationRunArtifact={loadAutomationRunArtifact}
            runAutomationTask={runAutomationTask}
            setSchedulerPaused={setSchedulerPaused}
            updateConfigDraft={updateConfigDraft}
            updateConfigTaskDraft={updateConfigTaskDraft}
            updateTaskSeconds={updateTaskSeconds}
            resetConfigDraft={resetConfigDraft}
            resetConfigToDefaults={resetConfigToDefaults}
            saveConfigDraft={saveConfigDraft}
          />
        ) : null}

        {activeTab === "proposals" ? (
          <CurationProposalsPanel
            factProposals={factProposals}
            factProposalsLoading={factProposalsLoading}
            factProposalsError={factProposalsError}
            factProposalActioning={factProposalActioning}
            managedSkills={managedSkills}
            selectedManagedSkillId={selectedManagedSkillId}
            selectedManagedSkill={selectedManagedSkill}
            selectedUsage={selectedUsage}
            selectedRecommendation={selectedRecommendation}
            selectedImprovementRecommendation={
              selectedImprovementRecommendation
            }
            managedSkillsLoading={managedSkillsLoading}
            managedSkillsError={managedSkillsError}
            managedSkillActioning={managedSkillActioning}
            managedSkillExports={managedSkillExports}
            loadFactProposals={loadFactProposals}
            loadManagedSkills={loadManagedSkills}
            loadManagedSkill={loadManagedSkill}
            runFactProposalAction={runFactProposalAction}
            runManagedSkillAction={runManagedSkillAction}
          />
        ) : null}

        {activeTab === "history" ? (
          <CurationHistoryPanel
            status={status}
            statusLoading={statusLoading}
            statusError={statusError}
            oplog={oplog}
            oplogError={oplogError}
            loadStatus={loadStatus}
            loadOplog={loadOplog}
          />
        ) : null}
      </CardContent>
    </Card>
  );
}
