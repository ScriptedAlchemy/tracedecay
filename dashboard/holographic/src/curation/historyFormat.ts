import { formatHistoryTime } from "./format";
import type {
  FactProposalRecord,
  ManagedSkill,
  MemoryAutomationRunArtifactPayloadResponse,
  MemoryAutomationRunRecord,
  MemoryOplogEvent,
} from "../types";

export function automationRunStatusClass(status: string): string {
  switch (status) {
    case "queued":
      return "border-primary/30 bg-primary/10 text-primary";
    case "running":
      return "border-accent/30 bg-accent/10 text-accent";
    case "succeeded":
      return "border-success/30 bg-success/10 text-success";
    case "failed":
      return "border-destructive/30 bg-destructive/10 text-destructive";
    case "skipped":
      return "border-warning/30 bg-warning/10 text-warning";
    default:
      return "border-border bg-muted/30 text-text-tertiary";
  }
}

export function oplogDetailSummary(event: MemoryOplogEvent): string {
  const detail = event.detail ?? {};
  const parts = Object.entries(detail)
    .filter(([, value]) => value !== null && value !== undefined && value !== "")
    .slice(0, 4)
    .map(([key, value]) => `${key}=${String(value)}`);
  return parts.join(" · ");
}

export function automationRunSummary(record: MemoryAutomationRunRecord): string {
  const backend = record.model ? `${record.backend}/${record.model}` : record.backend;
  const host = record.host_mode ? ` · ${record.host_mode}` : "";
  const counts = `accepted=${record.accepted_count} rejected=${record.rejected_count}`;
  const artifacts = record.artifacts?.length ? ` · artifacts=${record.artifacts.length}` : "";
  const fallback = record.fallback_status ? ` · ${record.fallback_status}` : "";
  const suffix = record.error && record.error !== record.fallback_status ? ` · ${record.error}` : "";
  return `${record.task} · ${record.trigger} · ${backend}${host} · ${counts}${artifacts}${fallback}${suffix}`;
}

export function automationArtifactPreview(
  artifact: MemoryAutomationRunArtifactPayloadResponse | null,
): string {
  if (!artifact) return "";
  return JSON.stringify(artifact.payload, null, 2);
}

export function factProposalSummary(proposal: FactProposalRecord): string {
  const request = proposal.add_fact_request;
  if (request?.content) return request.content;
  if (proposal.validation_reason) return proposal.validation_reason;
  return proposal.proposal_id;
}

export function factProposalDetail(proposal: FactProposalRecord): string {
  const tags = proposal.add_fact_request?.tags;
  const tagsText = Array.isArray(tags) && tags.length ? ` · tags=${tags.join(",")}` : "";
  const category = proposal.add_fact_request?.category
    ? ` · category=${proposal.add_fact_request.category}`
    : "";
  const factId = proposal.applied_fact_id ? ` · fact=${proposal.applied_fact_id}` : "";
  return `${proposal.run_id}${category}${tagsText}${factId}`;
}

export function managedSkillStateClass(state: string): string {
  switch (state) {
    case "active":
      return "border-success/30 bg-success/10 text-success";
    case "pending_approval":
      return "border-warning/30 bg-warning/10 text-warning";
    case "disabled":
      return "border-muted-foreground/30 bg-muted/40 text-text-tertiary";
    case "archived":
      return "border-border bg-background/50 text-text-tertiary";
    default:
      return "border-border bg-muted/30 text-text-tertiary";
  }
}

export function managedSkillSummary(skill: ManagedSkill): string {
  const source = skill.metadata.provenance?.source || "unknown";
  const actor = skill.metadata.provenance?.actor || "unknown";
  const runId = skill.metadata.provenance?.run_id
    ? ` · ${skill.metadata.provenance.run_id}`
    : "";
  const pinned = skill.metadata.pinned ? " · pinned" : "";
  return `${skill.metadata.category} · ${source}/${actor}${runId}${pinned}`;
}

export function formatUnixTime(ts?: number | null): string {
  if (!ts) return "never";
  return formatHistoryTime(new Date(ts * 1000).toISOString()) || String(ts);
}
