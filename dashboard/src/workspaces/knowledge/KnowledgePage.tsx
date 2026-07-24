import { useMemo, useState } from 'react';
import { Search } from 'lucide-react';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { Meter, Readout } from '../../ui/instrument.tsx';
import { Chart } from '../../viz/chart/Chart.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { formatCount, splitCount } from '../../ui/format.ts';
import { cn } from '../../ui/cn';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  MemoryOverviewPayloadSchema,
  type CategoryCount,
  type FactRow,
  type GrowthPoint,
  type HrrCoverageRow,
} from './contracts.ts';

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
    MemoryOverviewPayloadSchema,
  );
  const [selected, setSelected] = useState<FactRow | null>(null);

  return (
    <ExplorerSplit
      filters={
        <LegacyBoundary
          title="Memory"
          pending={overview.isPending}
          result={overview.data}
        >
          {(data) => {
            const stats = data.holographic.overview;
            const histogram = (stats?.trust_histogram ?? []).map((b) => ({
              label: b.label,
              value: b.count,
              hint: 'facts',
            }));
            // Ranked by count so the rail's length is a real ordering, not an
            // accident of whatever order the producer emitted rows in.
            const categories = [...(stats?.categories ?? [])].sort(
              (a, b) => b.count - a.count,
            );
            const categoryCeiling = categories.reduce(
              (max, row) => Math.max(max, row.count),
              0,
            );
            const growth = stats?.growth ?? [];
            return (
              <div className="flex flex-col gap-3">
                <form
                  className="relative"
                  onSubmit={(event) => {
                    event.preventDefault();
                    setApplied(query.trim());
                  }}
                >
                  <Search
                    aria-hidden
                    size={13}
                    className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-text-muted"
                  />
                  <input
                    value={query}
                    onChange={(event) => setQuery(event.target.value)}
                    placeholder="Search facts"
                    aria-label="Search facts"
                    className="h-8 w-full rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 pl-7 pr-2 text-xs text-text-primary placeholder:text-text-muted focus:border-accent/60 focus:outline-none"
                  />
                </form>
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
                      value={splitCount(stats?.facts).value}
                      unit={splitCount(stats?.facts).unit}
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
                        value={splitCount(stats?.entities).value}
                        unit={splitCount(stats?.entities).unit}
                      />
                    </div>
                    {stats?.banks != null ? (
                      <div className="min-w-0 flex-1 border-l border-edge-subtle px-3 py-2">
                        <Readout label="banks" size="sm" value={stats.banks} />
                      </div>
                    ) : null}
                  </div>
                </div>
                {histogram.length > 0 ? (
                  <figure className="flex flex-col gap-1.5">
                    <figcaption className="td-legend">trust distribution</figcaption>
                    {/* Bucket labels read ".0-0.1", ".1-0.2" and so on; at 9px
                     * every fifth one printed as unreadable debris under the
                     * bars. The axis is a known 0→1 scale, so it is ruled with
                     * its two ends instead of relabelled. */}
                    <Chart
                      ariaLabel={`Trust distribution across ${histogram.length} buckets; the facts list carries per-fact trust values`}
                      height={80}
                      option={{
                        xAxis: {
                          type: 'category',
                          data: histogram.map((bucket) => bucket.label),
                          axisLabel: { show: false },
                          axisTick: { show: false },
                        },
                        yAxis: { type: 'value', axisLabel: { show: false } },
                        grid: { left: 2, right: 2, top: 6, bottom: 2, containLabel: true },
                        series: [
                          {
                            type: 'bar',
                            barCategoryGap: '20%',
                            data: histogram.map((bucket) => bucket.value),
                          },
                        ],
                      }}
                    />
                    <div
                      aria-hidden
                      className="flex items-center justify-between border-t border-edge-subtle pt-1"
                    >
                      <span className="td-legend">0 · decayed</span>
                      <span className="td-legend">1 · held</span>
                    </div>
                  </figure>
                ) : null}
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
                {(stats?.hrr_coverage ?? []).length > 0 ? (
                  <figure className="flex flex-col gap-2">
                    <figcaption className="td-legend">HRR vector coverage</figcaption>
                    <div className="flex flex-col gap-2">
                      {(stats?.hrr_coverage ?? []).map((row) => (
                        <HrrCoverageBar key={row.category} row={row} />
                      ))}
                    </div>
                  </figure>
                ) : null}
                {growth.length > 0 ? (
                  <figure className="flex flex-col gap-1.5">
                    <figcaption className="td-legend">growth</figcaption>
                    {/* Same axis-elision as trust distribution: twelve weekly
                     * dates at 9px in a 224px rail is unreadable debris, so
                     * the shape carries the trend and the two endpoints are
                     * printed directly underneath instead of a rotated,
                     * truncated axis. */}
                    <Chart
                      ariaLabel={`Cumulative facts recorded across ${growth.length} periods, from ${growth[0]!.date} (${growth[0]!.cumulative_facts.toLocaleString()} facts) to ${growth[growth.length - 1]!.date} (${growth[growth.length - 1]!.cumulative_facts.toLocaleString()} facts)`}
                      height={70}
                      option={{
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
                      }}
                    />
                    {/* Date-over-value, the same shape as every other
                     * Readout on this page, rather than one cramped line —
                     * "MAY 8 · 3,200" beside its mirror at the opposite edge
                     * had nowhere to go in a 224px rail and truncated into
                     * the gap between them. */}
                    <div
                      aria-hidden
                      className="flex items-start justify-between gap-2 border-t border-edge-subtle pt-1.5"
                    >
                      <Readout
                        label={formatShortDate(growth[0]!.date)}
                        value={formatCount(growth[0]!.cumulative_facts)}
                        size="sm"
                      />
                      <Readout
                        label={formatShortDate(growth[growth.length - 1]!.date)}
                        value={formatCount(growth[growth.length - 1]!.cumulative_facts)}
                        size="sm"
                        align="right"
                      />
                    </div>
                  </figure>
                ) : null}
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
            if (data.holographic.error) {
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  memory store unavailable: {data.holographic.error}
                </p>
              );
            }
            if (facts.length === 0) {
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  {applied ? `no facts match “${applied}”` : 'no facts recorded'}
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
            return (
              <VirtualList
                items={facts}
                getKey={(fact) => String(fact.fact_id)}
                renderItem={(fact) => (
                  <FactListRow
                    fact={fact}
                    recallCeiling={recallCeiling}
                    selected={selected?.fact_id === fact.fact_id}
                    onSelect={() => setSelected(fact)}
                  />
                )}
              />
            );
          }}
        </LegacyBoundary>
      }
      inspector={
        selected ? (
          <InspectorPanel title="Fact" onClose={() => setSelected(null)}>
            <div className="flex flex-col gap-3">
              <TrustGauge score={selected.trust_score} />
              {selected.content ? (
                <p className="whitespace-pre-wrap text-xs leading-relaxed">
                  {selected.content}
                </p>
              ) : null}
              <FeedbackSplit
                helpful={selected.helpful_count ?? 0}
                unhelpful={selected.unhelpful_count ?? 0}
              />
              <KeyValueTree
                value={Object.fromEntries(
                  Object.entries(selected).filter(([k]) => k !== 'content'),
                )}
              />
            </div>
          </InspectorPanel>
        ) : undefined
      }
    />
  );
}

/** One fact, read as three ranked quantities and a sentence.
 *
 * The row previously spent forty pixels on a hairline trust bar with no
 * number, then printed the recall count as plain grey text — so a column of
 * facts carried no visible ordering at all and the two measurements that
 * define this product (how much a memory is trusted, how often it is
 * reinforced) were the least legible things on the row. Both now get a printed
 * figure AND a length: the digits for precision, the rail for ranking. */
function FactListRow({
  fact,
  recallCeiling,
  selected,
  onSelect,
}: {
  fact: FactRow;
  recallCeiling: number;
  selected: boolean;
  onSelect: () => void;
}) {
  const summary = useMemo(
    () => (fact.content ?? String(fact.fact_id)).split('\n')[0] ?? '',
    [fact],
  );
  const trust = Math.max(0, Math.min(fact.trust_score, 1));
  const recalls = fact.retrieval_count ?? 0;
  return (
    <DataRow selected={selected} onSelect={onSelect}>
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
        <Meter
          fraction={trust}
          className="h-[3px]"
          tone={
            trust >= 0.7 ? 'bg-accent' : trust >= 0.4 ? 'bg-accent/60' : 'bg-accent/30'
          }
        />
      </span>
      <span className="min-w-0 flex-1 truncate text-text-primary">{summary}</span>
      {/* Column priority under 768px: the fact itself and how far it can be
       * trusted are the row. Category and recall count are what a narrow
       * viewport gives up -- keeping all four columns crushed the summary to
       * three glyphs, which is not density, just damage. */}
      {fact.category ? (
        <span className="td-legend shrink-0 border border-edge-subtle px-1.5 py-1 max-md:hidden">
          {fact.category}
        </span>
      ) : null}
      <span className="flex w-20 shrink-0 flex-col items-end gap-1 max-md:hidden">
        <span className="td-value text-2xs leading-none text-text-secondary" data-cell="numeric">
          {recalls}
          <span className="td-unit ml-1">rc</span>
        </span>
        <Meter
          fraction={recallCeiling > 0 ? recalls / recallCeiling : null}
          className="h-[3px] w-full"
          align="right"
          tone="bg-text-muted"
        />
      </span>
    </DataRow>
  );
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
function CategoryBar({ row, ceiling }: { row: CategoryCount; ceiling: number }) {
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
        className="h-1"
        ariaLabel={`${row.category}: ${row.count.toLocaleString()} facts`}
      />
    </div>
  );
}

/** Per-category HRR coverage: fraction bar plus a truthful status label when
 * the bank is missing, stale, or under-vectorized.
 *
 * The status cell used to be 36px wide, which is narrower than the words it
 * has to hold: "no bank" and "stale" both wrapped onto a second line and
 * collided with the bar above. Category and status now share one line above a
 * full-width rail, so the longest status string in the taxonomy still sits on
 * one line inside a 224px filter rail. */
function HrrCoverageBar({ row }: { row: HrrCoverageRow }) {
  const clamped = Math.max(0, Math.min(row.coverage, 1));
  const degraded = row.status !== 'ready';
  // missing_bank has no vector bank to measure against, so a percentage would
  // be a fabricated denominator: the status is reported instead of a number.
  const readout =
    degraded && row.status !== 'missing_vectors'
      ? row.status === 'missing_bank'
        ? 'no bank'
        : 'stale'
      : `${(clamped * 100).toFixed(0)}%`;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 truncate text-2xs text-text-secondary">
          {row.category}
        </span>
        <span
          className={cn(
            'shrink-0 text-2xs',
            degraded ? 'td-legend' : 'tabular text-text-primary',
          )}
        >
          {readout}
        </span>
      </div>
      <span
        className="td-meter block h-1 w-full"
        role="img"
        aria-label={`${row.category} HRR coverage ${(clamped * 100).toFixed(0)}% (${row.hrr_vectors}/${row.facts} facts), status ${row.status.replace('_', ' ')}`}
      >
        <span
          className={cn('td-meter-fill', degraded && 'bg-accent/40')}
          style={{ width: `${clamped * 100}%` }}
        />
      </span>
    </div>
  );
}

function TrustGauge({ score }: { score: number }) {
  const clamped = Math.max(0, Math.min(score, 1));
  return (
    <div className="flex items-center gap-2">
      <span className="tabular text-lg font-semibold">{clamped.toFixed(2)}</span>
      <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-surface-3">
        <div
          className="h-full rounded-full bg-accent"
          style={{ width: `${clamped * 100}%` }}
        />
      </div>
      <span className="text-2xs text-text-muted">trust</span>
    </div>
  );
}

/** Helpful vs unhelpful feedback as one proportional split bar. */
function FeedbackSplit({ helpful, unhelpful }: { helpful: number; unhelpful: number }) {
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
