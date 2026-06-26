import { useCallback, useEffect, useRef, useState } from "react";

import type { api as defaultApi } from "../api";
import type {
  MemoryAutomationRunArtifactPayloadResponse,
  MemoryAutomationRunArtifactsResponse,
  MemoryAutomationRunRecord,
  MemoryAutomationRunResponse,
  MemoryCurateResponse,
} from "../types";
import {
  AUTOMATION_TASK_BY_ID,
  isActiveAutomationStatus,
  type AutomationRunApiMethod,
  type AutomationRunTask,
} from "./automationTasks";
import { errorMessage } from "./errors";

type AutomationRunsApi = Pick<
  typeof defaultApi,
  | "getMemoryAutomationRunArtifact"
  | "getMemoryAutomationRunArtifacts"
  | "getMemoryAutomationRuns"
  | AutomationRunApiMethod
>;

function upsertAutomationRun(
  records: MemoryAutomationRunRecord[],
  record: MemoryAutomationRunRecord,
): MemoryAutomationRunRecord[] {
  return [record, ...records.filter((existing) => existing.run_id !== record.run_id)];
}

export function useAutomationRuns({
  api,
  pollFastMs,
  setActiveTab,
  setMemoryPreviewFromRun,
  loadMemoryPreview,
  loadActivity,
  loadStatus,
  loadFactProposals,
  loadManagedSkills,
}: {
  api: AutomationRunsApi;
  pollFastMs: number;
  setActiveTab: (tab: "history") => void;
  setMemoryPreviewFromRun: (report: MemoryCurateResponse) => void;
  loadMemoryPreview: (force?: boolean) => Promise<unknown>;
  loadActivity: (showSpinner?: boolean) => void;
  loadStatus: () => void;
  loadFactProposals: (showSpinner?: boolean) => Promise<unknown>;
  loadManagedSkills: (showSpinner?: boolean) => Promise<unknown>;
}) {
  const [automationRuns, setAutomationRuns] = useState<MemoryAutomationRunRecord[]>([]);
  const automationRunsRef = useRef<MemoryAutomationRunRecord[]>([]);
  const [automationRunsError, setAutomationRunsError] = useState("");
  const [automationRunActioning, setAutomationRunActioning] =
    useState<AutomationRunTask | null>(null);
  const [automationRunError, setAutomationRunError] = useState("");
  const [automationRunArtifact, setAutomationRunArtifact] =
    useState<MemoryAutomationRunArtifactPayloadResponse | null>(null);
  const [automationRunArtifacts, setAutomationRunArtifacts] =
    useState<MemoryAutomationRunArtifactsResponse | null>(null);
  const [automationRunArtifactLoading, setAutomationRunArtifactLoading] = useState<string | null>(
    null,
  );
  const [automationRunArtifactError, setAutomationRunArtifactError] = useState("");

  const refreshCompletedRuns = useCallback(async (
    completedRuns: MemoryAutomationRunRecord[],
  ) => {
    let refreshMemoryPreview = false;
    let refreshFactProposals = false;
    let refreshManagedSkills = false;

    for (const run of completedRuns) {
      const descriptor = AUTOMATION_TASK_BY_ID[run.task as AutomationRunTask];
      if (!descriptor) continue;
      if (descriptor.refreshTarget === "memory_preview") refreshMemoryPreview = true;
      if (descriptor.refreshTarget === "fact_proposals") refreshFactProposals = true;
      if (descriptor.refreshTarget === "managed_skills") refreshManagedSkills = true;
    }

    const refreshes: Promise<unknown>[] = [];
    if (refreshMemoryPreview) {
      refreshes.push(loadMemoryPreview(true));
      loadActivity(false);
      loadStatus();
    }
    if (refreshFactProposals) refreshes.push(loadFactProposals(false));
    if (refreshManagedSkills) refreshes.push(loadManagedSkills(false));
    await Promise.all(refreshes);
  }, [
    loadActivity,
    loadFactProposals,
    loadManagedSkills,
    loadMemoryPreview,
    loadStatus,
  ]);

  const loadAutomationRuns = useCallback(() => {
    setAutomationRunsError("");
    return api
      .getMemoryAutomationRuns({ limit: 20 })
      .then(async (response) => {
        const nextRuns = response.records || [];
        const previousById = new Map<string, MemoryAutomationRunRecord>(
          automationRunsRef.current.map((run) => [run.run_id, run]),
        );
        const completedRuns = nextRuns.filter((run) => {
          const previous = previousById.get(run.run_id);
          return (
            previous &&
            isActiveAutomationStatus(previous.status) &&
            !isActiveAutomationStatus(run.status)
          );
        });
        automationRunsRef.current = nextRuns;
        setAutomationRuns(nextRuns);
        if (response.error) setAutomationRunsError(response.error);
        if (completedRuns.length > 0) await refreshCompletedRuns(completedRuns);
        return response;
      })
      .catch((err) => setAutomationRunsError(errorMessage(err)));
  }, [api, refreshCompletedRuns]);

  const loadAutomationRunArtifact = useCallback((runId: string, kind: string) => {
    const key = `${runId}:${kind}`;
    setAutomationRunArtifactLoading(key);
    setAutomationRunArtifactError("");
    return Promise.all([
      api.getMemoryAutomationRunArtifacts(runId),
      api.getMemoryAutomationRunArtifact(runId, kind),
    ])
      .then(([artifactsResponse, payloadResponse]) => {
        setAutomationRunArtifacts(artifactsResponse);
        setAutomationRunArtifact(payloadResponse);
        if (artifactsResponse.error || payloadResponse.error) {
          setAutomationRunArtifactError(artifactsResponse.error || payloadResponse.error || "");
        }
        return payloadResponse;
      })
      .catch((err) => {
        setAutomationRunArtifactError(errorMessage(err));
        throw err;
      })
      .finally(() => setAutomationRunArtifactLoading(null));
  }, [api]);

  const runAutomationTask = useCallback(async (task: AutomationRunTask) => {
    setAutomationRunActioning(task);
    setAutomationRunError("");
      setActiveTab("history");
    try {
      const descriptor = AUTOMATION_TASK_BY_ID[task];
      const response = await api[descriptor.runMethod]({ dry_run: true });
      const ledgerRecord = response.ledger_record;
      if (ledgerRecord) {
        setAutomationRuns((records) => {
          const nextRuns = upsertAutomationRun(records, ledgerRecord);
          automationRunsRef.current = nextRuns;
          return nextRuns;
        });
      }
      await loadAutomationRuns();
      if (
        descriptor.refreshTarget === "memory_preview" &&
        response.report &&
        !isActiveAutomationStatus(response.status)
      ) {
        const report = (response as MemoryAutomationRunResponse<MemoryCurateResponse>).report;
        if (report) setMemoryPreviewFromRun(report);
        loadActivity(false);
        loadStatus();
      } else if (descriptor.refreshTarget === "fact_proposals") {
        await loadFactProposals(false);
      } else if (descriptor.refreshTarget === "managed_skills") {
        await loadManagedSkills(false);
      }
      return response;
    } catch (err) {
      setAutomationRunError(errorMessage(err));
      throw err;
    } finally {
      setAutomationRunActioning(null);
    }
  }, [
    api,
    loadActivity,
    loadAutomationRuns,
    loadFactProposals,
    loadManagedSkills,
    loadStatus,
    setActiveTab,
    setMemoryPreviewFromRun,
  ]);

  useEffect(() => {
    if (!automationRuns.some((record) => isActiveAutomationStatus(record.status))) return;
    const timer = setTimeout(() => {
      void loadAutomationRuns();
    }, pollFastMs);
    return () => clearTimeout(timer);
  }, [automationRuns, loadAutomationRuns, pollFastMs]);

  return {
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
  };
}
