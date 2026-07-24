import { useMemo, useState } from 'react';
import { Search } from 'lucide-react';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { Chart } from '../../viz/chart/Chart.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { cn } from '../../ui/cn';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  MemoryOverviewPayloadSchema,
  type FactRow,
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
                <div className="grid grid-cols-1 gap-1.5">
                  <StatTile dense label="facts" value={stats?.facts ?? '—'} />
                  <StatTile dense label="entities" value={stats?.entities ?? '—'} />
                </div>
                {histogram.length > 0 ? (
                  <figure className="flex flex-col gap-1">
                    <figcaption className="text-2xs text-text-muted">
                      trust distribution (0 → 1)
                    </figcaption>
                    <Chart
                      ariaLabel={`Trust distribution across ${histogram.length} buckets; the facts list carries per-fact trust values`}
                      height={96}
                      option={{
                        xAxis: {
                          type: 'category',
                          data: histogram.map((bucket) => bucket.label),
                          axisLabel: { interval: 4, fontSize: 9 },
                        },
                        yAxis: { type: 'value', axisLabel: { show: false } },
                        grid: { left: 2, right: 2, top: 6, bottom: 2, containLabel: true },
                        series: [
                          {
                            type: 'bar',
                            barCategoryGap: '25%',
                            itemStyle: { borderRadius: [2, 2, 0, 0] },
                            data: histogram.map((bucket) => bucket.value),
                          },
                        ],
                      }}
                    />
                  </figure>
                ) : null}
                {(stats?.hrr_coverage ?? []).length > 0 ? (
                  <figure className="flex flex-col gap-1">
                    <figcaption className="text-2xs text-text-muted">
                      HRR vector coverage by category
                    </figcaption>
                    <div className="flex flex-col gap-1">
                      {(stats?.hrr_coverage ?? []).map((row) => (
                        <HrrCoverageBar key={row.category} row={row} />
                      ))}
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
            return (
              <VirtualList
                items={facts}
                getKey={(fact) => String(fact.fact_id)}
                renderItem={(fact) => (
                  <FactListRow
                    fact={fact}
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

function FactListRow({
  fact,
  selected,
  onSelect,
}: {
  fact: FactRow;
  selected: boolean;
  onSelect: () => void;
}) {
  const summary = useMemo(
    () => (fact.content ?? String(fact.fact_id)).split('\n')[0] ?? '',
    [fact],
  );
  return (
    <DataRow selected={selected} onSelect={onSelect}>
      <TrustBar score={fact.trust_score} />
      <span className="min-w-0 flex-1 truncate">{summary}</span>
      {fact.category ? (
        <span className="shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs text-text-muted">
          {fact.category}
        </span>
      ) : null}
      <span className="tabular w-16 shrink-0 text-right text-2xs text-text-muted">
        {fact.retrieval_count ?? 0} recalls
      </span>
    </DataRow>
  );
}

/** Per-category HRR coverage: fraction bar plus a truthful status label when
 * the bank is missing, stale, or under-vectorized. */
function HrrCoverageBar({ row }: { row: HrrCoverageRow }) {
  const clamped = Math.max(0, Math.min(row.coverage, 1));
  const degraded = row.status !== 'ready';
  return (
    <div className="flex items-center gap-2">
      <span className="w-20 shrink-0 truncate text-2xs text-text-muted">{row.category}</span>
      <span
        className="relative h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-surface-3"
        role="img"
        aria-label={`${row.category} HRR coverage ${(clamped * 100).toFixed(0)}% (${row.hrr_vectors}/${row.facts} facts), status ${row.status.replace('_', ' ')}`}
      >
        <span
          className={cn(
            'absolute inset-y-0 left-0 rounded-full',
            degraded ? 'bg-accent/40' : 'bg-accent',
          )}
          style={{ width: `${clamped * 100}%` }}
        />
      </span>
      <span className="tabular w-14 shrink-0 whitespace-nowrap text-right text-2xs text-text-muted">
        {degraded && row.status !== 'missing_vectors'
          ? row.status === 'missing_bank'
            ? 'no bank'
            : 'stale'
          : `${(clamped * 100).toFixed(0)}%`}
      </span>
    </div>
  );
}

/** Trust rendered as a fixed-width luminance bar: length = score, so a column
 * of rows reads as a sorted-trust texture at a glance. */
function TrustBar({ score }: { score: number }) {
  const clamped = Math.max(0, Math.min(score, 1));
  return (
    <span
      className="relative h-1 w-10 shrink-0 overflow-hidden rounded-full bg-surface-3"
      role="img"
      aria-label={`trust ${clamped.toFixed(2)}`}
    >
      <span
        className={cn(
          'absolute inset-y-0 left-0 rounded-full',
          clamped >= 0.7 ? 'bg-accent' : clamped >= 0.4 ? 'bg-accent/60' : 'bg-accent/30',
        )}
        style={{ width: `${clamped * 100}%` }}
      />
    </span>
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
