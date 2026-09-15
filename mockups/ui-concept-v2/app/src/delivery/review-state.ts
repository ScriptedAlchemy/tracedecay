export type ReviewMode = "story" | "code" | "evidence" | "feedback";
export type FeedbackLifecycle = "open" | "acknowledged" | "acted-upon" | "contradicted";

export type LocalFeedback = {
  id: string;
  kind: "comment" | "challenge";
  body: string;
  anchor: string;
  revision: string;
  lifecycle: FeedbackLifecycle;
  sourceRef?: string;
};

const FEEDBACK_KINDS = new Set<LocalFeedback["kind"]>(["comment", "challenge"]);
const FEEDBACK_LIFECYCLES = new Set<FeedbackLifecycle>(["open", "acknowledged", "acted-upon", "contradicted"]);

export function isLocalFeedback(value: unknown): value is LocalFeedback {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const item = value as Record<string, unknown>;
  return typeof item.id === "string" && item.id.length > 0
    && FEEDBACK_KINDS.has(item.kind as LocalFeedback["kind"])
    && typeof item.body === "string"
    && typeof item.anchor === "string" && item.anchor.length > 0
    && typeof item.revision === "string" && item.revision.length > 0
    && FEEDBACK_LIFECYCLES.has(item.lifecycle as FeedbackLifecycle)
    && (item.sourceRef === undefined || typeof item.sourceRef === "string");
}

export const REVIEW_707_REVISION = {
  base: "a1b2c3d",
  reviewed: "c3b2a1d",
  head: "d4e56a",
} as const;
