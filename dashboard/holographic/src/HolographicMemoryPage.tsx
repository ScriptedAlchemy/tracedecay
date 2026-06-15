import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  BrainCircuit,
  Copy,
  Database,
  Network,
  RefreshCw,
  Search,
  Sparkles,
  Table2,
  Wand2,
  X,
} from "lucide-react";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Input,
} from "./sdk";
import { Spinner } from "./Spinner";
import { api } from "./api";
import type {
  HolographicFact,
  MemoryDashboardResponse,
  MemorySimilarityPair,
} from "./types";
import SemanticMap, { type SemanticMapFocus } from "./SemanticMap";
import SimilarityPanel from "./SimilarityPanel";
import AssociationGraph from "./AssociationGraph";
import CurationPanel from "./CurationPanel";
import { NUM_BADGE } from "./ui";
import { MiniBar } from "./MiniBar";
import { categoryColorMap, slotColor } from "./viz/colors";
import { PanelError, PanelLoading } from "./viz/Status";
import { Sparkline } from "./viz/Sparkline";
import { CompositionBar } from "./viz/CompositionBar";
import { BrushableHistogram } from "./viz/Histogram";
import type { Bin } from "./viz/scale";

const INSPECTOR_ROW_LIMIT = 25;
const GRAPH_ROW_LIMIT = 500;

/** Render a JSON-array string ("[\"a\",\"b\"]") as chips; hide empty/invalid. */
function JsonChips({ raw, label }: { raw?: string | null; label: string }) {
  const items = useMemo(() => {
    const value = (raw ?? "").trim();
    if (!value || value === "[]") return [];
    try {
      const parsed = JSON.parse(value);
      if (Array.isArray(parsed)) return parsed.map(String).filter(Boolean);
    } catch {
      /* not JSON — fall through to raw text */
    }
    return [value];
  }, [raw]);

  if (items.length === 0) return null;
  return (
    <p className="mt-2 flex flex-wrap gap-1" aria-label={label}>
      {items.map((item, i) => (
        <span key={`${item}-${i}`} className="hv-chip" style={{ cursor: "default" }}>
          {item}
        </span>
      ))}
    </p>
  );
}

function highlighted(snippet?: string, fallback?: string | null) {
  const text = snippet || fallback || "";
  const parts: ReactNode[] = [];
  const regex = />>>(.*?)<<</g;
  let last = 0;
  let index = 0;
  let match: RegExpExecArray | null;
  while ((match = regex.exec(text)) !== null) {
    if (match.index > last) parts.push(text.slice(last, match.index));
    parts.push(
      <mark key={index++} className="bg-warning/30 px-0.5 text-warning">
        {match[1]}
      </mark>,
    );
    last = regex.lastIndex;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

function Stat({
  label,
  value,
  hint,
}: {
  label: string;
  value: string | number;
  /** Plain-language explanation surfaced as a native tooltip. */
  hint?: string;
}) {
  return (
    <div
      className="border border-border bg-background/50 px-3 py-2"
      title={hint}
      style={hint ? { cursor: "help" } : undefined}
    >
      <div className="font-mono-ui text-lg leading-none text-foreground">
        {value}
      </div>
      <div className="mt-1 text-xs tracking-[0.08em] text-text-tertiary">
        {label}
      </div>
    </div>
  );
}

function DataBars<T>({
  getLabel,
  getTone,
  getValue,
  getColor,
  items,
  title,
  header,
}: {
  getLabel: (item: T) => string;
  getTone?: (item: T) => string;
  getValue: (item: T) => number;
  /** Token color per row, shared with the map legend for cross-view cohesion. */
  getColor?: (item: T) => string;
  items: T[];
  title: string;
  /** Optional small-multiple rendered between the title and the rows. */
  header?: ReactNode;
}) {
  const max = Math.max(...items.map(getValue), 1);

  return (
    <div className="border border-border bg-background/30 p-3">
      <h3 className="mb-3 font-mondwest text-display text-xs tracking-[0.12em] text-text-secondary">
        {title}
      </h3>
      {header && <div className="mb-3">{header}</div>}
      <div className="flex flex-col gap-2">
        {items.length === 0 ? (
          <p className="text-xs text-text-tertiary">No data.</p>
        ) : (
          items.map((item, index) => {
            const value = getValue(item);
            const color = getColor?.(item);
            return (
              <div key={`${getLabel(item)}-${index}`} className="grid gap-1">
                <div className="flex min-w-0 items-center justify-between gap-3">
                  <span className="flex min-w-0 items-center gap-1.5 truncate text-xs text-text-secondary">
                    {color && (
                      <span className="hv-swatch" style={{ background: color }} />
                    )}
                    <span className="min-w-0 truncate">{getLabel(item)}</span>
                  </span>
                  <span className="font-mono-ui text-xs text-text-tertiary">
                    {value}
                    {getTone ? ` · ${getTone(item)}` : ""}
                  </span>
                </div>
                <MiniBar pct={(value / max) * 100} color={color} />
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function displayEntityType(entityType?: string | null) {
  const value = (entityType || "").trim();
  if (!value || value.toLowerCase() === "unknown") return "";
  return value;
}

function entityTypeBucketLabel(entityType?: string | null) {
  return displayEntityType(entityType) || "unclassified";
}

function SystemStrip({
  data,
}: {
  data: MemoryDashboardResponse;
}) {
  const provider = data.providers;
  const activeMemory = provider.memory_provider || "built-in";
  const pluginEngine = provider.plugin_context_engine;
  const curatorTools = provider.curator_tools;
  const agentToolsets = curatorTools?.agent_toolsets?.length ?? 0;
  const db = data.holographic;

  return (
    <div className="grid gap-3 sm:grid-cols-[repeat(2,minmax(0,1fr))] xl:grid-cols-[repeat(7,minmax(0,1fr))]">
      <Stat label="memory provider" value={activeMemory} />
      <Stat label="context engine" value={provider.context_engine || "compressor"} />
      <Stat label="context engine tools" value={pluginEngine?.tools?.length ?? 0} />
      <Stat
        label="curator tools"
        value={curatorTools?.enabled ? `${curatorTools.count ?? 0} enabled` : "not used"}
        hint={
          curatorTools?.enabled
            ? "Evidence-gathering tools the memory curator can call while reviewing facts."
            : "The memory curator runs without LLM tool calls here — duplicates are detected with built-in vector-similarity analysis instead."
        }
      />
      <Stat
        label="curator agents"
        value={agentToolsets > 0 ? agentToolsets : "none"}
        hint={
          agentToolsets > 0
            ? "Agent toolsets the curator can delegate cleanup work to."
            : "No agent-driven curation is configured. Curation previews and applies still work — they use the built-in deduplication planner."
        }
      />
      <Stat label="database" value={db.exists ? "ready" : "missing"} />
      <div className="min-w-0 border border-border bg-background/50 px-3 py-2 sm:col-span-2 xl:col-span-1">
        <div className="truncate font-mono-ui text-xs text-foreground">
          {db.path}
        </div>
        <div className="mt-1 text-xs tracking-[0.08em] text-text-tertiary">
          storage path
        </div>
      </div>
    </div>
  );
}

function SearchBox({
  query,
  refreshing,
  setQuery,
}: {
  query: string;
  refreshing: boolean;
  setQuery: (value: string) => void;
}) {
  return (
    <div className="relative min-w-0 w-full sm:max-w-xl">
      {refreshing ? (
        <Spinner className="absolute left-2.5 top-1/2 -translate-y-1/2 text-[0.875rem] text-primary" />
      ) : (
        <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
      )}
      <Input
        placeholder="Search holographic facts"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        className="h-8 py-0 pr-7 pl-8 text-xs leading-none"
      />
      {query && (
        <Button
          ghost
          size="xs"
          className="absolute right-1.5 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
          onClick={() => setQuery("")}
          aria-label="Clear"
        >
          <X />
        </Button>
      )}
    </div>
  );
}

function FactList({ data }: { data: MemoryDashboardResponse }) {
  const facts = data.holographic.facts;

  return (
    <Card className="overflow-hidden flex flex-col max-h-[60vh] md:max-h-[38rem] min-w-0">
      <CardHeader className="shrink-0">
        <CardTitle>{data.query ? "Matching Facts" : "Facts"}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col flex-1 min-h-0 overflow-hidden">
        <div
          className="flex flex-1 min-h-0 flex-col gap-2 overflow-y-auto overflow-x-hidden pr-1"
          tabIndex={0}
          role="region"
          aria-label={data.query ? "Matching facts" : "Facts"}
        >
          {facts.length === 0 ? (
            <p className="text-xs text-text-tertiary">No facts.</p>
          ) : (
            facts.map((fact: HolographicFact) => (
              <div key={fact.fact_id} className="border border-border p-3">
                <div className="mb-1 flex min-w-0 flex-wrap items-center gap-2">
                  <Badge tone="secondary" className={NUM_BADGE}>
                    {fact.category}
                  </Badge>
                  <span className="font-mono-ui text-xs text-text-tertiary">
                    #{fact.fact_id}
                  </span>
                  <Badge tone="outline" className={NUM_BADGE}>
                    trust {Number(fact.trust_score ?? 0).toFixed(2)}
                  </Badge>
                  <Badge tone="outline" className={NUM_BADGE}>
                    used {fact.retrieval_count}
                  </Badge>
                  <Badge tone="outline" className={NUM_BADGE}>
                    helpful {fact.helpful_count}
                  </Badge>
                  <Badge tone="outline" className={NUM_BADGE}>
                    {fact.has_hrr ? "HRR" : "no HRR"}
                  </Badge>
                </div>
                <p className="whitespace-pre-wrap text-sm leading-relaxed text-foreground">
                  {highlighted(fact.snippet, fact.content)}
                </p>
                <JsonChips raw={fact.tags} label="Tags" />
              </div>
            ))
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function EntityAndBankLists({ data }: { data: MemoryDashboardResponse }) {
  const h = data.holographic;
  const banks = h.overview?.memory_banks ?? [];
  const bankColors = useMemo(
    () => categoryColorMap(banks.map((b) => b.bank_name)),
    [banks],
  );

  return (
    <div className="grid min-w-0 gap-4 lg:grid-cols-[repeat(2,minmax(0,1fr))]">
      <Card className="overflow-hidden flex flex-col max-h-[50vh] md:max-h-[32rem] min-w-0">
        <CardHeader className="shrink-0">
          <CardTitle>Entities</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col flex-1 min-h-0 overflow-hidden">
          <div
            className="flex flex-1 min-h-0 flex-col gap-2 overflow-y-auto overflow-x-hidden pr-1"
            tabIndex={0}
            role="region"
            aria-label="Entities"
          >
            {h.entities.length === 0 ? (
              <p className="text-xs text-text-tertiary">No entities.</p>
            ) : (
              h.entities.map((entity) => (
                <div key={entity.entity_id} className="border border-border p-3">
                  <div className="flex min-w-0 items-center gap-2">
                    <span className="min-w-0 flex-1 truncate text-sm text-foreground">
                      {entity.name}
                    </span>
                    <Badge tone="outline" className={NUM_BADGE}>
                      {entity.fact_count}
                    </Badge>
                  </div>
                  {displayEntityType(entity.entity_type) && (
                    <p className="mt-1 text-xs text-text-tertiary">
                      {displayEntityType(entity.entity_type)}
                    </p>
                  )}
                  <JsonChips raw={entity.aliases} label="Aliases" />
                </div>
              ))
            )}
          </div>
        </CardContent>
      </Card>

      <Card className="overflow-hidden flex flex-col max-h-[50vh] md:max-h-[32rem] min-w-0">
        <CardHeader className="shrink-0">
          <CardTitle>Memory Banks</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col flex-1 min-h-0 overflow-hidden">
          {banks.length > 0 && (
            <div className="mb-3 shrink-0">
              <CompositionBar
                totalLabel="facts"
                segments={banks.map((bank) => ({
                  key: bank.bank_name,
                  label: bank.bank_name,
                  value: bank.fact_count,
                  color: bankColors.get(bank.bank_name) ?? "var(--hm-primary)",
                  detail: [{ label: "dim", value: bank.dim }],
                }))}
              />
            </div>
          )}
          <div
            className="flex flex-1 min-h-0 flex-col gap-2 overflow-y-auto overflow-x-hidden pr-1"
            tabIndex={0}
            role="region"
            aria-label="Memory banks"
          >
            {banks.length ? (
              banks.map((bank) => (
                <div key={bank.bank_id} className="border border-border p-3">
                  <div className="flex min-w-0 items-center gap-2">
                    <Database className="h-3.5 w-3.5 text-text-tertiary" />
                    <span
                      className="hv-swatch"
                      style={{ background: bankColors.get(bank.bank_name) }}
                    />
                    <span className="min-w-0 flex-1 truncate font-mono-ui text-xs text-foreground">
                      {bank.bank_name}
                    </span>
                    <Badge tone="outline" className={NUM_BADGE}>
                      dim {bank.dim}
                    </Badge>
                  </div>
                  <p className="mt-1 text-xs text-text-tertiary">
                    {bank.fact_count} facts
                  </p>
                </div>
              ))
            ) : (
              <p className="text-xs text-text-tertiary">No banks.</p>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

const HRR_STATUS_STROKE: Record<string, string> = {
  ready: "stroke-success",
  missing_vectors: "stroke-warning",
  missing_bank: "stroke-destructive",
  stale_bank: "stroke-text-tertiary",
};

const HRR_STATUS_TEXT: Record<string, string> = {
  ready: "text-success",
  missing_vectors: "text-warning",
  missing_bank: "text-destructive",
  stale_bank: "text-text-tertiary",
};

type HrrCoverageRow = NonNullable<
  MemoryDashboardResponse["holographic"]["overview"]
>["hrr_coverage"][number];

/**
 * Plain-language status explanations. Coverage (facts with HRR vectors) and
 * bank freshness are independent, so "100% coverage" can legitimately pair
 * with "stale bank" — spell that out instead of looking contradictory.
 */
function hrrStatusHint(row: HrrCoverageRow): string {
  switch (row.status) {
    case "ready":
      return "The bundled bank vector is up to date with every vectored fact in this category.";
    case "missing_vectors":
      return "No facts in this category have HRR vectors yet, so there is nothing to bundle.";
    case "missing_bank":
      return "Facts here have HRR vectors, but no bundled memory bank has been built for this category yet.";
    case "stale_bank":
      return (
        `Coverage means ${row.hrr_vectors} of ${row.facts} facts have HRR vectors — that can be 100% ` +
        `while the bank is stale. "Stale bank" means the bundled bank vector was last built from ` +
        `${row.bank_fact_count} fact(s), so it lags the current store until the next bank rebuild refreshes it automatically.`
      );
    default:
      return "";
  }
}

function CoverageGauge({ pct, status }: { pct: number; status: string }) {
  const stroke = HRR_STATUS_STROKE[status] ?? "stroke-text-tertiary";
  // r chosen so circumference ≈ 100, letting strokeDasharray = "pct 100".
  const r = 15.915;
  const clamped = Math.max(0, Math.min(100, pct));
  return (
    <div className="relative h-16 w-16 shrink-0">
      <svg viewBox="0 0 36 36" className="h-full w-full -rotate-90">
        <circle
          cx="18"
          cy="18"
          r={r}
          fill="none"
          strokeWidth="3"
          className="stroke-muted"
        />
        {/* Skip the arc at 0%; a round linecap on a zero-length dash leaves a dot. */}
        {clamped > 0 && (
          <circle
            cx="18"
            cy="18"
            r={r}
            fill="none"
            strokeWidth="3"
            strokeLinecap="round"
            strokeDasharray={`${clamped} 100`}
            className={stroke}
          />
        )}
      </svg>
      <div className="absolute inset-0 flex items-center justify-center font-mono-ui text-xs text-foreground">
        {pct}%
      </div>
    </div>
  );
}

function HrrCoveragePanel({ data }: { data: MemoryDashboardResponse }) {
  const rows = data.holographic.overview?.hrr_coverage ?? [];

  return (
    <Card>
      <CardHeader>
        <CardTitle>HRR Coverage</CardTitle>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <p className="text-xs text-text-tertiary">No categories.</p>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2">
            {rows.map((row) => {
              const pct = Math.round((row.coverage ?? 0) * 100);
              const textTone = HRR_STATUS_TEXT[row.status] ?? "text-text-tertiary";
              const hint = hrrStatusHint(row);
              return (
                <div
                  key={row.category}
                  className="flex min-w-0 items-center gap-3 border border-border p-3"
                >
                  <CoverageGauge pct={pct} status={row.status} />
                  <div className="flex min-w-0 flex-1 flex-col gap-1">
                    <span className="min-w-0 truncate font-mono-ui text-xs text-foreground">
                      {row.category}
                    </span>
                    <span
                      className={`truncate font-mono-ui text-[0.65rem] tracking-[0.04em] ${textTone}`}
                      title={hint || undefined}
                      style={
                        hint
                          ? {
                              cursor: "help",
                              textDecoration: "underline dotted",
                              textUnderlineOffset: 2,
                            }
                          : undefined
                      }
                    >
                      {row.status.replaceAll("_", " ")}
                    </span>
                    <span className="font-mono-ui text-[0.65rem] text-text-tertiary">
                      {row.hrr_vectors} / {row.facts} vectors
                    </span>
                    <span className="min-w-0 truncate font-mono-ui text-[0.65rem] text-text-tertiary">
                      {row.bank_name}
                    </span>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function TrustDistribution({ data }: { data: MemoryDashboardResponse }) {
  const buckets = data.holographic.overview?.trust_histogram ?? [];
  const total = buckets.reduce((sum, b) => sum + b.count, 0);
  // The server pre-bins trust into ten 0.1-wide buckets; adapt to shared bins.
  const bins: Bin[] = buckets.map((b) => ({
    x0: b.bucket / 10,
    x1: (b.bucket + 1) / 10,
    count: b.count,
  }));
  const mean =
    total > 0
      ? buckets.reduce((sum, b) => sum + (b.bucket / 10 + 0.05) * b.count, 0) / total
      : 0;

  return (
    <Card>
      <CardHeader>
        <CardTitle>Trust Distribution</CardTitle>
      </CardHeader>
      <CardContent>
        {buckets.length === 0 ? (
          <p className="text-xs text-text-tertiary">No data.</p>
        ) : (
          <>
            <BrushableHistogram
              bins={bins}
              height={110}
              format={(v) => v.toFixed(1)}
              tipLabel="facts"
            />
            <div className="mt-1 flex items-center justify-between font-mono-ui text-[0.65rem] text-text-tertiary">
              <span>trust score</span>
              <span className="text-text-secondary">
                {total} facts · mean {mean.toFixed(2)}
              </span>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

function GrowthSparkline({ data }: { data: MemoryDashboardResponse }) {
  const series = data.holographic.overview?.growth ?? [];
  // Older payloads don't carry cumulative_facts; hide the toggle then.
  const hasCumulative = series.some((d) => d.cumulative_facts != null);
  const [mode, setMode] = useState<"daily" | "cumulative">("daily");
  const effectiveMode = hasCumulative ? mode : "daily";

  const total = series.reduce((sum, d) => sum + d.facts, 0);
  const peak = series.reduce(
    (best, d) => (d.facts > best.facts ? d : best),
    { date: "", facts: 0 },
  );
  const latestCumulative = hasCumulative
    ? (series[series.length - 1]?.cumulative_facts ?? 0)
    : 0;

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          {effectiveMode === "cumulative" ? "Total Facts · Growth" : "Facts / Day"}
        </CardTitle>
      </CardHeader>
      <CardContent>
        {series.length === 0 ? (
          <p className="text-xs text-text-tertiary">No facts created recently.</p>
        ) : (
          <>
            {hasCumulative && (
              <div
                className="mb-2 flex items-center gap-1"
                role="group"
                aria-label="Growth chart mode"
              >
                {(["daily", "cumulative"] as const).map((m) => (
                  <button
                    key={m}
                    type="button"
                    className={`hv-chip${effectiveMode === m ? " hv-chip-active" : ""}`}
                    aria-pressed={effectiveMode === m}
                    onClick={() => setMode(m)}
                  >
                    {m}
                  </button>
                ))}
              </div>
            )}
            <Sparkline
              points={series.map((d) => ({
                label: d.date,
                value:
                  effectiveMode === "cumulative"
                    ? (d.cumulative_facts ?? d.facts)
                    : d.facts,
              }))}
              height={110}
              valueLabel={effectiveMode === "cumulative" ? "total facts" : "facts"}
              color={
                effectiveMode === "cumulative" ? "var(--hm-cat-1)" : "var(--hm-primary)"
              }
            />
            <div className="mt-1 flex items-center justify-between font-mono-ui text-[0.65rem] text-text-tertiary">
              <span>{series[0]?.date}</span>
              <span className="text-text-secondary">
                {effectiveMode === "cumulative"
                  ? `${latestCumulative} total facts`
                  : `${total} facts · peak ${peak.facts}${peak.date ? ` on ${peak.date}` : ""}`}
              </span>
              <span>{series[series.length - 1]?.date}</span>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

function OverviewBars({
  overview,
  data,
}: {
  overview: NonNullable<MemoryDashboardResponse["holographic"]["overview"]>;
  data: MemoryDashboardResponse;
}) {
  const categoryColors = useMemo(
    () => categoryColorMap(overview.categories.map((c) => c.category)),
    [overview.categories],
  );
  const entityTypes = overview.entity_types.map((item, i) => ({
    ...item,
    bucket: entityTypeBucketLabel(item.entity_type),
    color: slotColor(i),
  }));
  // A chart that is 100% "unclassified" carries no information; collapse it
  // into an explanatory empty state until real type labels exist.
  const allUnclassified =
    entityTypes.length > 0 && entityTypes.every((t) => t.bucket === "unclassified");
  const totalEntities = entityTypes.reduce((sum, t) => sum + t.count, 0);

  return (
    <div className="grid min-w-0 gap-4 xl:grid-cols-[repeat(3,minmax(0,1fr))]">
      <DataBars
        title="Categories"
        items={overview.categories}
        getLabel={(item) => item.category}
        getTone={(item) => `trust ${Number(item.avg_trust ?? 0).toFixed(2)}`}
        getValue={(item) => item.count}
        getColor={(item) => categoryColors.get(item.category) ?? "var(--hm-primary)"}
        header={
          <CompositionBar
            totalLabel="facts"
            segments={overview.categories.map((c) => ({
              key: c.category,
              label: c.category,
              value: c.count,
              color: categoryColors.get(c.category) ?? "var(--hm-primary)",
              detail: [
                { label: "avg trust", value: Number(c.avg_trust ?? 0).toFixed(2) },
              ],
            }))}
          />
        }
      />
      {allUnclassified ? (
        <div className="border border-border bg-background/30 p-3">
          <h3 className="mb-3 font-mondwest text-display text-xs tracking-[0.12em] text-text-secondary">
            Entity Types
          </h3>
          <p className="text-xs leading-relaxed text-text-tertiary">
            <span className="font-mono-ui text-text-secondary">
              {totalEntities}
            </span>{" "}
            linked {totalEntities === 1 ? "entity" : "entities"} — none have a
            type label yet, so there is no breakdown to chart.
          </p>
          <p className="mt-2 text-xs leading-relaxed text-text-tertiary">
            Type breakdowns appear here once entities are classified (for
            example by a curator&apos;s entity-classification pass).
          </p>
        </div>
      ) : (
        <DataBars
          title="Entity Types"
          items={entityTypes}
          getLabel={(item) => item.bucket}
          getValue={(item) => item.count}
          getColor={(item) => item.color}
          header={
            <CompositionBar
              totalLabel="entities"
              segments={entityTypes.map((t) => ({
                key: t.bucket,
                label: t.bucket,
                value: t.count,
                color: t.color,
              }))}
            />
          }
        />
      )}
      <HrrCoveragePanel data={data} />
    </div>
  );
}

function HolographicView({
  data,
  query,
  refreshing,
  setQuery,
  onApplied,
}: {
  data: MemoryDashboardResponse;
  query: string;
  refreshing: boolean;
  setQuery: (value: string) => void;
  onApplied?: () => void;
}) {
  const overview = data.holographic.overview;
  type ViewKey = "inspector" | "map" | "graph" | "similarity" | "curation";
  const [view, setViewState] = useState<ViewKey>(() => {
    const initial = new URLSearchParams(window.location.search).get("view");
    return initial === "map" ||
      initial === "graph" ||
      initial === "similarity" ||
      initial === "curation"
      ? initial
      : "inspector";
  });

  // Keep the ?view= deep link in sync so a reload stays where the user is.
  const setView = useCallback((next: ViewKey) => {
    setViewState(next);
    const url = new URL(window.location.href);
    if (next === "inspector") url.searchParams.delete("view");
    else url.searchParams.set("view", next);
    window.history.replaceState(null, "", url);
  }, []);

  // Cross-view navigation: a similarity pair jumps to the Semantic Map with
  // both facts selected and the first one pinned.
  const [mapFocus, setMapFocus] = useState<SemanticMapFocus | null>(null);
  const showPairOnMap = useCallback(
    (pair: MemorySimilarityPair) => {
      setMapFocus({
        ids: [pair.a_id, pair.b_id],
        pinId: pair.a_id,
        token: Date.now(),
      });
      setView("map");
    },
    [setView],
  );

  const VIEW_TABS: Array<{ key: ViewKey; label: string; icon: ReactNode }> = [
    { key: "inspector", label: "Inspector", icon: <Table2 className="h-3.5 w-3.5" /> },
    { key: "map", label: "Semantic Map", icon: <Sparkles className="h-3.5 w-3.5" /> },
    { key: "graph", label: "Graph", icon: <Network className="h-3.5 w-3.5" /> },
    { key: "similarity", label: "Similarity", icon: <Copy className="h-3.5 w-3.5" /> },
    { key: "curation", label: "Curation", icon: <Wand2 className="h-3.5 w-3.5" /> },
  ];

  return (
    <div className="flex min-w-0 w-full max-w-full flex-col gap-4">
      <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
        <SearchBox query={query} refreshing={refreshing} setQuery={setQuery} />
        <div className="hv-viewswitch flex shrink-0 max-w-full border border-border">
          {VIEW_TABS.map((tab) => (
            <Button
              key={tab.key}
              ghost={view !== tab.key}
              size="sm"
              className="gap-2 shrink-0"
              aria-current={view === tab.key ? "true" : undefined}
              onClick={() => setView(tab.key)}
            >
              {tab.icon}
              {tab.label}
            </Button>
          ))}
        </div>
      </div>
      <SystemStrip data={data} />

      {data.holographic.error && <PanelError error={data.holographic.error} />}

      {overview && (
        <>
          <div className="grid gap-3 sm:grid-cols-3">
            <Stat label="facts" value={overview.facts} />
            <Stat label="entities" value={overview.entities} />
            <Stat label="banks" value={overview.banks} />
          </div>
        </>
      )}

      {data.query && (
        <div className="text-xs text-text-tertiary" role="status" aria-live="polite">
          <span className="font-mono-ui text-foreground">
            {data.holographic.facts.length}
          </span>{" "}
          holographic match{data.holographic.facts.length === 1 ? "" : "es"} for{" "}
          <span className="font-mono-ui text-text-secondary">{data.query}</span>
        </div>
      )}

      {view === "map" ? (
        <SemanticMap query={query} focus={mapFocus} />
      ) : view === "graph" ? (
        <AssociationGraph graph={data.holographic.graph} />
      ) : view === "similarity" ? (
        <SimilarityPanel onShowOnMap={showPairOnMap} />
      ) : view === "curation" ? (
        <CurationPanel onApplied={onApplied} />
      ) : data.query ? (
        // While searching, results lead; the overview small-multiples follow.
        <>
          <div className="grid min-w-0 gap-4 xl:grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)]">
            <FactList data={data} />
            <EntityAndBankLists data={data} />
          </div>
          {overview && <OverviewBars overview={overview} data={data} />}
          {overview && (
            <div className="grid min-w-0 gap-4 xl:grid-cols-[repeat(2,minmax(0,1fr))]">
              <TrustDistribution data={data} />
              <GrowthSparkline data={data} />
            </div>
          )}
        </>
      ) : (
        <>
          {overview && <OverviewBars overview={overview} data={data} />}
          {overview && (
            <div className="grid min-w-0 gap-4 xl:grid-cols-[repeat(2,minmax(0,1fr))]">
              <TrustDistribution data={data} />
              <GrowthSparkline data={data} />
            </div>
          )}
          <div className="grid min-w-0 gap-4 xl:grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)]">
            <FactList data={data} />
            <EntityAndBankLists data={data} />
          </div>
        </>
      )}
    </div>
  );
}

export default function HolographicMemoryPage() {
  const [data, setData] = useState<MemoryDashboardResponse | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState("");
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const skipFirstQueryEffectRef = useRef(true);
  const loadSeqRef = useRef(0);

  const load = useCallback((q = query, quiet = false) => {
    if (quiet) setRefreshing(true);
    else setLoading(true);
    setError("");
    const seq = ++loadSeqRef.current;
    api
      .getMemoryDashboard({
        q,
        limit: INSPECTOR_ROW_LIMIT,
        graphLimit: GRAPH_ROW_LIMIT,
      })
      .then((resp) => {
        if (seq === loadSeqRef.current) setData(resp);
      })
      .catch((err) => {
        if (seq === loadSeqRef.current)
          setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (seq === loadSeqRef.current) {
          setLoading(false);
          setRefreshing(false);
        }
      });
  }, [query]);

  useEffect(() => {
    let cancelled = false;
    api
      .getMemoryDashboard({
        q: "",
        limit: INSPECTOR_ROW_LIMIT,
        graphLimit: GRAPH_ROW_LIMIT,
      })
      .then((resp) => {
        if (!cancelled) setData(resp);
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (skipFirstQueryEffectRef.current) {
      skipFirstQueryEffectRef.current = false;
      return;
    }
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => load(query, true), 300);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [load, query]);

  const content = useMemo(() => {
    if (!data) return null;
    return (
      <HolographicView
        data={data}
        query={query}
        refreshing={refreshing}
        setQuery={setQuery}
        onApplied={() => load(query, true)}
      />
    );
  }, [data, query, refreshing, load]);

  if (loading && !data) {
    return (
      <div className="py-24">
        <PanelLoading label="Loading holographic memory…" />
      </div>
    );
  }

  return (
    <div className="flex min-w-0 w-full max-w-full flex-col gap-4">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <BrainCircuit className="h-4 w-4 text-text-tertiary" />
          <h2 className="m-0 font-mondwest text-display text-xs tracking-[0.12em] text-text-tertiary">
            Plugin Inspector
          </h2>
        </div>
        <Button
          ghost
          size="icon"
          className="shrink-0 text-muted-foreground hover:text-foreground"
          disabled={loading || refreshing}
          onClick={() => load(query, true)}
          aria-label="Refresh Holographic Memory"
        >
          {refreshing ? <Spinner /> : <RefreshCw />}
        </Button>
      </div>

      {error && <PanelError error={error} onRetry={() => load(query, true)} />}

      {content}
    </div>
  );
}
