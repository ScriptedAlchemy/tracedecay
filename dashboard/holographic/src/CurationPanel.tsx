import { type ReactNode, type RefObject, useCallback, useEffect, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  History,
  ListChecks,
  ScrollText,
  Wand2,
} from "lucide-react";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import { Spinner } from "./Spinner";
import { api } from "./api";
import type {
  MemoryCurateAction,
  MemoryCurateResponse,
  MemoryCuratorActivityEvent,
  MemoryCuratorStatusResponse,
  MemoryOplogEvent,
} from "./types";

type CurationTab = "plan" | "history" | "activity";
type ActionRisk = "low" | "medium" | "high" | "review";

interface ActionGroupDef {
  key: string;
  label: string;
  description: string;
  ops: Set<string>;
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

function describe(a: MemoryCurateAction): string {
  switch (a.op) {
    case "merge":
      return `Merge #${a.loser} → #${a.winner}${a.similarity != null ? ` (sim ${a.similarity})` : ""}`;
    case "entity_merge": {
      const loser = a.loser_entity ?? a.loser ?? a.entity_id;
      const winner = a.winner_entity ?? a.winner ?? a.keep;
      const from = a.loser_name ? `${a.loser_name} (#${loser})` : `#${loser}`;
      const winnerName = a.winner_name ?? a.name;
      const to = winnerName ? `${winnerName} (#${winner})` : `#${winner}`;
      return `Merge entity ${from} → ${to}`;
    }
    case "entity_prune": {
      const entityId = a.entity_id ?? a.loser ?? a.fact_id;
      return a.name
        ? `Prune junk entity ${a.name} (#${entityId})`
        : `Prune junk entity #${entityId}`;
    }
    case "entity_classify": {
      const entityId = a.entity_id ?? a.fact_id;
      return a.name
        ? `Classify entity ${a.name} (#${entityId}) → ${a.entity_type}`
        : `Classify entity #${entityId} → ${a.entity_type}`;
    }
    case "delete":
      return a.duplicate_of != null
        ? `Delete #${a.fact_id} (duplicate of #${a.duplicate_of})`
        : `Delete #${a.fact_id}`;
    case "retag":
      return `Retag #${a.fact_id}`;
    case "recategorize":
      return `Recategorize #${a.fact_id} → ${a.category}`;
    case "reflect":
      return `Reflect (replaces ${(a.supersedes ?? []).map((s) => `#${s}`).join(", ")})`;
    default:
      return a.op;
  }
}

function splitTags(s?: string): string[] {
  return (s || "")
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
}

/** Compute tag buckets so the card shows what stays, not just what changes. */
function diffTags(oldStr?: string, newStr?: string) {
  const oldTags = splitTags(oldStr);
  const newTags = splitTags(newStr);
  const oldSet = new Set(oldTags);
  const newSet = new Set(newTags);
  return {
    oldTags,
    newTags,
    kept: oldTags.filter((t) => newSet.has(t)),
    removed: oldTags.filter((t) => !newSet.has(t)),
    added: newTags.filter((t) => !oldSet.has(t)),
  };
}

function isBookkeepingTag(tag: string): boolean {
  return tag.startsWith("cat:") || tag.startsWith("target:");
}

const DIAGNOSTIC_COUNT_KEYS = new Set([
  "contradictions_detected",
  "entity_scan_remaining",
  "entity_total",
  "entities_scanned",
  "orphan_entities",
  "orphan_entities_pruned",
  "related_clusters",
]);

const COUNT_LABELS: Record<string, string> = {
  delete: "delete",
  entity_merge: "entity merges",
  entity_classify: "entity classifications",
  entity_prune: "junk entities pruned",
  junk_entities_pruned: "junk entities pruned",
  merge: "fact merges",
  orphan_entities: "orphan entities",
  orphan_entities_pruned: "orphan entities pruned",
  recategorize: "recategorize",
  reflect: "reflections",
  retag: "retag",
};

function countLabel(key: string): string {
  return COUNT_LABELS[key] ?? key;
}

function actionRisk(op: string): ActionRisk {
  if (op === "retag" || op === "entity_prune" || op === "entity_classify") return "low";
  if (op === "entity_merge" || op === "recategorize") {
    return "medium";
  }
  // Fact removal is permanent (no archive/restore), so anything that deletes
  // or rewrites facts is high risk.
  if (op === "delete" || op === "merge" || op === "reflect") {
    return "high";
  }
  return "review";
}

function riskClass(risk: ActionRisk): string {
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

function groupActions(actions: MemoryCurateAction[]) {
  return ACTION_GROUPS.map((group) => ({
    ...group,
    actions:
      group.key === "other"
        ? actions.filter((action) => !ACTION_GROUPS.some((g) => g.key !== "other" && g.ops.has(action.op)))
        : actions.filter((action) => group.ops.has(action.op)),
  }));
}

function formatCounts(counts: Array<[string, number]>): string {
  if (!counts.length) return "no changes";
  return counts.map(([key, value]) => `${countLabel(key)}=${value}`).join(", ");
}

function MetadataRow({
  label,
  value,
}: {
  label: string;
  value: ReactNode;
}) {
  return (
    <div className="flex min-w-0 items-center justify-between gap-3 border-b border-border/50 py-2 last:border-b-0">
      <span className="text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
        {label}
      </span>
      <span className="min-w-0 text-right font-mono-ui text-xs text-text-secondary break-all">
        {value}
      </span>
    </div>
  );
}

function metadataValue(value: unknown, fallback = "auto"): ReactNode {
  if (value === null || value === undefined || value === "") return fallback;
  if (typeof value === "boolean") return value ? "yes" : "no";
  return String(value);
}

function activityTone(level?: string) {
  switch ((level || "info").toLowerCase()) {
    case "success":
      return "text-success";
    case "warning":
      return "text-warning";
    case "error":
      return "text-destructive";
    default:
      return "text-text-secondary";
  }
}

function formatActivityTime(ts: string): string {
  try {
    return new Date(ts).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return "--:--:--";
  }
}

function activityStatus(events: MemoryCuratorActivityEvent[], loading: boolean): string {
  const latest = events[events.length - 1];
  if (!latest) return "idle";
  if (latest.phase === "stale") return "stale";
  if (loading) return "live";
  if (latest.phase === "finish") return "complete";
  if (latest.phase === "lock") return "skipped";
  return "recent";
}

function activityStatusClass(status: string): string {
  switch (status) {
    case "live":
      return "border-success/30 bg-success/10 text-success";
    case "stale":
      return "border-warning/30 bg-warning/10 text-warning";
    case "complete":
      return "border-primary/30 bg-primary/10 text-primary";
    default:
      return "border-border bg-muted/30 text-text-tertiary";
  }
}

/**
 * One consistent localized timestamp for every curation history surface
 * (plan "saved" chip, history rows, preview metadata). Falls back to the raw
 * value when it is not a parseable date.
 */
function formatHistoryTime(ts?: string | null): string {
  if (!ts) return "";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return String(ts);
  return date.toLocaleString([], {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Oplog timestamps are unix seconds. */
function formatOplogTime(ts: number): string {
  if (!ts) return "";
  return formatHistoryTime(new Date(ts * 1000).toISOString());
}

/** Compact one-line summary of an oplog row's detail payload. */
function oplogDetailSummary(event: MemoryOplogEvent): string {
  const detail = event.detail ?? {};
  const parts = Object.entries(detail)
    .filter(([, value]) => value !== null && value !== undefined && value !== "")
    .slice(0, 4)
    .map(([key, value]) => `${key}=${String(value)}`);
  return parts.join(" · ");
}

function ActivityScroller({
  events,
  loading,
  error,
  scrollRef,
}: {
  events: MemoryCuratorActivityEvent[];
  loading: boolean;
  error: string;
  scrollRef: RefObject<HTMLDivElement>;
}) {
  const status = activityStatus(events, loading);
  const stale = status === "stale";
  return (
    <div className="flex min-h-0 flex-1 flex-col border border-border bg-background/40">
      <div className="flex items-center justify-between gap-2 border-b border-border px-3 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <ScrollText className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <span className="text-xs font-medium text-foreground">Live Activity</span>
        </div>
        <div className="flex shrink-0 items-center gap-2 text-[11px] text-text-tertiary">
          {loading && !stale ? <Spinner /> : null}
          <span
            className={`rounded border px-1.5 py-0.5 uppercase tracking-[0.08em] ${activityStatusClass(status)}`}
          >
            {status}
          </span>
          <span>{events.length} events</span>
        </div>
      </div>
      {error ? (
        <div className="border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {error}
        </div>
      ) : null}
      {stale ? (
        <div className="border-b border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
          The last curator run stopped reporting activity. Refresh or start a new preview to resume from a fresh run.
        </div>
      ) : null}
      <div
        ref={scrollRef}
        className="min-h-[12rem] flex-1 overflow-y-auto overflow-x-hidden p-3 font-mono-ui text-xs"
      >
        {events.length === 0 ? (
          <div className="text-text-tertiary">
            Start a preview or apply run to watch curator activity here.
          </div>
        ) : (
          <div className="flex flex-col gap-1.5">
            {events.map((event, index) => (
              <div
                key={`${event.ts}-${index}`}
                className="grid grid-cols-[4.5rem_5.5rem_minmax(0,1fr)] gap-2"
              >
                <span className="text-text-tertiary">{formatActivityTime(event.ts)}</span>
                <span className="truncate uppercase tracking-[0.08em] text-text-tertiary">
                  {event.phase}
                </span>
                <span className={`min-w-0 break-words ${activityTone(event.level)}`}>
                  {event.message}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

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

function ActionRow({ action }: { action: MemoryCurateAction }) {
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
      {/* Header row: badge + operation + tier */}
      <div className="flex items-start gap-2">
        <Badge className="shrink-0 text-[10px] uppercase mt-0.5">{action.op}</Badge>
        <span
          className={`shrink-0 rounded-sm border px-1.5 py-0.5 text-[10px] uppercase tracking-[0.08em] mt-0.5 ${riskClass(risk)}`}
          title={risk === "review" ? "Unknown operation; review carefully before applying." : `${risk} risk`}
        >
          {risk}
        </span>
        <div className="min-w-0 flex-1">
          <div className="text-xs font-medium text-foreground">{describe(action)}</div>
        </div>
        {action.tier && (
          <span className="shrink-0 text-[10px] tracking-[0.08em] text-text-tertiary mt-0.5">
            {action.tier}
          </span>
        )}
      </div>

      {/* Memory content — tap to expand the full fact (clamped by default) */}
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

      {/* Retag: show kept tags and the delta so normalization does not look destructive. */}
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
          <TagBucket label="Kept" tags={kept.length ? kept : newTags} tone="neutral" />
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

      {/* Reason from AI */}
      {action.reason && (
        <div className="text-[11px] text-text-tertiary leading-relaxed border-t border-border/50 pt-1.5 mt-0.5">
          {action.reason}
        </div>
      )}
    </div>
  );
}

function ActionGroup({
  group,
  defaultOpen,
}: {
  group: ActionGroupDef & { actions: MemoryCurateAction[] };
  defaultOpen: boolean;
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
            {(["high", "medium", "low", "review"] as ActionRisk[]).map((risk) =>
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

/**
 * Minimal confirm modal — local replacement for the core SPA's `ConfirmDialog`
 * (not exposed on the plugin SDK, and it relies on `react-dom` createPortal,
 * which the plugin's React shim does not provide). Rendered inline; the fixed
 * overlay still covers the viewport.
 */
function InlineConfirm({
  open,
  title,
  description,
  children,
  confirmLabel,
  loading,
  onCancel,
  onConfirm,
}: {
  open: boolean;
  title: string;
  description?: string;
  children?: ReactNode;
  confirmLabel: string;
  loading?: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  if (!open) return null;
  return (
    <div
      role="dialog"
      aria-modal="true"
      onClick={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
    >
      <div className="relative mx-4 w-full max-w-md border border-border bg-card shadow-lg">
        <div className="flex flex-col gap-1 border-b border-border p-4">
          <h2 className="font-mondwest text-display text-sm font-bold tracking-[0.12em]">
            {title}
          </h2>
          {description && (
            <p className="font-sans text-xs leading-relaxed text-muted-foreground">
              {description}
            </p>
          )}
        </div>
        {children ? <div className="border-b border-border p-4">{children}</div> : null}
        <div className="flex items-center justify-end gap-2 p-3">
          <Button type="button" outlined onClick={onCancel} disabled={loading}>
            Cancel
          </Button>
          <Button type="button" onClick={onConfirm} disabled={loading}>
            {loading ? "…" : confirmLabel}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default function CurationPanel({
  onApplied,
}: {
  onApplied?: () => void;
}) {
  const [report, setReport] = useState<MemoryCurateResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [previewSavedAt, setPreviewSavedAt] = useState<string | null>(null);
  const [previewStale, setPreviewStale] = useState(false);
  const [previewStaleReason, setPreviewStaleReason] = useState("");
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [error, setError] = useState("");
  const [activeTab, setActiveTab] = useState<CurationTab>("plan");
  const [status, setStatus] = useState<MemoryCuratorStatusResponse | null>(null);
  const [statusLoading, setStatusLoading] = useState(false);
  const [statusError, setStatusError] = useState("");
  const [oplog, setOplog] = useState<MemoryOplogEvent[]>([]);
  const [oplogError, setOplogError] = useState("");
  const [activity, setActivity] = useState<MemoryCuratorActivityEvent[]>([]);
  const [activityLoading, setActivityLoading] = useState(false);
  const [activityError, setActivityError] = useState("");
  const activityRef = useRef<HTMLDivElement>(null);
  const previewSavedAtRef = useRef<string | null>(null);
  // Anchor for visibility checks: keep-mounted hosts (the standalone shell)
  // hide inactive tab panels with `display: none` instead of unmounting them,
  // and a hidden panel must not keep polling the server.
  const panelRef = useRef<HTMLDivElement>(null);

  const applySavedPreview = useCallback((
    savedReport: MemoryCurateResponse,
    savedAt?: string | null,
    stale = false,
    staleReason = "",
  ) => {
    previewSavedAtRef.current = savedAt ?? null;
    setReport(savedReport);
    setPreviewSavedAt(savedAt ?? null);
    setPreviewStale(stale);
    setPreviewStaleReason(staleReason);
  }, []);

  const loadSavedPreview = useCallback((force = false) => {
    return api
      .getMemoryCuratorPreview()
      .then((r) => {
        if (r.report && (force || r.saved_at !== previewSavedAtRef.current)) {
          applySavedPreview(
            r.report,
            r.saved_at ?? null,
            Boolean(r.stale),
            r.stale_reason || "",
          );
        } else if (!r.report && !loading && !applying) {
          previewSavedAtRef.current = null;
          setReport(null);
          setPreviewSavedAt(null);
          setPreviewStale(false);
          setPreviewStaleReason("");
        }
        return r;
      })
      .catch(() => {});
  }, [applySavedPreview, applying, loading]);

  const loadActivity = useCallback((showSpinner = false) => {
    if (showSpinner) setActivityLoading(true);
    setActivityError("");
    api
      .getMemoryCuratorActivity({ limit: 120 })
      .then((r) => {
        const events = r.events || [];
        setActivity(events);
        const latestFinish = [...events]
          .reverse()
          .find((event) => event.phase === "finish" && !event.synthetic);
        if (latestFinish?.dry_run) {
          loadSavedPreview(false);
        }
      })
      .catch((e) => setActivityError(e instanceof Error ? e.message : String(e)))
      .finally(() => {
        if (showSpinner) setActivityLoading(false);
      });
  }, [loadSavedPreview]);

  useEffect(() => {
    loadSavedPreview(true);
  }, [loadSavedPreview]);

  const preview = () => {
    setLoading(true);
    setError("");
    setActiveTab("activity");
    loadActivity(true);
    api
      .postMemoryCurate({ dry_run: true })
      .then((r) => {
        setReport(r);
        const savedAt = new Date().toISOString();
        previewSavedAtRef.current = savedAt;
        setPreviewSavedAt(savedAt);
        setPreviewStale(false);
        setPreviewStaleReason("");
        loadSavedPreview(true);
        loadActivity();
        loadStatus();
        // Land on the plan once the dry run finishes — the activity tab only
        // matters while the run is in flight.
        setActiveTab("plan");
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  };

  const apply = () => {
    setApplying(true);
    setError("");
    setActiveTab("activity");
    loadActivity(true);
    api
      .postMemoryCurate({ dry_run: false })
      .then((r) => {
        setReport(r);
        setPreviewSavedAt(null);
        setPreviewStale(false);
        setPreviewStaleReason("");
        setConfirmOpen(false);
        loadActivity();
        loadStatus();
        onApplied?.();
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setApplying(false));
  };

  const loadStatus = useCallback(() => {
    setStatusLoading(true);
    setStatusError("");
    api
      .getMemoryCuratorStatus()
      .then((r) => setStatus(r))
      .catch((e) => setStatusError(e instanceof Error ? e.message : String(e)))
      .finally(() => setStatusLoading(false));
  }, []);

  const loadOplog = useCallback(() => {
    setOplogError("");
    api
      .getMemoryOplog({ limit: 30 })
      .then((r) => {
        setOplog(r.events || []);
        if (r.error) setOplogError(r.error);
      })
      .catch((e) => setOplogError(e instanceof Error ? e.message : String(e)));
  }, []);

  useEffect(() => {
    if (activeTab === "plan" && !loading && !applying) {
      loadSavedPreview(false);
    }
  }, [activeTab, applying, loadSavedPreview, loading]);

  useEffect(() => {
    if (activeTab === "history" && !status && !statusLoading) {
      loadStatus();
    }
  }, [activeTab, loadStatus, status, statusLoading]);

  useEffect(() => {
    if (activeTab === "history") {
      loadOplog();
    }
  }, [activeTab, loadOplog]);

  useEffect(() => {
    if (activeTab === "activity" && activity.length === 0) {
      loadActivity(true);
    }
  }, [activeTab, activity.length, loadActivity]);

  useEffect(() => {
    if (activeTab !== "activity" && !loading && !applying) return undefined;
    const interval = window.setInterval(() => {
      // Suspend polling while the panel is hidden (offsetParent is null under
      // a display:none ancestor); the next tick after re-show refreshes.
      if (panelRef.current?.offsetParent === null) return;
      loadActivity(false);
    }, loading || applying ? 900 : 2500);
    return () => window.clearInterval(interval);
  }, [activeTab, applying, loadActivity, loading]);

  useEffect(() => {
    const el = activityRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [activity]);

  const actions = report?.actions ?? [];
  const counts = report?.counts ?? {};
  const isPlan = report?.dry_run ?? true;
  const shownCounts = isPlan ? counts : (report?.applied_counts ?? counts);
  const actionCounts = Object.entries(shownCounts).filter(
    ([key]) => !DIAGNOSTIC_COUNT_KEYS.has(key),
  );
  const diagnosticCounts = Object.entries(counts).filter(([key]) =>
    DIAGNOSTIC_COUNT_KEYS.has(key),
  );
  const actionGroups = groupActions(actions);
  const nonEmptyActionGroups = actionGroups.filter((group) => group.actions.length > 0);
  const planLabel = actions.length ? `Plan ${actions.length}` : "Plan";
  const confirmGroupCounts = nonEmptyActionGroups.map((group) => [
    group.label,
    group.actions.length,
  ] as const);
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
          className="grid grid-cols-3 gap-1 rounded-sm border border-border bg-secondary/30 p-1 shrink-0"
        >
          {tabs.map(({ id, label, Icon }) => {
            const active = activeTab === id;
            return (
              <button
                key={id}
                type="button"
                onClick={() => setActiveTab(id)}
                className={`flex min-w-0 items-center justify-center gap-1.5 px-2 py-1.5 text-xs ${
                  active
                    ? "bg-background text-foreground shadow-sm"
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
          <>
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
                  <ActionGroup
                    key={group.key}
                    group={group}
                    defaultOpen={i === 0}
                  />
                ))}
              </div>
            ) : null}
          </>
        ) : null}

        {activeTab === "activity" ? (
          <div className="flex flex-1 min-h-0 flex-col gap-3">
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
          <div className="flex flex-1 min-h-0 flex-col gap-3 overflow-y-auto overflow-x-hidden pr-1">
            <div className="flex min-w-0 items-center justify-between gap-2 shrink-0">
              <div className="min-w-0">
                <div className="text-xs font-medium text-foreground">
                  Curator Status
                </div>
                <div className="text-[11px] text-text-tertiary">
                  Scheduler state, last run summary, and recent snapshots.
                </div>
              </div>
              <Button
                size="xs"
                ghost
                disabled={statusLoading}
                onClick={loadStatus}
                className="shrink-0 gap-2"
              >
                {statusLoading ? <Spinner /> : null}
                Refresh
              </Button>
            </div>
            {statusError ? (
              <div className="border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive shrink-0">
                {statusError}
              </div>
            ) : null}
            {status ? (
              <>
                <div className="border border-border bg-background/30 px-3">
                  <div className="pt-2 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
                    Run history
                  </div>
                  <MetadataRow label="Provider" value={status.provider || "none"} />
                  <MetadataRow label="Run count" value={status.state.run_count} />
                  <MetadataRow
                    label="Last apply"
                    value={formatHistoryTime(status.state.last_run_at) || "never"}
                  />
                  <MetadataRow label="Last applied summary" value={status.state.last_run_summary || "none"} />
                  <MetadataRow
                    label="Last preview"
                    value={formatHistoryTime(status.state.last_preview_at) || "never"}
                  />
                  <MetadataRow label="Last preview summary" value={status.state.last_preview_summary || "none"} />
                </div>
                <div className="border border-border bg-background/30 px-3">
                  <div className="pt-2 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
                    Curator configuration
                  </div>
                  <div className="pt-1 text-[11px] text-text-tertiary">
                    Settings, not run results — "auto" means the provider
                    default is used.
                  </div>
                  <MetadataRow label="Enabled" value={status.config.enabled ? "yes" : "no"} />
                  <MetadataRow label="Paused" value={status.state.paused ? "yes" : "no"} />
                  <MetadataRow label="Interval hours" value={metadataValue(status.config.interval_hours)} />
                  <MetadataRow label="Idle gate hours" value={metadataValue(status.config.min_idle_hours)} />
                  <MetadataRow label="Resolved mode" value={metadataValue(status.config.mode, "fast")} />
                  <MetadataRow label="Dry-run first" value={metadataValue(status.config.dry_run_first, "no")} />
                  <MetadataRow label="Scan cap" value={metadataValue(status.config.scan_cap)} />
                  <MetadataRow label="Scan cap grace" value={metadataValue(status.config.scan_cap_grace, "0")} />
                  <MetadataRow label="Max candidates" value={metadataValue(status.config.max_candidates)} />
                  <MetadataRow
                    label="Related threshold"
                    value={metadataValue(status.config.related_cluster_threshold)}
                  />
                  <MetadataRow
                    label="Batch size"
                    value={metadataValue(status.config.batch_size)}
                  />
                  <MetadataRow
                    label="Tool calls / batch"
                    value={metadataValue(status.config.max_tool_calls_per_batch)}
                  />
                  <MetadataRow
                    label="LLM calls / run"
                    value={metadataValue(status.config.max_llm_calls_per_run)}
                  />
                  <MetadataRow label="Facts / run" value={metadataValue(status.config.per_run_facts)} />
                  <MetadataRow
                    label="Candidates / fact"
                    value={metadataValue(status.config.candidates_per_fact)}
                  />
                  <MetadataRow
                    label="Source cap"
                    value={metadataValue(status.config.candidate_source_cap)}
                  />
                  <MetadataRow
                    label="Expansion scan cap"
                    value={metadataValue(status.config.candidate_expansion_scan_cap)}
                  />
                  <MetadataRow
                    label="Parallel LLM"
                    value={metadataValue(status.config.max_parallel_llm)}
                  />
                </div>
                <div className="border border-border bg-background/30 px-3 py-2">
                  <div className="mb-1 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
                    Recent snapshots
                  </div>
                  {status.snapshots.length ? (
                    <div className="flex flex-col gap-1">
                      {status.snapshots.map((snapshot) => (
                        <div
                          key={snapshot.path}
                          className="min-w-0 font-mono-ui text-xs text-text-secondary break-all"
                        >
                          {snapshot.name}
                        </div>
                      ))}
                    </div>
                  ) : (
                    <div className="text-xs text-text-tertiary">No snapshots found.</div>
                  )}
                </div>
              </>
            ) : null}
            <div className="border border-border bg-background/30 px-3 py-2">
              <div className="mb-1 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
                Recent memory operations
              </div>
              {oplogError ? (
                <div className="text-xs text-destructive">{oplogError}</div>
              ) : null}
              {oplog.length ? (
                <div className="flex flex-col gap-1">
                  {oplog.map((event) => (
                    <div
                      key={event.id}
                      className="grid grid-cols-[7.5rem_5.5rem_minmax(0,1fr)] gap-2 font-mono-ui text-xs"
                    >
                      <span className="text-text-tertiary">{formatOplogTime(event.ts)}</span>
                      <span className="truncate uppercase tracking-[0.08em] text-text-secondary">
                        {event.op}
                      </span>
                      <span className="min-w-0 break-all text-text-tertiary">
                        {event.fact_id != null ? `#${event.fact_id} ` : ""}
                        {oplogDetailSummary(event)}
                      </span>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="text-xs text-text-tertiary">
                  No memory operations recorded yet.
                </div>
              )}
            </div>
            {report ? (
              <>
                <div className="text-xs font-medium text-foreground">
                  Current Preview
                </div>
                <div className="border border-border bg-background/30 px-3">
                  <MetadataRow label="Run mode" value={isPlan ? "preview" : "applied"} />
                  {previewSavedAt ? (
                    <MetadataRow label="Saved" value={formatHistoryTime(previewSavedAt)} />
                  ) : null}
                  {previewStale ? (
                    <MetadataRow
                      label="Preview state"
                      value={previewStaleReason || "stale"}
                    />
                  ) : null}
                  <MetadataRow label="Actions" value={actions.length} />
                  <MetadataRow label="Counts" value={formatCounts(actionCounts)} />
                  <MetadataRow label="Signals" value={formatCounts(diagnosticCounts)} />
                  <MetadataRow label="LLM calls" value={report.llm_calls} />
                  {report.skipped_actions != null ? (
                    <MetadataRow label="Skipped" value={report.skipped_actions} />
                  ) : null}
                  {report.snapshot ? (
                    <MetadataRow label="Snapshot" value={report.snapshot} />
                  ) : null}
                </div>
                {report.coverage ? (
                  <div className="border border-border bg-background/30 px-3">
                    <MetadataRow
                      label="Facts scanned"
                      value={`${report.coverage.scanned}/${report.coverage.active_total}`}
                    />
                    <MetadataRow
                      label="Facts due"
                      value={report.coverage.due_remaining}
                    />
                    {report.coverage.entity_total != null ? (
                      <MetadataRow
                        label="Entities scanned"
                        value={`${report.coverage.entities_scanned ?? 0}/${report.coverage.entity_total}`}
                      />
                    ) : null}
                    {report.coverage.entity_scan_remaining != null ? (
                      <MetadataRow
                        label="Entities due"
                        value={report.coverage.entity_scan_remaining}
                      />
                    ) : null}
                  </div>
                ) : null}
              </>
            ) : (
              <div className="border border-border bg-background/30 px-3 py-4 text-xs text-text-tertiary">
                Preview a plan to see current run metadata, signals, and coverage.
              </div>
            )}
          </div>
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
