import { useLayoutEffect, useMemo, useRef, useState, type RefObject } from 'react';
import type { EChartsOption } from 'echarts';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary } from '../../ui/ReadSection.tsx';
import { FigureRail, Meter, Readout } from '../../ui/instrument.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { Chart } from '../../viz/chart/Chart.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { formatCount, splitCount } from '../../ui/format.ts';
import { cn } from '../../ui/cn';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  type MemoryCategoryCountV1,
  MemoryFactDetailPayloadV1Schema,
  type MemoryFactRowV1,
  type MemoryHrrCoverageV1,
  MemoryOverviewPayloadV1Schema,
  MemoryStatusPayloadV1Schema,
} from '../../contracts/generated.ts';
import {
  composeTrustDistribution,
  factsBelow,
  hrrStatusLabel,
  summarizeHrrCoverage,
  summarizeLoadedTrust,
  trustSourceNote,
  type LoadedTrust,
  type TrustDistribution,
} from './trust.ts';

const BASE = '/api/plugins/holographic';

/** Knowledge: memory facts with trust as the primary visual axis, entity
 * summary, and fact drill-down. The semantic WebGL map is the phase-2 canvas
 * per the visualization catalog. */
export function KnowledgePage() {
  const [query, setQuery] = useState('');
  const [applied, setApplied] = useState('');
  const overview = useLegacy(
    ['memory', 'overview', applied],
    `${BASE}/?limit=100${applied ? `&q=${encodeURIComponent(applied)}` : ''}`,
    MemoryOverviewPayloadV1Schema,
  );
  // The overview's own `trust_histogram` comes back all-zero against a real
  // store (see trust.ts). This route reports the same distribution in four
  // coarser bands and is correct, so it is read as the fallback source rather
  // than leaving the plate empty. Cheap — ~0.1s against a live daemon.
  const status = useLegacy(['memory', 'status'], `${BASE}/status`, MemoryStatusPayloadV1Schema);
  const statusBands =
    status.data?.outcome === 'ok' ? status.data.data.memory : undefined;
  const overviewData = overview.data?.outcome === 'ok' ? overview.data.data : undefined;
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
    statusBands,
    overviewData?.holographic.facts,
  );
  const [selected, setSelected] = useState<MemoryFactRowV1 | null>(null);
  const detail = useLegacy(
    ['memory', 'fact', String(selected?.fact_id ?? '')],
    `${BASE}/fact/${encodeURIComponent(String(selected?.fact_id ?? ''))}`,
    MemoryFactDetailPayloadV1Schema,
    { enabled: selected != null },
  );
  const selectedDetail =
    detail.data?.outcome === 'ok' && detail.data.data.fact ? detail.data.data.fact : selected;

  return (
    <ExplorerSplit
      header={
        <div className="border-b border-edge-subtle bg-surface-1 px-4 py-2">
          <h1 className="text-sm font-semibold tracking-tight">Knowledge</h1>
        </div>
      }
      filters={
        <LegacyBoundary
          title="Memory"
          pending={overview.isPending}
          result={overview.data}
        >
          {(data) => {
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
                    setQuery('');
                    setApplied('');
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
                    {stats?.banks != null ? (
                      <div className="min-w-0 flex-1 border-l border-edge-subtle px-3 py-2">
                        <Readout label="banks" size="sm" value={stats.banks} />
                      </div>
                    ) : null}
                  </div>
                </div>
                <TrustDistributionPlate distribution={trust} />
                {categories.length > 0 ? (
                  <figure className="flex flex-col gap-2">
                    <figcaption className="td-legend">facts by category</figcaption>
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
                <HrrCoveragePlate rows={stats?.hrr_coverage ?? []} />
                {growth.length > 0 ? <GrowthChart growth={growth} /> : null}
              </div>
            );
          }}
        </LegacyBoundary>
      }
      list={
        <LegacyBoundary
          title="Facts"
          pending={overview.isPending}
          result={overview.data}
        >
          {(data) => {
            const facts = data.holographic.facts ?? [];
            const factsRead = data.holographic.reads?.facts;
            if (data.holographic.error) {
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  memory store unavailable: {data.holographic.error}
                </p>
              );
            }
            if (factsRead?.state === 'error') {
              return (
                <p role="status" className="p-6 text-center text-sm text-state-error">
                  Fact list read failed
                  {factsRead.error ? `: ${factsRead.error}` : '.'}
                </p>
              );
            }
            if (facts.length === 0) {
              const coverage = data.holographic.facts_coverage;
              const boundedQuery =
                applied && coverage?.query_applied_after_limit
                  ? `no match in the loaded top-${coverage.limit.toLocaleString()} slice for “${applied}”`
                  : null;
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  {boundedQuery ?? (applied ? `no loaded facts match “${applied}”` : 'no facts recorded')}
                </p>
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
                recallCeiling={recallCeiling}
                loaded={loaded}
                distribution={trust}
                query={applied}
                selected={selected}
                onSelect={setSelected}
              />
            );
          }}
        </LegacyBoundary>
      }
      inspector={
        selectedDetail ? (
          <InspectorPanel title="Fact" onClose={() => setSelected(null)}>
            <div className="flex flex-col gap-3">
              {detail.isPending ? (
                <p className="text-2xs text-text-muted">Loading canonical fact detail…</p>
              ) : detail.data?.outcome !== 'ok' ? (
                <p className="text-2xs text-state-partial">
                  Canonical detail is unavailable; this is the bounded overview row.
                </p>
              ) : null}
              <TrustGauge score={selectedDetail.trust_score} />
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
                  Object.entries(selectedDetail).filter(([k]) => k !== 'content'),
                )}
              />
            </div>
          </InspectorPanel>
        ) : undefined
      }
    />
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
        type: 'category',
        data: growth.map((point) => point.date),
        axisLabel: { show: false },
        axisTick: { show: false },
      },
      yAxis: { type: 'value', axisLabel: { show: false } },
      grid: { left: 2, right: 2, top: 6, bottom: 2, containLabel: true },
      series: [
        {
          type: 'line',
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
 * This plate used to render ten bars from `overview.trust_histogram` and, on
 * any real store, every one of those bars was zero — the producer names its
 * rows `trust-<n>` and the consumer parses them as bare integers, so no bucket
 * ever receives a count. An empty chart is not an honest empty state: it looks
 * like a store with no trust rather than a reading that failed to arrive.
 *
 * `composeTrustDistribution` therefore takes the first source that carries any
 * mass, and this plate prints which one it used. When the mass all lands in a
 * single band there is no shape to draw, so the reading is stated instead —
 * one full bar beside nine empty ones is the same non-information in a more
 * confident costume.
 */
function TrustDistributionPlate({ distribution }: { distribution: TrustDistribution }) {
  if (distribution.source === 'none') {
    return (
      <figure className="flex flex-col gap-1">
        <figcaption className="td-legend">trust distribution</figcaption>
        <p className="text-2xs leading-relaxed text-text-muted">
          The store reported no trust distribution — not a distribution of zero, but
          no reading at all.
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
          All {distribution.total.toLocaleString()} facts sit in one band,{' '}
          <span className="td-value text-text-primary">{only.label}</span>. There is no
          spread to draw.
        </p>
        <p className="text-3xs text-text-muted">{trustSourceNote(distribution.source)}</p>
      </figure>
    );
  }
  const ceiling = distribution.bands.reduce((max, band) => Math.max(max, band.count), 0);
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
              tone={band.count === 0 ? 'bg-transparent' : undefined}
            />
            <span
              className={cn(
                'td-value w-8 shrink-0 text-right text-3xs',
                band.count === 0 ? 'text-text-muted' : 'text-text-secondary',
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
        {distribution.total.toLocaleString()} facts across{' '}
        {distribution.bands.length} bands, {distribution.occupied} of them occupied ·{' '}
        {trustSourceNote(distribution.source)}
      </figcaption>
    </figure>
  );
}

/**
 * HRR coverage as one sentence and its exceptions.
 *
 * Six category bars all sitting between 96% and 100% is one fact drawn six
 * times, and it hides the reading that does vary — four of those six banks are
 * stale or incompletely vectorized, which no coverage percentage shows. The
 * uniformity is stated; only the banks that deviate get a row.
 */
function HrrCoveragePlate({ rows }: { rows: readonly MemoryHrrCoverageV1[] }) {
  const summary = summarizeHrrCoverage(rows);
  if (!summary) return null;
  return (
    <figure className="flex flex-col gap-1.5">
      <figcaption className="td-legend">HRR vector coverage</figcaption>
      <p className="text-2xs leading-relaxed text-text-secondary">{summary.line}</p>
      {summary.exceptions.length > 0 ? (
        <ul className="flex flex-col">
          {/* Two lines, not one. The longest status in the taxonomy is
            * "missing vectors"; beside a coverage figure in a 224px filter
            * rail it leaves under seventy pixels for the category name, which
            * clipped "decision" to "dec…" — the one word on the row a reader
            * actually needs. */}
          {summary.exceptions.map((row) => (
            <li
              key={row.category}
              className="flex flex-col gap-0.5 border-b border-edge-subtle py-1 last:border-b-0"
            >
              <span className="flex items-baseline gap-2">
                <span className="min-w-0 flex-1 truncate text-2xs text-text-primary">
                  {row.category}
                </span>
                {/* missing_bank has no bank to measure against, so a percentage
                  * would be a fabricated denominator; the status stands alone. */}
                {row.status !== 'missing_bank' ? (
                  <span
                    className="td-value shrink-0 text-3xs text-text-muted"
                    data-cell="numeric"
                  >
                    {Math.round(Math.max(0, Math.min(row.coverage, 1)) * 100)}% vectorized
                  </span>
                ) : null}
              </span>
              {/* `--raw-state-stale` measures 4.39:1 against the light
                * substrate at this 10px legend tier — under AA. The status is
                * already the only thing on the row that is not a number, and
                * only non-ready banks get a row at all, so the word carries the
                * signal without a hue that cannot be read. */}
              <span className="td-legend truncate text-text-primary">
                {hrrStatusLabel(row.status)}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
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
  const unreached = factsBelow(distribution, loaded.min);
  const sameEverywhere = loaded.min === loaded.max;
  return (
    <div className="flex flex-col gap-0.5 border-b border-edge-subtle px-3 py-2">
      <p className="td-legend">
        {loaded.count.toLocaleString()} facts loaded
        {query ? ` · matching “${query}”` : ''}
      </p>
      <p className="text-2xs leading-relaxed text-text-muted">
        {sameEverywhere
          ? `Every one is at trust ${loaded.max.toFixed(2)}.`
          : `Trust ${loaded.min.toFixed(2)}–${loaded.max.toFixed(2)}, with ${loaded.atMax.toLocaleString()} at exactly ${loaded.max.toFixed(2)}.`}
        {unreached != null && unreached > 0
          ? ` The store holds ${unreached.toLocaleString()} further facts below ${loaded.min.toFixed(2)} that this slice does not reach.`
          : ''}
      </p>
    </div>
  );
}

/** Two lines of a fact's content, which is the height a 56px row can carry.
 * A row taller than this stops being a list; a row shorter than this shows a
 * ninety-character prefix of a nineteen-hundred-character fact. */
export const FACT_ROW_HEIGHT = 56;

const FACT_SUMMARY_CHARACTER_SAMPLE = 'abcdefghijklmnopqrstuvwxyz';

function FactList({
  facts,
  recallCeiling,
  loaded,
  distribution,
  query,
  selected,
  onSelect,
}: {
  facts: MemoryFactRowV1[];
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
        <span ref={summaryProbeRef} className="min-w-0 flex-1 text-xs leading-snug">
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
          loaded ? (
            <FactListHeader loaded={loaded} distribution={distribution} query={query} />
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
  const summary = useMemo(() => content.split('\n')[0] ?? '', [content]);
  const clipped = characterLimit !== null && content.length > characterLimit;
  const trust = Math.max(0, Math.min(fact.trust_score, 1));
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
            'td-value text-2xs leading-none',
            trust >= 0.7
              ? 'text-text-primary'
              : trust >= 0.4
                ? 'text-text-secondary'
                : 'text-text-muted',
          )}
          data-cell="numeric"
        >
          {trust.toFixed(2)}
        </span>
        {showTrustRail ? (
          <Meter
            fraction={trust}
            height="row"
            tone={
              trust >= 0.7 ? 'bg-accent' : trust >= 0.4 ? 'bg-accent/60' : 'bg-accent/30'
            }
          />
        ) : null}
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="line-clamp-2 leading-snug text-text-primary" title={content}>
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
        characterProbe.getBoundingClientRect().width / FACT_SUMMARY_CHARACTER_SAMPLE.length;
      const charactersPerLine =
        characterWidth > 0 ? Math.floor(summaryProbe.clientWidth / characterWidth) : 0;
      const next = charactersPerLine > 0 ? charactersPerLine * 2 : null;
      setCharacterLimit((previous) => (previous === next ? previous : next));
    };
    measure();
    if (typeof ResizeObserver !== 'function') return;
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
  return date.toLocaleDateString('en-US', {
    month: 'short',
    day: 'numeric',
    timeZone: 'UTC',
  });
}

/** One category's share of the loaded fact set, read the same way the fact
 * list itself is: a printed count for precision, a rail scaled to the busiest
 * category on screen for ranking. No fabricated denominator — the rail
 * measures against the largest category actually present, not an assumed
 * total. */
function CategoryBar({ row, ceiling }: { row: MemoryCategoryCountV1; ceiling: number }) {
  const fraction = ceiling > 0 ? row.count / ceiling : null;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 truncate text-2xs text-text-secondary">
          {row.category}
        </span>
        <span className="td-value text-2xs text-text-primary" data-cell="numeric">
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
  return <Readout label="trust" size="lg" value={clamped.toFixed(2)} fraction={clamped} />;
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
    return <p className="text-2xs text-text-muted">feedback counts not reported</p>;
  }
  const total = helpful + unhelpful;
  if (total === 0) {
    return <p className="text-2xs text-text-muted">no feedback recorded</p>;
  }
  return (
    <figure className="flex flex-col gap-1">
      <div className="flex h-1.5 overflow-hidden rounded-full bg-surface-3">
        <div className="bg-accent" style={{ width: `${(helpful / total) * 100}%` }} />
        <div className="bg-state-stale" style={{ width: `${(unhelpful / total) * 100}%` }} />
      </div>
      <figcaption className="tabular text-2xs text-text-muted">
        {helpful} helpful · {unhelpful} unhelpful
      </figcaption>
    </figure>
  );
}
