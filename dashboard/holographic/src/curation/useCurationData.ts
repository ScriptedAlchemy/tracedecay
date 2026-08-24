import { useCallback, useEffect, useRef, useState } from "react";
import { api as defaultApi } from "../api";
import type {
  MemoryCuratorActivityEvent,
  MemoryCuratorStatusResponse,
  MemoryOplogEvent,
} from "../types";
import type { AutomationRunApiMethod } from "./automationTasks";
import { errorMessage } from "./errors";
import { useAutomationConfig } from "./useAutomationConfig";
import { useAutomationRuns } from "./useAutomationRuns";
import { useFactProposals } from "./useFactProposals";
import { useManagedSkills } from "./useManagedSkills";

export type CurationTab =
  "automation" | "proposals" | "history" | "activity";

export type CurationApi = Pick<
  typeof defaultApi,
  | "applyFactProposal"
  | "approveManagedSkill"
  | "archiveManagedSkill"
  | "disableManagedSkill"
  | "discardManagedSkillUpdate"
  | "getAutomationSchedulerStatus"
  | "getFactProposals"
  | "getManagedSkill"
  | "getManagedSkills"
  | "getMemoryAutomationConfig"
  | "getMemoryAutomationRunArtifact"
  | "getMemoryAutomationRunArtifacts"
  | "getMemoryAutomationRuns"
  | "getMemoryCuratorActivity"
  | "getMemoryCuratorStatus"
  | "getMemoryOplog"
  | "patchMemoryAutomationConfig"
  | "pauseAutomationScheduler"
  | AutomationRunApiMethod
  | "rejectFactProposal"
  | "resetMemoryAutomationConfig"
  | "restoreManagedSkill"
  | "resumeAutomationScheduler"
>;

export function useCurationData({
  api = defaultApi,
  onApplied,
  pollFastMs = 900,
  pollIdleMs = 2500,
}: {
  api?: CurationApi;
  onApplied?: () => void;
  pollFastMs?: number;
  pollIdleMs?: number;
}) {
  const [activeTab, setActiveTab] = useState<CurationTab>("automation");
  const [status, setStatus] = useState<MemoryCuratorStatusResponse | null>(
    null,
  );
  const [statusLoading, setStatusLoading] = useState(false);
  const [statusError, setStatusError] = useState("");
  const [oplog, setOplog] = useState<MemoryOplogEvent[]>([]);
  const [oplogError, setOplogError] = useState("");
  const [activity, setActivity] = useState<MemoryCuratorActivityEvent[]>([]);
  const [activityLoading, setActivityLoading] = useState(false);
  const [activityError, setActivityError] = useState("");
  const {
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
    loadConfig,
    loadSchedulerStatus,
    setSchedulerPaused,
    updateConfigDraft,
    updateConfigTaskDraft,
    resetConfigDraft,
    resetConfigToDefaults,
    saveConfigDraft,
  } = useAutomationConfig(api);
  const {
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
    loadManagedSkills,
    loadManagedSkill,
    runManagedSkillAction,
  } = useManagedSkills(api);
  const {
    factProposals,
    factProposalsLoading,
    factProposalsError,
    factProposalActioning,
    loadFactProposals,
    runFactProposalAction,
  } = useFactProposals({ api, onApplied });
  const activityRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const loadActivity = useCallback(
    (showSpinner = false) => {
      if (showSpinner) setActivityLoading(true);
      setActivityError("");
      api
        .getMemoryCuratorActivity({ limit: 120 })
        .then((response) => {
          setActivity(response.events || []);
        })
        .catch((err) => setActivityError(errorMessage(err)))
        .finally(() => {
          if (showSpinner) setActivityLoading(false);
        });
    },
    [api],
  );

  const loadStatus = useCallback(() => {
    setStatusLoading(true);
    setStatusError("");
    api
      .getMemoryCuratorStatus()
      .then((response) => setStatus(response))
      .catch((err) => setStatusError(errorMessage(err)))
      .finally(() => setStatusLoading(false));
  }, [api]);

  const loadOplog = useCallback(() => {
    setOplogError("");
    api
      .getMemoryOplog({ limit: 30 })
      .then((response) => {
        setOplog(response.events || []);
        if (response.error) setOplogError(response.error);
      })
      .catch((err) => setOplogError(errorMessage(err)));
  }, [api]);

  const {
    automationRuns,
    automationRunsError,
    automationRunActioning,
    automationRunError,
    automationRunArtifacts,
    automationRunArtifact,
    automationRunArtifactLoading,
    automationRunArtifactError,
    loadAutomationRuns,
    loadAutomationRunArtifact,
    runAutomationTask,
  } = useAutomationRuns({
    api,
    pollFastMs,
    setActiveTab,
    loadActivity,
    loadStatus,
    loadFactProposals,
    loadManagedSkills,
  });

  useEffect(() => {
    loadConfig().catch(() => {});
  }, [loadConfig]);

  useEffect(() => {
    loadSchedulerStatus(false).catch(() => {});
  }, [loadSchedulerStatus]);

  useEffect(() => {
    if (
      (activeTab === "history" || activeTab === "automation") &&
      !status &&
      !statusLoading
    ) {
      loadStatus();
    }
  }, [activeTab, loadStatus, status, statusLoading]);

  // The loaders are recreated whenever unrelated state they close over
  // changes (e.g. selecting a skill), which would re-fire tab effects keyed
  // on them and refetch mid-tab. Reading them through a ref keys the tab
  // effects on tab activation alone.
  const tabLoaders = useRef({
    loadOplog,
    loadAutomationRuns,
    loadSchedulerStatus,
    loadFactProposals,
    loadManagedSkills,
  });
  tabLoaders.current = {
    loadOplog,
    loadAutomationRuns,
    loadSchedulerStatus,
    loadFactProposals,
    loadManagedSkills,
  };

  useEffect(() => {
    if (activeTab === "history") {
      tabLoaders.current.loadOplog();
    }
  }, [activeTab]);

  useEffect(() => {
    if (activeTab === "automation") {
      tabLoaders.current.loadAutomationRuns();
      tabLoaders.current.loadSchedulerStatus(false).catch(() => {});
    }
  }, [activeTab]);

  useEffect(() => {
    if (activeTab === "proposals") {
      tabLoaders.current.loadFactProposals();
      tabLoaders.current.loadManagedSkills();
    }
  }, [activeTab]);

  useEffect(() => {
    if (activeTab === "activity" && activity.length === 0) {
      loadActivity(true);
    }
  }, [activeTab, activity.length, loadActivity]);

  useEffect(() => {
    if (activeTab !== "activity") return undefined;
    const interval = window.setInterval(
      () => {
        if (panelRef.current?.offsetParent === null) return;
        loadActivity(false);
      },
      pollIdleMs,
    );
    return () => window.clearInterval(interval);
  }, [activeTab, loadActivity, pollIdleMs]);

  useEffect(() => {
    const element = activityRef.current;
    if (!element) return;
    element.scrollTop = element.scrollHeight;
  }, [activity]);

  return {
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
    runAutomationTask,
    loadActivity,
    loadStatus,
    loadOplog,
    loadAutomationRuns,
    loadAutomationRunArtifact,
    loadFactProposals,
    runFactProposalAction,
    loadManagedSkills,
    loadManagedSkill,
    runManagedSkillAction,
    loadConfig,
    loadSchedulerStatus,
    setSchedulerPaused,
    updateConfigDraft,
    updateConfigTaskDraft,
    resetConfigDraft,
    resetConfigToDefaults,
    saveConfigDraft,
  };
}
