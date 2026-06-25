import {
  History,
  ListChecks,
  ScrollText,
  Wand2,
} from "lucide-react";
import { Button, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import { Spinner } from "./Spinner";
import {
  countLabel,
  formatHistoryTime,
} from "./curation/format";
import { groupActions } from "./curation/risk";
import { ActivityScroller } from "./curation/ActivityScroller";
import { ActionReviewGroup } from "./curation/ActionReviewGroup";
import {
  useCurationData,
  type CurationTab,
} from "./curation/useCurationData";
import { CurationHistoryPanel } from "./curation/CurationHistoryPanel";
import { InlineConfirm } from "./curation/InlineConfirm";
import { isActiveAutomationStatus, type AutomationRunTask } from "./curation/automationTasks";
import type { SecondsField, TaskField } from "./curation/configTypes";

const DIAGNOSTIC_COUNT_KEYS = new Set([
  "contradictions_detected",
  "entity_scan_remaining",
  "entity_total",
  "entities_scanned",
  "orphan_entities",
  "orphan_entities_pruned",
  "related_clusters",
]);

export default function CurationPanel({
  onApplied,
}: {
  onApplied?: () => void;
}) {
  const {
    report,
    loading,
    applying,
    previewSavedAt,
    previewStale,
    previewStaleReason,
    confirmOpen,
    error,
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
    setConfirmOpen,
    setActiveTab,
    preview,
    apply,
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

  const actions = report?.actions ?? [];
  const counts: Record<string, number> = report?.counts ?? {};
  const isPlan = report?.dry_run ?? true;
  const shownCounts: Record<string, number> = isPlan ? counts : (report?.applied_counts ?? counts);
  const actionCounts = Object.entries(shownCounts).filter(
    ([key]) => !DIAGNOSTIC_COUNT_KEYS.has(key),
  );
  const diagnosticCounts = Object.entries(counts).filter(([key]) =>
    DIAGNOSTIC_COUNT_KEYS.has(key),
  );
  const actionGroups = groupActions(actions);
  const nonEmptyActionGroups = actionGroups.filter((group) => group.actions.length > 0);
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
      (record) => record.task === task && isActiveAutomationStatus(record.status),
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
  const taskFieldError = (
    task: AutomationRunTask,
    field: TaskField,
  ) => configFieldErrors[`${task}.${field}`];
  const planLabel = actions.length ? `Plan ${actions.length}` : "Plan";
  const confirmGroupCounts = nonEmptyActionGroups.map((group) => [
    group.label,
    group.actions.length,
  ] as const);
  const selectedUsage = selectedManagedSkill
    ? managedSkillUsage[selectedManagedSkill.metadata.id]
    : null;
  const selectedRecommendation = selectedManagedSkill
    ? managedSkillRecommendations[selectedManagedSkill.metadata.id]
    : null;
  const selectedImprovementRecommendation = selectedManagedSkill
    ? managedSkillImprovementRecommendations[selectedManagedSkill.metadata.id]
    : null;
  const tabs: Array<{ id: CurationTab; label: string; Icon: typeof Wand2 }> = [
    { id: "plan", label: planLabel, Icon: ListChecks },
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
        <div className="flex items-center gap-2 shrink-0">
          <Button size="sm" ghost disabled={loading} onClick={preview} className="gap-2">
            {loading ? <Spinner /> : null}
            Preview
          </Button>
          <Button
            size="sm"
            disabled={!report || !isPlan || actions.length === 0 || applying}
            onClick={() => setConfirmOpen(true)}
            title={
              applying
                ? "Apply in progress…"
                : !report || !isPlan
                  ? "Run a Preview first to build a plan"
                  : actions.length === 0
                    ? "Nothing to apply — the last preview proposed no changes"
                    : "Apply the previewed plan (deletes flagged duplicates)"
            }
          >
            Apply
          </Button>
        </div>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 flex-1 min-h-0 overflow-hidden">
        <p className="text-xs text-text-tertiary shrink-0">
          Review a curation plan and check the latest run signals. Applying a
          plan permanently deletes the flagged duplicate facts.
        </p>

        <div
          ref={panelRef}
          role="tablist"
          aria-label="Curation views"
          className="grid grid-cols-3 gap-1 rounded-sm border border-border bg-secondary/30 p-1 shrink-0"
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

        {activeTab === "plan" ? (
          <div
            role="tabpanel"
            id="curation-panel-plan"
            aria-labelledby="curation-tab-plan"
            className="flex flex-col gap-3 flex-1 min-h-0 overflow-hidden"
          >
            {error && (
              <div className="border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive shrink-0">
                {error}
              </div>
            )}
            {previewStale ? (
              <div className="border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning shrink-0">
                {previewStaleReason || "This saved preview is stale because the memory store changed."}
              </div>
            ) : null}

            {report && (
              <div className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-text-secondary shrink-0">
                <span>{isPlan ? "proposed actions" : "applied actions"}:</span>
                {actionCounts.length === 0 ? (
                  <span className="text-text-tertiary">no changes</span>
                ) : (
                  actionCounts.map(([k, v]) => (
                    <span key={k} className="font-mono-ui whitespace-nowrap">
                      {countLabel(k)}={v}
                    </span>
                  ))
                )}
                {diagnosticCounts.length > 0 ? (
                  <>
                    <span className="text-text-tertiary">· signals</span>
                    {diagnosticCounts.map(([k, v]) => (
                      <span key={k} className="font-mono-ui whitespace-nowrap text-text-tertiary">
                        {countLabel(k)}={v}
                      </span>
                    ))}
                  </>
                ) : null}
                <span className="text-text-tertiary whitespace-nowrap">· llm_calls={report.llm_calls}</span>
                {report.coverage ? (
                  <span className="text-text-tertiary whitespace-nowrap">
                    · scanned={report.coverage.scanned}/{report.coverage.active_total}
                    {report.coverage.due_remaining
                      ? ` · due=${report.coverage.due_remaining}`
                      : ""}
                  </span>
                ) : null}
                {report.coverage?.entity_total != null ? (
                  <span className="text-text-tertiary whitespace-nowrap">
                    · entities={report.coverage.entities_scanned ?? 0}/{report.coverage.entity_total}
                    {report.coverage.entity_scan_remaining
                      ? ` · entity_due=${report.coverage.entity_scan_remaining}`
                      : ""}
                  </span>
                ) : null}
                {isPlan && previewSavedAt ? (
                  <span className="text-text-tertiary whitespace-nowrap">
                    · saved={formatHistoryTime(previewSavedAt)}
                  </span>
                ) : null}
                {!isPlan && report.skipped_actions ? (
                  <span className="text-warning whitespace-nowrap">· skipped={report.skipped_actions}</span>
                ) : null}
              </div>
            )}

            {report?.apply_errors?.length ? (
              <div className="border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning shrink-0">
                {report.apply_errors.length} action(s) failed to apply.
              </div>
            ) : null}

            {!report && !loading && (
              <p className="text-xs text-text-tertiary shrink-0">
                Click <span className="text-text-secondary">Preview</span> to see proposed maintenance actions.
              </p>
            )}

            {actions.length > 0 ? (
              <div className="flex flex-1 min-h-0 flex-col gap-2 overflow-y-auto overflow-x-hidden pr-1">
                {nonEmptyActionGroups.map((group, i) => (
                  <ActionReviewGroup
                    key={group.key}
                    group={group}
                    defaultOpen={i === 0}
                  />
                ))}
              </div>
            ) : null}
          </div>
        ) : null}

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
                  Live phases from preview and apply runs.
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
            {error ? (
              <div className="border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive shrink-0">
                {error}
              </div>
            ) : null}
            <ActivityScroller
              events={activity}
              loading={loading || applying || activityLoading}
              error={activityError}
              scrollRef={activityRef}
            />
          </div>
        ) : null}

        {activeTab === "history" ? (
          <CurationHistoryPanel
            report={report}
            previewSavedAt={previewSavedAt}
            previewStale={previewStale}
            previewStaleReason={previewStaleReason}
            actionsLength={actions.length}
            actionCounts={actionCounts}
            diagnosticCounts={diagnosticCounts}
            isPlan={isPlan}
            status={status}
            statusLoading={statusLoading}
            statusError={statusError}
            oplog={oplog}
            oplogError={oplogError}
            automationRuns={automationRuns}
            automationRunsError={automationRunsError}
            automationRunActioning={automationRunActioning}
            automationRunError={automationRunError}
            automationRunArtifacts={automationRunArtifacts}
            automationRunArtifact={automationRunArtifact}
            automationRunArtifactLoading={automationRunArtifactLoading}
            automationRunArtifactError={automationRunArtifactError}
            factProposals={factProposals}
            factProposalsLoading={factProposalsLoading}
            factProposalsError={factProposalsError}
            factProposalActioning={factProposalActioning}
            managedSkills={managedSkills}
            selectedManagedSkillId={selectedManagedSkillId}
            selectedManagedSkill={selectedManagedSkill}
            selectedUsage={selectedUsage}
            selectedRecommendation={selectedRecommendation}
            selectedImprovementRecommendation={selectedImprovementRecommendation}
            managedSkillsLoading={managedSkillsLoading}
            managedSkillsError={managedSkillsError}
            managedSkillActioning={managedSkillActioning}
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
            loadOplog={loadOplog}
            loadAutomationRuns={loadAutomationRuns}
            loadSchedulerStatus={loadSchedulerStatus}
            loadAutomationRunArtifact={loadAutomationRunArtifact}
            loadFactProposals={loadFactProposals}
            loadManagedSkills={loadManagedSkills}
            loadManagedSkill={loadManagedSkill}
            runAutomationTask={runAutomationTask}
            runFactProposalAction={runFactProposalAction}
            runManagedSkillAction={runManagedSkillAction}
            setSchedulerPaused={setSchedulerPaused}
            updateConfigDraft={updateConfigDraft}
            updateConfigTaskDraft={updateConfigTaskDraft}
            updateTaskSeconds={updateTaskSeconds}
            resetConfigDraft={resetConfigDraft}
            resetConfigToDefaults={resetConfigToDefaults}
            saveConfigDraft={saveConfigDraft}
          />
        ) : null}
      </CardContent>

      <InlineConfirm
        open={confirmOpen}
        title="Apply memory curation?"
        description="Apply runs a fresh curation pass first, then applies the recomputed plan. Flagged duplicate facts are permanently deleted — this cannot be undone."
        confirmLabel="Apply"
        loading={applying}
        onCancel={() => setConfirmOpen(false)}
        onConfirm={apply}
      >
        <div className="flex flex-col gap-2 text-xs">
          <div className="font-medium text-foreground">Preview summary</div>
          {confirmGroupCounts.length === 0 ? (
            <div className="text-text-tertiary">No previewed actions.</div>
          ) : (
            <div className="grid grid-cols-2 gap-x-3 gap-y-1">
              {confirmGroupCounts.map(([label, count]) => (
                <div key={label} className="flex items-center justify-between gap-2">
                  <span className="text-text-tertiary">{label}</span>
                  <span className="font-mono-ui text-text-secondary">{count}</span>
                </div>
              ))}
            </div>
          )}
          <div className="text-warning">
            Deleted facts are removed permanently and cannot be restored.
          </div>
        </div>
      </InlineConfirm>
    </Card>
  );
}
