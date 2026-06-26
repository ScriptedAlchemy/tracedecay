export type AutomationRunTask = "memory_curator" | "session_reflector" | "skill_writer";

export type AutomationRunApiMethod =
  | "postAutomationRunMemoryCurator"
  | "postAutomationRunSessionReflection"
  | "postAutomationRunSkillWriting";

export type AutomationRunRefreshTarget =
  | "memory_preview"
  | "fact_proposals"
  | "managed_skills";

export interface AutomationTaskDescriptor {
  id: AutomationRunTask;
  runMethod: AutomationRunApiMethod;
  refreshTarget: AutomationRunRefreshTarget;
  enabledLabel: string;
  scheduleLabel: string;
  runAriaLabel: string;
}

export const AUTOMATION_TASKS = [
  {
    id: "memory_curator",
    runMethod: "postAutomationRunMemoryCurator",
    refreshTarget: "memory_preview",
    enabledLabel: "Run memory curator",
    scheduleLabel: "Memory curator schedule",
    runAriaLabel: "Memory curator",
  },
  {
    id: "session_reflector",
    runMethod: "postAutomationRunSessionReflection",
    refreshTarget: "fact_proposals",
    enabledLabel: "Run session reflector",
    scheduleLabel: "Session reflector schedule",
    runAriaLabel: "Session reflector",
  },
  {
    id: "skill_writer",
    runMethod: "postAutomationRunSkillWriting",
    refreshTarget: "managed_skills",
    enabledLabel: "Run skill writer",
    scheduleLabel: "Skill writer schedule",
    runAriaLabel: "Skill writer",
  },
] satisfies AutomationTaskDescriptor[];

export const AUTOMATION_TASK_BY_ID = Object.fromEntries(
  AUTOMATION_TASKS.map((descriptor) => [descriptor.id, descriptor]),
) as Record<AutomationRunTask, AutomationTaskDescriptor>;

export function isActiveAutomationStatus(status?: string | null): boolean {
  return status === "queued" || status === "running";
}

export function schedulerTaskLabel(task: string): string {
  return AUTOMATION_TASK_BY_ID[task as AutomationRunTask]?.runAriaLabel.toLowerCase() ?? task;
}
