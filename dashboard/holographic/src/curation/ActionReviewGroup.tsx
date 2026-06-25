import { type Key, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { Badge } from "../sdk";
import { describe, diffTags, isBookkeepingTag } from "./format";
import {
  actionRisk,
  riskClass,
  type ActionGroupDef,
  type ActionRisk,
} from "./risk";
import type { MemoryCurateAction } from "../types";

const RISK_ORDER: ActionRisk[] = ["high", "medium", "low", "review"];

function TagBucket({
  label,
  tags,
  tone,
}: {
  label: string;
  tags: string[];
  tone: "neutral" | "removed" | "added";
}) {
  if (!tags.length) return null;
  const chipClass =
    tone === "removed"
      ? "border-destructive/30 bg-destructive/10 text-destructive/90 line-through"
      : tone === "added"
        ? "border-success/30 bg-success/10 text-success"
        : "border-border bg-secondary/60 text-text-secondary";
  return (
    <div className="flex flex-col gap-1 min-w-0">
      <div className="text-[10px] uppercase tracking-[0.08em] text-text-tertiary">
        {label}
      </div>
      <div className="flex flex-wrap gap-1.5">
        {tags.map((tag) => (
          <span
            key={`${label}-${tag}`}
            className={`rounded-sm border px-1.5 py-0.5 font-mono-ui text-[10px] leading-4 break-all ${chipClass}`}
          >
            {tag}
          </span>
        ))}
      </div>
    </div>
  );
}

function ActionRow({ action }: { action: MemoryCurateAction; key?: Key }) {
  const content = action.content ?? "";
  const [expanded, setExpanded] = useState(false);
  const risk = actionRisk(action.op);
  const isRetag = action.op === "retag";
  const isDelete = action.op === "delete";
  const isEntityMerge = action.op === "entity_merge";
  const isEntityPrune = action.op === "entity_prune";
  const isEntityClassify = action.op === "entity_classify";
  const isReflect = action.op === "reflect";
  const { oldTags, newTags, kept, removed, added } = isRetag
    ? diffTags(action.old_tags, action.tags)
    : { oldTags: [], newTags: [], kept: [], removed: [], added: [] };
  const tagsOnlyReordered = isRetag && removed.length === 0 && added.length === 0;
  const normalizationOnly =
    isRetag &&
    removed.length > 0 &&
    added.length === 0 &&
    removed.every(isBookkeepingTag);

  return (
    <div className="flex flex-col gap-2 border border-border bg-background/40 px-3 py-2.5">
      <div className="flex items-start gap-2">
        <Badge className="shrink-0 text-[10px] uppercase mt-0.5">{action.op}</Badge>
        <span
          className={`shrink-0 rounded-sm border px-1.5 py-0.5 text-[10px] uppercase tracking-[0.08em] mt-0.5 ${riskClass(risk)}`}
          title={risk === "review" ? "Unknown operation; review carefully before applying." : `${risk} risk`}
        >
          {risk}
        </span>
        <div className="min-w-0 flex-1">
          <div className="text-xs font-medium text-foreground break-words">{describe(action)}</div>
        </div>
        {action.tier && (
          <span className="shrink-0 text-[10px] tracking-[0.08em] text-text-tertiary mt-0.5">
            {action.tier}
          </span>
        )}
      </div>

      {content && (
        <div className="flex flex-col gap-1 border-l-2 border-border pl-2.5">
          <div className="text-[10px] uppercase tracking-[0.08em] text-text-tertiary">
            Memory
          </div>
          <button
            type="button"
            onClick={() => setExpanded((v) => !v)}
            className="text-left"
            title={expanded ? "Show less" : "Show full fact"}
          >
            <p
              className={`text-xs text-text-secondary leading-relaxed break-words ${
                expanded ? "" : "line-clamp-3"
              }`}
            >
              {content}
            </p>
            <span className="text-[10px] uppercase tracking-[0.08em] text-text-tertiary">
              {expanded ? "show less" : "tap to expand"}
            </span>
          </button>
        </div>
      )}

      {isRetag && (
        <div className="flex flex-col gap-2 border border-border/60 bg-secondary/30 p-2 text-[11px] min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium text-text-secondary">Tag change</span>
            {normalizationOnly && (
              <span className="rounded-sm border border-border bg-background/60 px-1.5 py-0.5 text-[10px] uppercase tracking-[0.08em] text-text-tertiary">
                normalization only
              </span>
            )}
          </div>
          <TagBucket label="Kept" tags={kept} tone="neutral" />
          <TagBucket label="Removed" tags={removed} tone="removed" />
          <TagBucket label="Added" tags={added} tone="added" />
          {tagsOnlyReordered && (
            <span className="text-text-tertiary italic">
              Tags reordered/normalized — no tags added or removed.
            </span>
          )}
          {oldTags.length > 0 && newTags.length === 0 && (
            <span className="text-warning">
              This would leave the memory with no tags.
            </span>
          )}
        </div>
      )}

      {isDelete && (
        <div className="text-[11px] text-warning italic">
          Will be permanently deleted. This cannot be undone.
        </div>
      )}

      {isEntityMerge && (
        <div className="flex flex-col gap-1 text-[11px] text-text-tertiary">
          <span>
            Consolidates duplicate entity records and preserves their linked memories
            under the surviving entity.
          </span>
          {action.normalized_identity || action.fact_links_moved != null ? (
            <span className="font-mono-ui">
              {action.normalized_identity ? `identity=${action.normalized_identity}` : ""}
              {action.normalized_identity && action.fact_links_moved != null ? " · " : ""}
              {action.fact_links_moved != null ? `links moved=${action.fact_links_moved}` : ""}
            </span>
          ) : null}
        </div>
      )}

      {isEntityPrune && (
        <div className="flex flex-col gap-1 text-[11px] text-text-tertiary">
          <span>
            Removes a low-value, junk, or orphan entity reference without changing
            the underlying fact text.
          </span>
          {action.fact_links_removed != null ? (
            <span className="font-mono-ui">links removed={action.fact_links_removed}</span>
          ) : null}
        </div>
      )}

      {isEntityClassify && (
        <div className="flex flex-col gap-1 text-[11px] text-text-tertiary">
          <span>
            Adds a coarse type label used by the entity list, graph, and
            curator filters. The entity links and facts are unchanged.
          </span>
          <span className="font-mono-ui">
            {action.old_entity_type ?? "unknown"} → {action.entity_type}
            {action.fact_count != null ? ` · linked facts=${action.fact_count}` : ""}
          </span>
        </div>
      )}

      {isReflect && (
        <div className="text-[11px] text-text-tertiary">
          Creates a new {action.category ?? "general"} fact
          {action.supersedes?.length
            ? `, supersedes ${action.supersedes.map((s) => `#${s}`).join(", ")}`
            : ""}
          .
        </div>
      )}

      {action.reason && (
        <div className="text-[11px] text-text-tertiary leading-relaxed border-t border-border/50 pt-1.5 mt-0.5">
          {action.reason}
        </div>
      )}
    </div>
  );
}

export function ActionReviewGroup({
  group,
  defaultOpen,
}: {
  group: ActionGroupDef & { actions: MemoryCurateAction[] };
  defaultOpen: boolean;
  key?: Key;
}) {
  const [open, setOpen] = useState(defaultOpen);
  if (group.actions.length === 0) return null;
  const riskCounts = group.actions.reduce<Record<ActionRisk, number>>(
    (acc, action) => {
      acc[actionRisk(action.op)] += 1;
      return acc;
    },
    { low: 0, medium: 0, high: 0, review: 0 },
  );

  return (
    <section className="border border-border bg-background/30">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full min-w-0 items-start gap-2 px-3 py-2 text-left"
      >
        {open ? (
          <ChevronDown className="mt-0.5 h-3.5 w-3.5 shrink-0 text-text-tertiary" />
        ) : (
          <ChevronRight className="mt-0.5 h-3.5 w-3.5 shrink-0 text-text-tertiary" />
        )}
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs font-medium text-foreground">{group.label}</span>
            <Badge tone="outline" className="text-[10px]">
              {group.actions.length}
            </Badge>
            {RISK_ORDER.map((risk) =>
              riskCounts[risk] ? (
                <span
                  key={risk}
                  className={`rounded-sm border px-1.5 py-0.5 text-[10px] uppercase tracking-[0.08em] ${riskClass(risk)}`}
                >
                  {risk} {riskCounts[risk]}
                </span>
              ) : null,
            )}
          </div>
          <div className="mt-0.5 text-[11px] text-text-tertiary">
            {group.description}
          </div>
        </div>
      </button>
      {open ? (
        <div className="flex flex-col gap-2 border-t border-border/70 p-2">
          {group.actions.map((action, i) => (
            <ActionRow key={`${group.key}-${action.op}-${i}`} action={action} />
          ))}
        </div>
      ) : null}
    </section>
  );
}
