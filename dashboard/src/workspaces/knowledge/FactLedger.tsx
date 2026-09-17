/**
 * The FACT LEDGER: the exact, keyboard-operable reading of the loaded slice.
 *
 * The constellation above it is a picture of the same rows; this is the
 * authority a reader can traverse, sort, and select from. A row under the
 * pointer or under focus is inspected (the inspector previews it); a row
 * clicked or activated is selected (the inspector loads its canonical detail
 * and trust audit). The two are different verbs and the ledger fires them
 * separately.
 *
 * The ledger is honest about its reach. The overview route ranks the store by
 * trust and serves at most a hundred rows with no cursor, so the sort control
 * reorders what arrived and the header says what the slice cannot reach.
 */
import {
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from 'react';

import type {
  DashboardCoverageV1,
  MemoryFactRowV1,
  MemoryFactsCoverageV1,
  MemoryReadStatusV1,
} from '../../contracts/generated.ts';
import { DataRow } from '../../ui/archetypes/ExplorerSplit.tsx';
import { cn } from '../../ui/cn';
import { FigureRail, Meter } from '../../ui/instrument.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import {
  FACT_SORTS,
  factSortLabel,
  ledgerDay,
  type FactSort,
} from './ledger.ts';
import { factsBelow, type LoadedTrust, type TrustDistribution } from './trust.ts';

/** Two lines of a fact's content, which is the height a 56px row can carry.
 * A row taller than this stops being a list; a row shorter than this shows a
 * ninety-character prefix of a nineteen-hundred-character fact. */
export const FACT_ROW_HEIGHT = 56;

const FACT_SUMMARY_CHARACTER_SAMPLE = 'abcdefghijklmnopqrstuvwxyz';

export function FactLedger({
  facts,
  coverageNotice,
  loaded,
  distribution,
  query,
  sort,
  onSort,
  storeFactCount,
  factsCoverage,
  selectedFactId,
  inspectedFactId,
  onInspect,
  onSelect,
}: {
  /** Already ordered by `sort`. */
  facts: MemoryFactRowV1[];
  coverageNotice: ReactNode;
  loaded: LoadedTrust | null;
  distribution: TrustDistribution;
  query: string;
  sort: FactSort;
  onSort: (sort: FactSort) => void;
  /** The store's own fact total from the overview summary, when it reported one. */
  storeFactCount: number | null;
  factsCoverage: MemoryFactsCoverageV1;
  selectedFactId: string | null;
  inspectedFactId: string | null;
  onInspect: (factId: string) => void;
  onSelect: (factId: string) => void;
}) {
  const listRootRef = useRef<HTMLDivElement>(null);
  const summaryProbeRef = useRef<HTMLSpanElement>(null);
  const characterProbeRef = useRef<HTMLSpanElement>(null);
  const characterLimit = useFactSummaryCharacterLimit(
    listRootRef,
    summaryProbeRef,
    characterProbeRef,
  );
  // Recall counts have no absolute ceiling, so the rail is scaled to the
  // busiest fact actually on screen. That makes the column a ranking of what
  // is loaded — which is what it is — rather than an implied fraction of some
  // total the daemon never reported.
  const recallCeiling = facts.reduce(
    (max, fact) => Math.max(max, fact.retrieval_count ?? 0),
    0,
  );
  return (
    <div ref={listRootRef} className="relative h-full" data-testid="fact-ledger">
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
        <span className="hidden w-20 shrink-0 md:block" />
        <span className="hidden w-24 shrink-0 lg:block" />
        <span className="hidden w-20 shrink-0 lg:block" />
        <span className="hidden w-20 shrink-0 xl:block" />
        <span className="hidden w-20 shrink-0 md:block" />
      </div>
      <VirtualList
        items={facts}
        getKey={(fact) => String(fact.fact_id)}
        estimateHeight={FACT_ROW_HEIGHT}
        header={
          <>
            {coverageNotice}
            {loaded ? (
              <LedgerHeader
                loaded={loaded}
                distribution={distribution}
                query={query}
                sort={sort}
                onSort={onSort}
                storeFactCount={storeFactCount}
                factsCoverage={factsCoverage}
              />
            ) : null}
            <ColumnLegend />
          </>
        }
        renderItem={(fact) => (
          <FactRow
            fact={fact}
            recallCeiling={recallCeiling}
            // A rail scaled 0-1 across a slice whose trust never leaves the top
            // tenth is the same length on every row: not a ranking, just ink.
            // The header states the slice's spread instead, and the printed
            // figure keeps the precision.
            showTrustRail={loaded ? !loaded.flat : true}
            characterLimit={characterLimit}
            selected={selectedFactId === fact.fact_id}
            inspected={inspectedFactId === fact.fact_id}
            onInspect={() => onInspect(fact.fact_id)}
            onSelect={() => onSelect(fact.fact_id)}
          />
        )}
      />
    </div>
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
 * the list". The same header owns the sort control, because a sort is a
 * reordering of exactly this slice and nothing more.
 */
function LedgerHeader({
  loaded,
  distribution,
  query,
  sort,
  onSort,
  storeFactCount,
  factsCoverage,
}: {
  loaded: LoadedTrust;
  distribution: TrustDistribution;
  query: string;
  sort: FactSort;
  onSort: (sort: FactSort) => void;
  storeFactCount: number | null;
  factsCoverage: MemoryFactsCoverageV1;
}) {
  const measuredRange =
    loaded.min != null && loaded.max != null ? { min: loaded.min, max: loaded.max } : null;
  const unreached = measuredRange ? factsBelow(distribution, measuredRange.min) : null;
  const sameEverywhere = measuredRange?.min === measuredRange?.max;
  const eligible = factsCoverage.eligible ?? storeFactCount;
  const beyond =
    eligible != null && eligible > loaded.total ? eligible - loaded.total : null;
  return (
    <div className="flex flex-col gap-1 border-b border-edge-subtle px-3 py-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <p className="td-legend">
          {loaded.total.toLocaleString()} facts loaded · {loaded.measured.toLocaleString()} with
          trust · {loaded.unavailable.toLocaleString()} unavailable
          {query ? ` · matching “${query}”` : ''}
        </p>
        <span aria-hidden className="td-rule" />
        <label className="flex items-center gap-2 text-3xs text-text-muted">
          <span className="td-legend">sort</span>
          <select
            aria-label="Sort loaded facts"
            value={sort}
            onChange={(event) => onSort(event.target.value as FactSort)}
            className="min-h-[var(--touch-target-min)] border border-edge-subtle bg-surface-0 px-2 text-2xs text-text-secondary"
          >
            {FACT_SORTS.map((option) => (
              <option key={option} value={option}>
                {factSortLabel(option)}
              </option>
            ))}
          </select>
        </label>
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        {measuredRange == null
          ? 'No loaded fact exposes a trust measurement.'
          : sameEverywhere
            ? `Every measured fact is at trust ${measuredRange.max.toFixed(2)}.`
            : `Trust ${measuredRange.min.toFixed(2)}–${measuredRange.max.toFixed(2)}, with ${loaded.atMax.toLocaleString()} at exactly ${measuredRange.max.toFixed(2)}.`}
        {measuredRange && unreached != null && unreached > 0
          ? ` The store holds ${unreached.toLocaleString()} further facts below ${measuredRange.min.toFixed(2)} that this slice does not reach.`
          : ''}
      </p>
      {/* Paging is a typed absence, not a control drawn disabled: the route
        * serves one bounded, trust-ranked slice and no cursor, so the ledger
        * states the bound and how much of the store lies past it. */}
      <p
        className="text-3xs leading-relaxed text-text-muted"
        data-testid="fact-ledger-bound"
      >
        Sorted within this slice only. The read is bounded to the top{' '}
        {factsCoverage.limit.toLocaleString()} by trust
        {beyond != null
          ? ` — ${beyond.toLocaleString()} more ${beyond === 1 ? 'fact lies' : 'facts lie'} beyond it`
          : eligible == null
            ? ' — the store did not report how many facts lie beyond it'
            : ''}
        ; the memory route serves no page cursor, so paging is unavailable rather than hidden.
      </p>
    </div>
  );
}

/** The column legend, aligned to the row grid below it. Columns that a
 * viewport gives up are withdrawn here at the same breakpoints. */
function ColumnLegend() {
  return (
    <div
      aria-hidden
      className="sticky top-0 z-10 flex items-center gap-3 border-b border-edge-subtle bg-surface-0/95 px-3 py-1.5 backdrop-blur"
    >
      <span className="td-legend w-14 shrink-0">trust</span>
      <span className="td-legend min-w-0 flex-1">fact</span>
      <span className="td-legend hidden w-20 shrink-0 md:block">category</span>
      <span className="td-legend hidden w-24 shrink-0 lg:block">source</span>
      <span className="td-legend hidden w-20 shrink-0 lg:block">created</span>
      <span className="td-legend hidden w-20 shrink-0 xl:block">recalled</span>
      <span className="td-legend hidden w-20 shrink-0 text-right md:block">recalls</span>
    </div>
  );
}

/** One fact, read as two ranked quantities, its provenance columns, and as
 * much of the fact as fits.
 *
 * Both measured quantities (how much a memory is trusted, how often it is
 * reinforced) get a printed figure AND a length: the digits for precision, the
 * rail for ranking. Facts here run to nearly two thousand characters on ONE
 * line, so the summary clamps to two lines, carries the full text on `title`,
 * and prints an explicit control on any row that is still cut — the control
 * opens the same inspector the row does, where the content is shown in full.
 */
function FactRow({
  fact,
  recallCeiling,
  showTrustRail,
  characterLimit,
  selected,
  inspected,
  onInspect,
  onSelect,
}: {
  fact: MemoryFactRowV1;
  recallCeiling: number;
  showTrustRail: boolean;
  characterLimit: number | null;
  selected: boolean;
  inspected: boolean;
  onInspect: () => void;
  onSelect: () => void;
}) {
  const content = fact.content ?? String(fact.fact_id);
  const summary = useMemo(() => content.split('\n')[0] ?? '', [content]);
  const clipped = characterLimit !== null && content.length > characterLimit;
  const trust =
    typeof fact.trust_score === 'number' ? Math.max(0, Math.min(fact.trust_score, 1)) : null;
  const recalls = fact.retrieval_count ?? 0;
  const withheld = fact.payload_access !== 'eligible';
  return (
    <DataRow
      selected={selected}
      onSelect={onSelect}
      onInspect={onInspect}
      height={FACT_ROW_HEIGHT}
      align="start"
      className={cn(inspected && !selected && 'bg-surface-1')}
    >
      <span className="flex w-14 shrink-0 flex-col gap-1">
        <span
          className={cn(
            'td-value text-2xs leading-none',
            trust == null
              ? 'text-text-muted'
              : trust >= 0.7
                ? 'text-text-primary'
                : trust >= 0.4
                  ? 'text-text-secondary'
                  : 'text-text-muted',
          )}
          data-cell="numeric"
        >
          {trust == null ? '—' : trust.toFixed(2)}
        </span>
        {showTrustRail && trust != null ? (
          <Meter
            fraction={trust}
            height="row"
            tone={trust >= 0.7 ? 'bg-accent' : trust >= 0.4 ? 'bg-accent/60' : 'bg-accent/30'}
          />
        ) : null}
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        {withheld ? (
          <span className="td-legend text-state-redacted" data-payload-access={fact.payload_access}>
            payload {fact.payload_access.replaceAll('_', ' ')} · content withheld
          </span>
        ) : (
          <span className="line-clamp-2 leading-snug text-text-primary" title={content}>
            {summary}
          </span>
        )}
        {clipped ? (
          <span className="td-legend text-accent">
            {content.length.toLocaleString()} chars · open for the rest
          </span>
        ) : null}
      </span>
      {/* Column priority under 768px: the fact itself and how far it can be
       * trusted are the row. Provenance columns are what a narrow viewport
       * gives up; every one of them is on the inspector. */}
      <span className="hidden w-20 shrink-0 md:block">
        {fact.category ? (
          <span className="td-legend inline-block max-w-full truncate border border-edge-subtle px-1.5 py-1">
            {fact.category}
          </span>
        ) : (
          <span className="td-legend text-text-muted">—</span>
        )}
      </span>
      <span
        className="td-value hidden w-24 shrink-0 truncate text-2xs text-text-secondary lg:block"
        title={fact.source_label ?? undefined}
      >
        {fact.source_label ?? <span className="text-text-muted">no source label</span>}
      </span>
      <span className="td-value hidden w-20 shrink-0 text-2xs text-text-secondary lg:block" data-cell="numeric">
        {ledgerDay(fact.created_at, '—')}
      </span>
      <span
        className={cn(
          'td-value hidden w-20 shrink-0 text-2xs xl:block',
          fact.last_recalled_at == null ? 'text-text-muted' : 'text-text-secondary',
        )}
        data-cell="numeric"
      >
        {ledgerDay(fact.last_recalled_at, 'never')}
      </span>
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

/** The two sub-read coverage statements the ledger prints above its rows: the
 * bounded fact read and the bounded graph read, each with the daemon's reason. */
export function MemoryCoverageNotices({
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
    factsCoverage.completeness !== 'complete' || factsRead?.state === 'partial';
  const graphReadComplete =
    graphRead?.state === 'ready' || graphRead?.state === 'complete_zero_findings';
  const graphIncomplete = graphCoverage.completeness !== 'complete' || !graphReadComplete;
  if (!factsIncomplete && !graphIncomplete) return null;
  const graphReset = graphRead?.code === 'graph_reset_required';
  return (
    <div className="flex flex-col gap-1 border-b border-edge-subtle px-3 py-2 text-2xs leading-relaxed">
      {factsIncomplete ? (
        <p role="status" data-state={factsRead?.state ?? 'partial'} className="text-state-partial">
          {factsRead?.state === 'partial'
            ? `Fact read is partial; reported fact coverage is ${factsCoverage.completeness}`
            : `Fact coverage is ${factsCoverage.completeness}`}
          ; this read was bounded to at most {factsCoverage.limit.toLocaleString()} facts.
        </p>
      ) : null}
      {graphReset ? (
        <p role="status" data-state="error" className="text-state-error">
          Memory graph reset required
          {graphRead.error ? `: ${graphRead.error}` : '.'}
        </p>
      ) : null}
      {graphIncomplete ? (
        <p role="status" data-state={graphRead?.state ?? 'unknown'} className="text-state-partial">
          Memory graph coverage is {graphCoverage.completeness}
          {graphCoverage.omission_reasons.length > 0
            ? `; omissions: ${graphCoverage.omission_reasons.join(', ')}.`
            : '.'}
        </p>
      ) : null}
    </div>
  );
}
