import type { MemoryCurateAction } from "../types";

export type ActionRisk = "low" | "medium" | "high" | "review";

export interface ActionGroupDef {
  key: string;
  label: string;
  description: string;
  ops: Set<string>;
}

export interface ActionGroup extends ActionGroupDef {
  actions: MemoryCurateAction[];
}

const ACTION_GROUPS: ActionGroupDef[] = [
  {
    key: "fact_cleanup",
    label: "Fact cleanup",
    description: "Delete or merge stale and duplicate facts.",
    ops: new Set(["delete", "merge"]),
  },
  {
    key: "entity_cleanup",
    label: "Entity cleanup",
    description: "Classify, merge, or prune entity records.",
    ops: new Set(["entity_classify", "entity_merge", "entity_prune"]),
  },
  {
    key: "organization",
    label: "Organization",
    description: "Retag and recategorize facts without changing fact content.",
    ops: new Set(["retag", "recategorize"]),
  },
  {
    key: "reflections",
    label: "Reflections",
    description: "Create durable summary facts from related memories.",
    ops: new Set(["reflect"]),
  },
  {
    key: "other",
    label: "Other",
    description: "Actions that need extra review because their operation is unfamiliar.",
    ops: new Set(),
  },
];

export function actionRisk(op: string): ActionRisk {
  if (op === "retag" || op === "entity_prune" || op === "entity_classify") return "low";
  if (op === "entity_merge" || op === "recategorize") {
    return "medium";
  }
  if (op === "delete" || op === "merge" || op === "reflect") {
    return "high";
  }
  return "review";
}

export function riskClass(risk: ActionRisk): string {
  switch (risk) {
    case "low":
      return "border-success/30 bg-success/10 text-success";
    case "medium":
      return "border-warning/30 bg-warning/10 text-warning";
    case "high":
      return "border-destructive/30 bg-destructive/10 text-destructive";
    default:
      return "border-border bg-secondary/50 text-text-tertiary";
  }
}

export function groupActions(actions: MemoryCurateAction[]): ActionGroup[] {
  return ACTION_GROUPS.map((group) => ({
    ...group,
    actions:
      group.key === "other"
        ? actions.filter((action) => !ACTION_GROUPS.some((g) => g.key !== "other" && g.ops.has(action.op)))
        : actions.filter((action) => group.ops.has(action.op)),
  }));
}
