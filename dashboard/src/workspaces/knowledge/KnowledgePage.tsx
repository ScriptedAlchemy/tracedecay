import {
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import type { EChartsOption } from "echarts";
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from "../../ui/archetypes/ExplorerSplit.tsx";
import { ReadSection, envelopeReadState } from "../../ui/ReadSection.tsx";
import { FigureRail, Meter, Readout } from "../../ui/instrument.tsx";
import { SearchField } from "../../ui/search/SearchField.tsx";
import { Chart } from "../../viz/chart/Chart.tsx";
import { VirtualList } from "../../ui/VirtualList.tsx";
import { formatCount, splitCount } from "../../ui/format.ts";
import { cn } from "../../ui/cn";
import { envelopePayload, useEnvelope } from "../../data/query/useEnvelope.ts";
import { scopeKey, useScope } from "../../data/scope/store.ts";
import {
  type DashboardCoverageV1,
  type MemoryCategoryCountV1,
  MemoryFactDetailPayloadV1Schema,
  type MemoryFactRowV1,
  type MemoryFactsCoverageV1,
  MemoryOverviewPayloadV1Schema,
  type MemoryReadStatusV1,
  MemoryStatusPayloadV1Schema,
} from "../../contracts/generated.ts";
import { CurationConsole } from "./CurationConsole.tsx";
import { FactTrustHistory } from "./FactTrustHistory.tsx";
import { MemoryGeometry } from "./MemoryGeometry.tsx";
import { MemoryOplog } from "./MemoryOplog.tsx";
import {
  KNOWLEDGE_PANEL_ID,
  KnowledgeViewSwitcher,
  knowledgeTabId,
  knowledgeViewNote,
  useKnowledgeView,
  type KnowledgeViewKind,
} from "./KnowledgeViews.tsx";
import {
  composeTrustDistribution,
  factsBelow,
  summarizeLoadedTrust,
  trustSourceNote,
  type LoadedTrust,
  type TrustDistribution,
} from "./trust.ts";

const BASE = "/api/plugins/holographic";

/**
 * Knowledge — channel seven.
 *
 * Four camera positions over one memory store, in the order a reader descends
 * through it: the facts explorer, the phase geometry those facts sit in, the
 * daemon's automatic curation outcomes, and the store's own record
 * of what changed. `KnowledgeViews.tsx` owns the camera; each view owns its reads,
 * so a position is paid for only when it is looked at.
 *
 * Everything the daemon mounts for holographic memory is now consumed here.
 * Three of those routes are contracted (`/`, `/status`, `/fact/{id}`) and read
 * through the generated schemas; the rest answer bare JSON and are read through
 * the house payload ladder with schemas written against their handlers — see
 * `data/query/memory.ts`, which explains why that split exists and what it
 * obliges.
 */
export function KnowledgePage() {
  const [view, selectView] = useKnowledgeView();
  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* `flex-wrap`, because the note under the camera is prose and the
       * switcher is four 44px targets: held on one row they laid the note
       * past the right edge at 320 CSS px and at 400% zoom. */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-edge-subtle bg-surface-1 px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Knowledge</h1>
        <KnowledgeViewSwitcher active={view} onSelect={selectView} />
        <p className="min-w-0 text-2xs text-text-muted">
          {knowledgeViewNote(view)}
        </p>
      </div>
      {/* The element `aria-controls` names, present for as long as the switcher
       * is — a reference to an element that was never drawn is an invalid one,
       * which is what the accessibility gate reads it as. */}
      <div
        id={KNOWLEDGE_PANEL_ID}
        role="tabpanel"
        aria-labelledby={knowledgeTabId(view)}
        className="flex min-h-0 flex-1 flex-col"
      >
        <KnowledgeView kind={view} />
      </div>
    </div>
  );
}

/** The camera, applied. Exhaustive so a view added to the switcher cannot be
 * left without something to draw. */
function KnowledgeView({ kind }: { kind: KnowledgeViewKind }) {
  switch (kind) {
    case "facts":
      return <KnowledgeFacts />;
    case "geometry":
      return <MemoryGeometry />;
    case "curation":
      return <CurationConsole />;
    case "oplog":
      return <MemoryOplog />;
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** The facts explorer: memory facts with trust as the primary visual axis,
 * entity summary, and fact drill-down. The semantic WebGL map is the phase-2
 * canvas per the visualization catalog. */
function KnowledgeFacts() {
  const scope = useScope((state) => state.scope);
  const currentScopeKey = scopeKey(scope);
  const [query, setQuery] = useState("");
  const [applied, setApplied] = useState("");
  const overview = useEnvelope(
    ["memory", "overview", applied],
    `${BASE}/?limit=100${applied ? `&q=${encodeURIComponent(applied)}` : ""}`,
    MemoryOverviewPayloadV1Schema,
  );
  // The overview histogram is the finest canonical store-wide distribution.
  // Status contributes the current four-band authority when the histogram is
  // empty, so an empty store stays distinct from a failed reading.
  const status = useEnvelope(
    ["memory", "status"],
    `${BASE}/status`,
    MemoryStatusPayloadV1Schema,
  );
  const statusMemory = envelopePayload(status.data)?.memory;
  const overviewData = envelopePayload(overview.data);
  // One distribution for the two plates that draw it.
  //
  // The rail and the list are separate boundaries on purpose — a failed read
  // has to be reported in both panes rather than leaving one a hollow shell —
  // but they are the same read, and each was composing the distribution from
  // the same three values written slightly differently. Two spellings of one
  // computation is one place for them to drift apart, which on this plate would
  // mean a rail and a list disagreeing about the trust of the same facts.
  const trust = composeTrustDistribution(
    overviewData?.holographic.overview?.trust_histogram,
    statusMemory,
    overviewData?.holographic.facts,
  );
  const [selection, setSelection] = useState<{
    scopeKey: string;
    fact: MemoryFactRowV1;
  } | null>(null);
  const selected =
    selection?.scopeKey === currentScopeKey ? selection.fact : null;
  const detail = useEnvelope(
    ["memory", "fact", String(selected?.fact_id ?? "")],
    `${BASE}/fact/${encodeURIComponent(String(selected?.fact_id ?? ""))}`,
    MemoryFactDetailPayloadV1Schema,
    { enabled: selected != null },
  );
  const selectedDetail = envelopePayload(detail.data)?.fact ?? selected;

  return (
    <ExplorerSplit
      filters={
        <>
          <ReadSection
            title="Memory"
            state={envelopeReadState(overview.isPending, overview.data, {
              loading: "loading memory overview",
              unknown: "memory overview has not answered",
            })}
            chrome="centered"
          >
            {(envelope) => {
              const data = envelope.payload;
              const stats = data.holographic.overview;
              // Ranked by count so the rail's length is a real ordering, not an
              // accident of whatever order the producer emitted rows in.
              const categories = [...(stats?.categories ?? [])].sort(
                (a, b) => b.count - a.count,
              );
              const categoryCeiling = categories.reduce(
                (max, row) => Math.max(max, row.count),
                0,
              );
              const factCount = splitCount(stats?.facts);
              const entityCount = splitCount(stats?.entities);
              const growth = stats?.growth ?? [];
              return (
                <div className="flex flex-col gap-3">
                  <SearchField
                    value={query}
                    onChange={setQuery}
                    onSubmit={() => setApplied(query.trim())}
                    onClear={() => {
                      setQuery("");
                      setApplied("");
                    }}
                    label="Search facts"
                    placeholder="Search facts"
                    hint="press / to focus, Esc to clear"
                    submitted={applied}
                  />
                  {/* The rail used to be a 2×1 grid of equal tiles whose 26px
                   * numerals overflowed their own cells — 41,204 facts rendered
                   * as the string "41…". Facts is the quantity this workspace
                   * exists to report, so it takes the display tier and the whole
                   * rail width in the compact magnitude language; the supporting
                   * counts sit under it on one shared bezel. */}
                  <div className="flex flex-col">
                    <div className="td-raised border border-edge-subtle px-3 py-3">
                      <Readout
                        label="facts"
                        size="xl"
                        value={factCount.value}
                        unit={factCount.unit}
                        note={
                          stats?.facts != null
                            ? `${stats.facts.toLocaleString()} recorded`
                            : undefined
                        }
                      />
                    </div>
                    <div className="flex border-x border-b border-edge-subtle bg-surface-1">
                      <div className="min-w-0 flex-1 px-3 py-2">
                        <Readout
                          label="entities"
                          size="sm"
                          value={entityCount.value}
                          unit={entityCount.unit}
                        />
                      </div>
                    </div>
                  </div>
                  <TrustDistributionPlate distribution={trust} />
                  {statusMemory ? (
                    <figure className="flex flex-col gap-1.5">
                      <figcaption className="td-legend">memory algebra</figcaption>
                      <p className="text-2xs text-text-secondary">
                        {statusMemory.algebra.name}
                      </p>
                      <p className="text-3xs text-text-muted">
                        {statusMemory.algebra.hrr_dim.toLocaleString()} dimensions
                        {" · "}
                        estimated capacity {statusMemory.algebra.estimated_capacity.toLocaleString()}
                      </p>
                      <p className="text-3xs text-text-muted">
                        {statusMemory.feedback_funnel.rated_fact_count.toLocaleString()} rated of{" "}
                        {statusMemory.feedback_funnel.retrieved_fact_count.toLocaleString()} retrieved
                        {" · "}
                        {statusMemory.feedback_funnel.feedback_total.toLocaleString()} feedback events
                      </p>
                    </figure>
                  ) : null}
                  {categories.length > 0 ? (
                    <figure className="flex flex-col gap-2">
                      <figcaption className="td-legend">
                        facts by category
                      </figcaption>
                      <div className="flex flex-col gap-2">
                        {categories.map((row) => (
                          <CategoryBar
                            key={row.category}
                            row={row}
                            ceiling={categoryCeiling}
                          />
                        ))}
                      </div>
                    </figure>
                  ) : null}
                  {growth.length > 0 ? <GrowthChart growth={growth} /> : null}
                </div>
              );
            }}
          </ReadSection>
        </>
      }
      list={
        <ReadSection
          title="Facts"
          state={envelopeReadState(overview.isPending, overview.data, {
            loading: "loading facts",
            unknown: "memory facts have not answered",
          })}
          chrome="centered"
        >
          {(envelope) => {
            const data = envelope.payload;
            const facts = data.holographic.facts ?? [];
            const factsRead = data.holographic.reads?.facts;
            const graphRead = data.holographic.reads?.graph;
            const factsComplete =
              data.holographic.facts_coverage.completeness === "complete" &&
              (factsRead?.state === "ready" ||
                factsRead?.state === "complete_zero_findings");
            const coverageNotice = (
              <MemoryCoverageNotices
                factsCoverage={data.holographic.facts_coverage}
                factsRead={factsRead}
                graphCoverage={data.holographic.graph.coverage}
                graphRead={graphRead}
              />
            );
            if (data.holographic.error) {
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  memory store unavailable: {data.holographic.error}
                </p>
              );
            }
            if (
              factsRead &&
              factsRead.state !== "ready" &&
              factsRead.state !== "partial" &&
              factsRead.state !== "complete_zero_findings"
            ) {
              return (
                <p
                  role="status"
                  data-state={factsRead.state}
                  className="p-6 text-center text-sm text-state-error"
                >
                  Fact list read is {factsRead.state.replaceAll("_", " ")}
                  {factsRead.error ? `: ${factsRead.error}` : "."}
                </p>
              );
            }
            if (facts.length === 0) {
              return (
                <div className="flex flex-col">
                  {coverageNotice}
                  <p className="p-6 text-center text-sm text-text-muted">
                    {applied
                      ? `no loaded facts match “${applied}”`
                      : factsComplete
                        ? "no facts recorded"
                        : "no facts were returned by this incomplete read"}
                  </p>
                </div>
              );
            }
            // Recall counts have no absolute ceiling, so the rail is scaled to
            // the busiest fact actually on screen. That makes the column a
            // ranking of what is loaded — which is what it is — rather than an
            // implied fraction of some total the daemon never reported.
            const recallCeiling = facts.reduce(
              (max, fact) => Math.max(max, fact.retrieval_count ?? 0),
              0,
            );
            const loaded = summarizeLoadedTrust(facts);
            return (
              <FactList
                facts={facts}
                coverageNotice={coverageNotice}
                recallCeiling={recallCeiling}
                loaded={loaded}
                distribution={trust}
                query={applied}
                selected={selected}
                onSelect={(fact) =>
                  setSelection({ scopeKey: currentScopeKey, fact })
                }
              />
            );
          }}
        </ReadSection>
      }
      inspector={
        selectedDetail ? (
          <InspectorPanel title="Fact" onClose={() => setSelection(null)}>
            <div className="flex flex-col gap-3">
              {detail.isPending ? (
                <p className="text-2xs text-text-muted">
                  Loading canonical fact detail…
                </p>
              ) : detail.data?.outcome !== "envelope" ? (
                <p className="text-2xs text-state-partial">
                  Canonical detail is unavailable; this is the bounded overview
                  row.
                </p>
              ) : null}
              {selectedDetail.trust_score == null ? (
                <p className="text-2xs text-text-muted">
                  trust unavailable while payload access is {selectedDetail.payload_access}
                </p>
              ) : (
                <TrustGauge score={selectedDetail.trust_score} />
              )}
              {selectedDetail.content ? (
                <p className="whitespace-pre-wrap text-xs leading-relaxed">
                  {selectedDetail.content}
                </p>
              ) : null}
              <FeedbackSplit
                helpful={selectedDetail.helpful_count ?? null}
                unhelpful={selectedDetail.unhelpful_count ?? null}
              />
              <KeyValueTree
                value={Object.fromEntries(
                  Object.entries(selectedDetail).filter(
                    ([k]) => k !== "content",
                  ),
                )}
              />
              {/* The gauge and the split above are terminal figures: where the
               * score landed, not how. The audit says how, and it is the one
               * reading on this inspector that can report its own
               * incompleteness. */}
              <FactTrustHistory factId={selectedDetail.fact_id ?? null} />
            </div>
          </InspectorPanel>
        ) : undefined
      }
    />
  );
}

function MemoryCoverageNotices({
  factsCoverage,
  factsRead,
  graphCoverage,
  graphRead,
}: {
  factsCoverage: MemoryFactsCoverageV1;
  factsRead: MemoryReadStatusV1 | undefined;
  graphCoverage: DashboardCoverageV1;
  graphRead: MemoryReadStatusV1 | undefined;
}) {
  const factsIncomplete =
    factsCoverage.completeness !== "complete" || factsRead?.state === "partial";
  const graphReadComplete =
    graphRead?.state === "ready" ||
    graphRead?.state === "complete_zero_findings";
  const graphIncomplete =
    graphCoverage.completeness !== "complete" || !graphReadComplete;
  if (!factsIncomplete && !graphIncomplete) return null;
  const graphReset = graphRead?.code === "graph_reset_required";
  return (
    <div className="flex flex-col gap-1 border-b border-edge-subtle px-3 py-2 text-2xs leading-relaxed">
      {factsIncomplete ? (
        <p role="status" data-state={factsRead?.state ?? "partial"} className="text-state-partial">
          {factsRead?.state === "partial"
            ? `Fact read is partial; reported fact coverage is ${factsCoverage.completeness}`
            : `Fact coverage is ${factsCoverage.completeness}`}
          ; this read was bounded to at most{" "}
          {factsCoverage.limit.toLocaleString()} facts.
        </p>
      ) : null}
      {graphReset ? (
        <p role="status" data-state="error" className="text-state-error">
          Memory graph reset required
          {graphRead.error ? `: ${graphRead.error}` : "."}
        </p>
      ) : null}
      {graphIncomplete ? (
        <p role="status" data-state={graphRead?.state ?? "unknown"} className="text-state-partial">
          Memory graph coverage is {graphCoverage.completeness}
          {graphCoverage.omission_reasons.length > 0
            ? `; omissions: ${graphCoverage.omission_reasons.join(", ")}.`
            : "."}
        </p>
      ) : null}
    </div>
  );
}

function GrowthChart({
  growth,
}: {
  growth: readonly { date: string; cumulative_facts: number }[];
}) {
  const option = useMemo<EChartsOption>(
    () => ({
      xAxis: {
        type: "category",
        data: growth.map((point) => point.date),
        axisLabel: { show: false },
        axisTick: { show: false },
      },
      yAxis: { type: "value", axisLabel: { show: false } },
      grid: { left: 2, right: 2, top: 6, bottom: 2, containLabel: true },
      series: [
        {
          type: "line",
          showSymbol: false,
          smooth: true,
          areaStyle: {},
          data: growth.map((point) => point.cumulative_facts),
        },
      ],
    }),
    [growth],
  );
  const first = growth[0];
  const last = growth.at(-1);
  if (!first || !last) return null;
  return (
    <figure className="flex flex-col gap-1.5">
      <figcaption className="td-legend">growth</figcaption>
      {/* Same axis-elision as trust distribution: twelve weekly dates at 9px in
       * a 224px rail is unreadable debris, so the shape carries the trend and
       * the two endpoints are printed directly underneath instead of a rotated,
       * truncated axis. */}
      <Chart
        ariaLabel={`Cumulative facts recorded across ${growth.length} periods, from ${first.date} (${first.cumulative_facts.toLocaleString()} facts) to ${last.date} (${last.cumulative_facts.toLocaleString()} facts)`}
        height={70}
        option={option}
      />
      {/* Date-over-value, the same shape as every other Readout on this page,
       * rather than one cramped line — "MAY 8 · 3,200" beside its mirror at
       * the opposite edge had nowhere to go in a 224px rail and truncated into
       * the gap between them. */}
      <div
        aria-hidden
        className="flex items-start justify-between gap-2 border-t border-edge-subtle pt-1.5"
      >
        <Readout
          label={formatShortDate(first.date)}
          value={formatCount(first.cumulative_facts)}
          size="sm"
        />
        <Readout
          label={formatShortDate(last.date)}
          value={formatCount(last.cumulative_facts)}
          size="sm"
          align="right"
        />
      </div>
    </figure>
  );
}

/**
 * The trust distribution, or a statement of why there is nothing to draw.
 *
 * `composeTrustDistribution` takes the finest canonical source that carries
 * mass, and this plate prints which one it used. When the mass all lands in a
 * single band there is no shape to draw, so the reading is stated instead —
 * one full bar beside nine empty ones is the same non-information in a more
 * confident costume.
 */
function TrustDistributionPlate({
  distribution,
}: {
  distribution: TrustDistribution;
}) {
  if (distribution.source === "none") {
    return (
      <figure className="flex flex-col gap-1">
        <figcaption className="td-legend">trust distribution</figcaption>
        <p className="text-2xs leading-relaxed text-text-muted">
          The store reported no trust distribution — not a distribution of zero,
          but no reading at all.
        </p>
      </figure>
    );
  }
  const occupied = distribution.bands.filter((band) => band.count > 0);
  if (distribution.degenerate) {
    const only = occupied[0]!;
    return (
      <figure className="flex flex-col gap-1">
        <figcaption className="td-legend">trust distribution</figcaption>
        <p className="text-2xs leading-relaxed text-text-secondary">
          All {distribution.total.toLocaleString()} facts sit in one band,{" "}
          <span className="td-value text-text-primary">{only.label}</span>.
          There is no spread to draw.
        </p>
        <p className="text-3xs text-text-muted">
          {trustSourceNote(distribution.source)}
        </p>
      </figure>
    );
  }
  const ceiling = distribution.bands.reduce(
    (max, band) => Math.max(max, band.count),
    0,
  );
  return (
    <figure className="flex flex-col gap-1.5">
      <figcaption className="td-legend">trust distribution</figcaption>
      <div className="flex flex-col gap-1">
        {distribution.bands.map((band) => (
          <div key={band.label} className="flex items-center gap-2">
            <span
              className="td-value w-16 shrink-0 text-3xs text-text-muted"
              data-cell="numeric"
            >
              {band.label}
            </span>
            <Meter
              fraction={ceiling > 0 ? band.count / ceiling : null}
              className="min-w-0 flex-1"
              tone={band.count === 0 ? "bg-transparent" : undefined}
            />
            <span
              className={cn(
                "td-value w-8 shrink-0 text-right text-3xs",
                band.count === 0 ? "text-text-muted" : "text-text-secondary",
              )}
              data-cell="numeric"
            >
              {band.count.toLocaleString()}
            </span>
          </div>
        ))}
      </div>
      {/* Bands with no facts keep their row and print their zero: an absent
       * band drawn as a missing row would read as a narrower scale than the
       * one actually measured. */}
      <figcaption className="text-3xs leading-relaxed text-text-muted">
        {distribution.total.toLocaleString()} facts across{" "}
        {distribution.bands.length} bands, {distribution.occupied} of them
        occupied · {trustSourceNote(distribution.source)}
      </figcaption>
    </figure>
  );
}

/**
 * What the loaded slice of facts is, stated above the rows.
 *
 * The list is a top-100 slice ordered so the highest-trust facts fill it. A
 * reader scrolling ninety-six rows that all read 1.00 will conclude the store
 * has no low-trust facts; the store in fact holds twenty-one below 0.75 that
 * this slice never reaches. That is not a detail — it is the difference
 * between "feedback never moves a score" and "you are looking at the top of
 * the list".
 */
function FactListHeader({
  loaded,
  distribution,
  query,
}: {
  loaded: LoadedTrust;
  distribution: TrustDistribution;
  query: string;
}) {
  const measuredRange =
    loaded.min != null && loaded.max != null
      ? { min: loaded.min, max: loaded.max }
      : null;
  const unreached = measuredRange
    ? factsBelow(distribution, measuredRange.min)
    : null;
  const sameEverywhere = measuredRange?.min === measuredRange?.max;
  return (
    <div className="flex flex-col gap-0.5 border-b border-edge-subtle px-3 py-2">
      <p className="td-legend">
        {loaded.total.toLocaleString()} facts loaded · {loaded.measured.toLocaleString()} with trust ·{" "}
        {loaded.unavailable.toLocaleString()} unavailable
        {query ? ` · matching “${query}”` : ""}
      </p>
      <p className="text-2xs leading-relaxed text-text-muted">
        {measuredRange == null
          ? "No loaded fact exposes a trust measurement."
          : sameEverywhere
            ? `Every measured fact is at trust ${measuredRange.max.toFixed(2)}.`
            : `Trust ${measuredRange.min.toFixed(2)}–${measuredRange.max.toFixed(2)}, with ${loaded.atMax.toLocaleString()} at exactly ${measuredRange.max.toFixed(2)}.`}
        {measuredRange && unreached != null && unreached > 0
          ? ` The store holds ${unreached.toLocaleString()} further facts below ${measuredRange.min.toFixed(2)} that this slice does not reach.`
          : ""}
      </p>
    </div>
  );
}

/** Two lines of a fact's content, which is the height a 56px row can carry.
 * A row taller than this stops being a list; a row shorter than this shows a
 * ninety-character prefix of a nineteen-hundred-character fact. */
export const FACT_ROW_HEIGHT = 56;

const FACT_SUMMARY_CHARACTER_SAMPLE = "abcdefghijklmnopqrstuvwxyz";

function FactList({
  facts,
  coverageNotice,
  recallCeiling,
  loaded,
  distribution,
  query,
  selected,
  onSelect,
}: {
  facts: MemoryFactRowV1[];
  coverageNotice: ReactNode;
  recallCeiling: number;
  loaded: LoadedTrust | null;
  distribution: TrustDistribution;
  query: string;
  selected: MemoryFactRowV1 | null;
  onSelect: (fact: MemoryFactRowV1) => void;
}) {
  const listRootRef = useRef<HTMLDivElement>(null);
  const summaryProbeRef = useRef<HTMLSpanElement>(null);
  const characterProbeRef = useRef<HTMLSpanElement>(null);
  const characterLimit = useFactSummaryCharacterLimit(
    listRootRef,
    summaryProbeRef,
    characterProbeRef,
  );
  return (
    <div ref={listRootRef} className="relative h-full">
      <div
        aria-hidden
        className="pointer-events-none invisible absolute inset-x-0 flex gap-3 px-3 pt-2"
      >
        <span className="w-14 shrink-0" />
        <span
          ref={summaryProbeRef}
          className="min-w-0 flex-1 text-xs leading-snug"
        >
          <span ref={characterProbeRef} className="whitespace-nowrap">
            {FACT_SUMMARY_CHARACTER_SAMPLE}
          </span>
        </span>
        <span className="hidden w-16 shrink-0 md:block" />
        <span className="hidden w-20 shrink-0 md:block" />
      </div>
      <VirtualList
        items={facts}
        getKey={(fact) => String(fact.fact_id)}
        estimateHeight={FACT_ROW_HEIGHT}
        header={
          coverageNotice || loaded ? (
            <>
              {coverageNotice}
              {loaded ? (
                <FactListHeader
                  loaded={loaded}
                  distribution={distribution}
                  query={query}
                />
              ) : null}
            </>
          ) : null
        }
        renderItem={(fact) => (
          <FactListRow
            fact={fact}
            recallCeiling={recallCeiling}
            // A rail scaled 0-1 across a slice whose trust never leaves the top
            // tenth is the same length on every row: not a ranking, just ink.
            // The header states the slice's spread instead, and the printed
            // figure keeps the precision.
            showTrustRail={loaded ? !loaded.flat : true}
            characterLimit={characterLimit}
            selected={selected?.fact_id === fact.fact_id}
            onSelect={() => onSelect(fact)}
          />
        )}
      />
    </div>
  );
}

/** One fact, read as two ranked quantities and as much of the fact as fits.
 *
 * The row previously spent forty pixels on a hairline trust bar with no
 * number, then printed the recall count as plain grey text — so a column of
 * facts carried no visible ordering at all and the two measurements that
 * define this product (how much a memory is trusted, how often it is
 * reinforced) were the least legible things on the row. Both now get a printed
 * figure AND a length: the digits for precision, the rail for ranking.
 *
 * Two further problems the real store exposed. Facts here run to nearly two
 * thousand characters on ONE line, so a single-line row truncated every
 * interesting fact at its first clause and the reader had no way to tell a
 * clipped row from a short one. The summary now clamps to two lines, carries
 * the full text on `title`, and prints an explicit control on any row that is
 * still cut — the control opens the same inspector the row does, where the
 * content is shown in full. And the trust rail is suppressed when the loaded
 * slice has no spread: ninety-six rails all drawn at 90-100% of their track
 * are ninety-six copies of one length.
 */
function FactListRow({
  fact,
  recallCeiling,
  showTrustRail,
  characterLimit,
  selected,
  onSelect,
}: {
  fact: MemoryFactRowV1;
  recallCeiling: number;
  showTrustRail: boolean;
  characterLimit: number | null;
  selected: boolean;
  onSelect: () => void;
}) {
  const content = fact.content ?? String(fact.fact_id);
  const summary = useMemo(() => content.split("\n")[0] ?? "", [content]);
  const clipped = characterLimit !== null && content.length > characterLimit;
  const trust =
    typeof fact.trust_score === "number"
      ? Math.max(0, Math.min(fact.trust_score, 1))
      : null;
  const recalls = fact.retrieval_count ?? 0;
  return (
    <DataRow
      selected={selected}
      onSelect={onSelect}
      height={FACT_ROW_HEIGHT}
      align="start"
    >
      <span className="flex w-14 shrink-0 flex-col gap-1">
        <span
          className={cn(
            "td-value text-2xs leading-none",
            trust == null
              ? "text-text-muted"
              : trust >= 0.7
              ? "text-text-primary"
              : trust >= 0.4
                ? "text-text-secondary"
                : "text-text-muted",
          )}
          data-cell="numeric"
        >
          {trust == null ? "—" : trust.toFixed(2)}
        </span>
        {showTrustRail && trust != null ? (
          <Meter
            fraction={trust}
            height="row"
            tone={
              trust >= 0.7
                ? "bg-accent"
                : trust >= 0.4
                  ? "bg-accent/60"
                  : "bg-accent/30"
            }
          />
        ) : null}
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span
          className="line-clamp-2 leading-snug text-text-primary"
          title={content}
        >
          {summary}
        </span>
        {clipped ? (
          <span className="td-legend text-accent">
            {content.length.toLocaleString()} chars · open for the rest
          </span>
        ) : null}
      </span>
      {/* Column priority under 768px: the fact itself and how far it can be
       * trusted are the row. Category and recall count are what a narrow
       * viewport gives up -- keeping all four columns crushed the summary to
       * three glyphs, which is not density, just damage. */}
      {fact.category ? (
        <span className="td-legend shrink-0 border border-edge-subtle px-1.5 py-1 max-md:hidden">
          {fact.category}
        </span>
      ) : null}
      <FigureRail
        value={recalls}
        unit="rc"
        fraction={recallCeiling > 0 ? recalls / recallCeiling : null}
        tone="bg-text-muted"
        className="max-md:hidden"
      />
    </DataRow>
  );
}

/** One width calibration for the scroll region replaces per-row layout reads. */
function useFactSummaryCharacterLimit(
  listRootRef: RefObject<HTMLDivElement | null>,
  summaryProbeRef: RefObject<HTMLSpanElement | null>,
  characterProbeRef: RefObject<HTMLSpanElement | null>,
): number | null {
  const [characterLimit, setCharacterLimit] = useState<number | null>(null);
  useLayoutEffect(() => {
    const scrollContainer = listRootRef.current?.parentElement;
    const summaryProbe = summaryProbeRef.current;
    const characterProbe = characterProbeRef.current;
    if (!scrollContainer || !summaryProbe || !characterProbe) return;
    const measure = () => {
      const characterWidth =
        characterProbe.getBoundingClientRect().width /
        FACT_SUMMARY_CHARACTER_SAMPLE.length;
      const charactersPerLine =
        characterWidth > 0
          ? Math.floor(summaryProbe.clientWidth / characterWidth)
          : 0;
      const next = charactersPerLine > 0 ? charactersPerLine * 2 : null;
      setCharacterLimit((previous) => (previous === next ? previous : next));
    };
    measure();
    if (typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(measure);
    observer.observe(scrollContainer);
    return () => observer.disconnect();
  }, [characterProbeRef, listRootRef, summaryProbeRef]);
  return characterLimit;
}

/** "2026-05-08" -> "May 8". The growth caption prints a date beside a
 * facts count in a 224px rail; the full ISO stamp alone (10 chars) leaves no
 * room for the count next to it before the two end labels collide. The full
 * date stays in the chart's `ariaLabel` — this is a display-only compaction,
 * not a different value. */
function formatShortDate(iso: string): string {
  const date = new Date(`${iso}T00:00:00Z`);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    timeZone: "UTC",
  });
}

/** One category's share of the loaded fact set, read the same way the fact
 * list itself is: a printed count for precision, a rail scaled to the busiest
 * category on screen for ranking. No fabricated denominator — the rail
 * measures against the largest category actually present, not an assumed
 * total. */
function CategoryBar({
  row,
  ceiling,
}: {
  row: MemoryCategoryCountV1;
  ceiling: number;
}) {
  const fraction = ceiling > 0 ? row.count / ceiling : null;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 truncate text-2xs text-text-secondary">
          {row.category}
        </span>
        <span
          className="td-value text-2xs text-text-primary"
          data-cell="numeric"
        >
          {formatCount(row.count)}
        </span>
      </div>
      <Meter
        fraction={fraction}
        ariaLabel={`${row.category}: ${row.count.toLocaleString()} facts`}
      />
    </div>
  );
}

/** Trust as what it is: one measured quantity on the 0–1 scale, printed and
 * given the same length every other readout in the product uses. */
function TrustGauge({ score }: { score: number }) {
  const clamped = Math.max(0, Math.min(score, 1));
  return (
    <Readout
      label="trust"
      size="lg"
      value={clamped.toFixed(2)}
      fraction={clamped}
    />
  );
}

/** Helpful vs unhelpful feedback as one proportional split bar. */
export function FeedbackSplit({
  helpful,
  unhelpful,
}: {
  helpful: number | null;
  unhelpful: number | null;
}) {
  if (helpful === null || unhelpful === null) {
    return (
      <p className="text-2xs text-text-muted">feedback counts not reported</p>
    );
  }
  const total = helpful + unhelpful;
  if (total === 0) {
    return <p className="text-2xs text-text-muted">no feedback recorded</p>;
  }
  return (
    <figure className="flex flex-col gap-1">
      <div className="flex h-1.5 overflow-hidden rounded-full bg-surface-3">
        <div
          className="bg-accent"
          style={{ width: `${(helpful / total) * 100}%` }}
        />
        <div
          className="bg-state-stale"
          style={{ width: `${(unhelpful / total) * 100}%` }}
        />
      </div>
      <figcaption className="tabular text-2xs text-text-muted">
        {helpful} helpful · {unhelpful} unhelpful
      </figcaption>
    </figure>
  );
}
