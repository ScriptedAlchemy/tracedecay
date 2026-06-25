import { useCallback, useState } from "react";

import type { api as defaultApi } from "../api";
import type {
  AutomationSchedulerStatusResponse,
  AutomationTaskConfig,
  AutomationTaskSet,
  MemoryAutomationConfig,
  MemoryAutomationConfigPatch,
  MemoryAutomationConfigResponse,
} from "../types";
import { AUTOMATION_TASKS } from "./automationTasks";
import { errorMessage } from "./errors";
import type { ConfigFieldErrors } from "./configTypes";

type AutomationTaskKey = keyof AutomationTaskSet;
type AutomationConfigApi = Pick<
  typeof defaultApi,
  | "getAutomationSchedulerStatus"
  | "getMemoryAutomationConfig"
  | "patchMemoryAutomationConfig"
  | "pauseAutomationScheduler"
  | "resetMemoryAutomationConfig"
  | "resumeAutomationScheduler"
>;

interface ConfigValidationError {
  field?: unknown;
  message?: unknown;
}

interface ErrorWithBody {
  body?: {
    validation_errors?: ConfigValidationError[];
  };
}

function cloneTaskSet(tasks: AutomationTaskSet): AutomationTaskSet {
  const cloned: Partial<AutomationTaskSet> = {};
  for (const { id } of AUTOMATION_TASKS) {
    cloned[id] = { ...tasks[id] };
  }
  return cloned as AutomationTaskSet;
}

function cloneConfig(config: MemoryAutomationConfig): MemoryAutomationConfig {
  return {
    ...config,
    tasks: cloneTaskSet(config.tasks),
  };
}

function configToPatch(config: MemoryAutomationConfig): MemoryAutomationConfigPatch {
  const patch: MemoryAutomationConfigPatch = {
    enabled: config.enabled,
    host_mode: config.host_mode,
    model: config.model || null,
    timeout_secs: config.timeout_secs,
    scheduler_tick_secs: config.scheduler_tick_secs,
    max_tokens: config.max_tokens ?? null,
    temperature: config.temperature ?? null,
    require_dashboard_approval: config.require_dashboard_approval,
    auto_apply_memory_ops: config.auto_apply_memory_ops,
    auto_enable_skills: config.auto_enable_skills,
    ...cloneTaskSet(config.tasks),
  };
  if (config.backend !== "external_command") {
    patch.backend = config.backend;
  }
  return patch;
}

function sameConfig(a: MemoryAutomationConfig | null, b: MemoryAutomationConfig | null) {
  return JSON.stringify(a) === JSON.stringify(b);
}

function configFieldErrorsFromError(err: unknown): ConfigFieldErrors {
  const errors = (err as ErrorWithBody)?.body?.validation_errors;
  if (!Array.isArray(errors)) return {};
  return Object.fromEntries(
    errors
      .filter((error) => typeof error.field === "string" && typeof error.message === "string")
      .map((error) => [error.field as string, error.message as string]),
  );
}

export function useAutomationConfig(api: AutomationConfigApi) {
  const [configResponse, setConfigResponse] = useState<MemoryAutomationConfigResponse | null>(null);
  const [configDraft, setConfigDraft] = useState<MemoryAutomationConfig | null>(null);
  const [savedConfig, setSavedConfig] = useState<MemoryAutomationConfig | null>(null);
  const [configLoading, setConfigLoading] = useState(false);
  const [configSaving, setConfigSaving] = useState(false);
  const [configResetting, setConfigResetting] = useState(false);
  const [configError, setConfigError] = useState("");
  const [configFieldErrors, setConfigFieldErrors] = useState<ConfigFieldErrors>({});
  const [schedulerStatus, setSchedulerStatus] =
    useState<AutomationSchedulerStatusResponse | null>(null);
  const [schedulerStatusLoading, setSchedulerStatusLoading] = useState(false);
  const [schedulerStatusError, setSchedulerStatusError] = useState("");
  const [schedulerActioning, setSchedulerActioning] = useState<"pause" | "resume" | null>(null);
  const configDirty = !sameConfig(configDraft, savedConfig);

  const applyConfigResponse = useCallback((response: MemoryAutomationConfigResponse) => {
    const effective = cloneConfig(response.effective);
    setConfigResponse(response);
    setConfigDraft(effective);
    setSavedConfig(cloneConfig(response.effective));
    setConfigFieldErrors({});
  }, []);

  const loadConfig = useCallback(() => {
    setConfigLoading(true);
    setConfigError("");
    setConfigFieldErrors({});
    return api
      .getMemoryAutomationConfig()
      .then((response) => {
        applyConfigResponse(response);
        return response;
      })
      .catch((err) => {
        setConfigError(errorMessage(err));
        throw err;
      })
      .finally(() => setConfigLoading(false));
  }, [api, applyConfigResponse]);

  const loadSchedulerStatus = useCallback((showSpinner = false) => {
    if (showSpinner) setSchedulerStatusLoading(true);
    setSchedulerStatusError("");
    return api
      .getAutomationSchedulerStatus()
      .then((response) => {
        setSchedulerStatus(response);
        return response;
      })
      .catch((err) => {
        setSchedulerStatusError(errorMessage(err));
        throw err;
      })
      .finally(() => {
        if (showSpinner) setSchedulerStatusLoading(false);
      });
  }, [api]);

  const setSchedulerPaused = useCallback(async (paused: boolean) => {
    const action = paused ? "pause" : "resume";
    setSchedulerActioning(action);
    setSchedulerStatusError("");
    try {
      const response = paused
        ? await api.pauseAutomationScheduler()
        : await api.resumeAutomationScheduler();
      setSchedulerStatus(response);
      await loadConfig();
      return response;
    } catch (err) {
      setSchedulerStatusError(errorMessage(err));
      throw err;
    } finally {
      setSchedulerActioning(null);
    }
  }, [api, loadConfig]);

  const updateConfigDraft = useCallback((patch: Partial<MemoryAutomationConfig>) => {
    setConfigDraft((current) => (current ? { ...current, ...patch } : current));
  }, []);

  const updateConfigTaskDraft = useCallback((
    task: AutomationTaskKey,
    patch: Partial<AutomationTaskConfig>,
  ) => {
    setConfigDraft((current) => {
      if (!current) return current;
      return {
        ...current,
        tasks: {
          ...current.tasks,
          [task]: {
            ...current.tasks[task],
            ...patch,
          },
        },
      };
    });
  }, []);

  const resetConfigDraft = useCallback(() => {
    setConfigDraft(savedConfig ? cloneConfig(savedConfig) : null);
    setConfigError("");
    setConfigFieldErrors({});
  }, [savedConfig]);

  const saveConfigDraft = useCallback(async () => {
    if (!configDraft) return null;
    setConfigSaving(true);
    setConfigError("");
    setConfigFieldErrors({});
    try {
      const response = await api.patchMemoryAutomationConfig(configToPatch(configDraft));
      applyConfigResponse(response);
      return response;
    } catch (err) {
      setConfigError(errorMessage(err));
      setConfigFieldErrors(configFieldErrorsFromError(err));
      throw err;
    } finally {
      setConfigSaving(false);
    }
  }, [api, applyConfigResponse, configDraft]);

  const resetConfigToDefaults = useCallback(async () => {
    setConfigResetting(true);
    setConfigError("");
    setConfigFieldErrors({});
    try {
      const response = await api.resetMemoryAutomationConfig();
      applyConfigResponse(response);
      return response;
    } catch (err) {
      setConfigError(errorMessage(err));
      setConfigFieldErrors(configFieldErrorsFromError(err));
      throw err;
    } finally {
      setConfigResetting(false);
    }
  }, [api, applyConfigResponse]);

  return {
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
  };
}
