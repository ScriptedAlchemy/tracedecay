import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react';
import type { EChartsOption } from 'echarts';
import { ReadModelState, ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { Corners, Meter, Readout } from '../../ui/instrument.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';
import { Chart } from '../../viz/chart/Chart.tsx';
import { formatCount, splitCount } from '../../ui/format.ts';
import { cn } from '../../ui/cn';
import { envelopePayload, useEnvelope } from '../../data/query/useEnvelope.ts';
import { scopeKey, useScope } from '../../data/scope/store.ts';
import { usePublishStatusRegisters } from '../../data/shell/statusRegisters.ts';
import {
  type MemoryCategoryCountV1,
  MemoryFactDetailPayloadV1Schema,
  type MemoryHolographicPayloadV1,
  MemoryOverviewPayloadV1Schema,
  type MemoryStatusV1,
  MemoryStatusPayloadV1Schema,
} from '../../contracts/generated.ts';
import { CurationConsole } from './CurationConsole.tsx';
import { MemoryGeometry } from './MemoryGeometry.tsx';
import { MemoryOplog } from './MemoryOplog.tsx';
import { FactCameras } from './FactCameras.tsx';
import { FactInspector } from './FactInspector.tsx';
import { FactLedger, FactSortControl, MemoryCoverageNotices } from './FactLedger.tsx';
import { composeFactScene, type FactScene } from './factScene.ts';
import { useFactsAddress } from './factsAddress.ts';
import { sortFacts } from './ledger.ts';
import { cameraRegister, graphRegister, memoryRegister } from './knowledgeRegisters.ts';
import {
  KNOWLEDGE_PANEL_ID,
  KnowledgeViewSwitcher,
  knowledgeTabId,
  knowledgeViewNote,
  useKnowledgeView,
  type KnowledgeViewKind,
} from './KnowledgeViews.tsx';
import {
  composeTrustDistribution,
  summarizeLoadedTrust,
  trustSourceNote,
  type TrustDistribution,
} from './trust.ts';

const BASE = '/api/plugins/holographic';

/**
 * Knowledge, channel seven.
 *
 * Four camera positions over one memory store, in the order a reader descends
 * through it: the facts explorer, the phase geometry those facts sit in, the
 * daemon's automatic curation outcomes, and the store's own record of what
 * changed. `KnowledgeViews.tsx` owns the camera; each view owns its reads, so
 * a position is paid for only when it is looked at, and one camera's answer
 * can never stand in for another's.
 *
 * Everything the daemon mounts for holographic memory is consumed here. Three
 * of those routes are contracted (`/`, `/status`, `/fact/{id}`) and read
 * through the generated schemas; the rest answer bare JSON and are read through
 * the house payload ladder with schemas written against their handlers, see
 * `data/query/memory.ts`, which explains why that split exists and what it
 * obliges.
 */
export function KnowledgePage() {
  const [view, selectView] = useKnowledgeView();
  // The camera position rides the shell's status strip beside the transport
  // facts, so a linked position can be read off the screen from any camera.
  usePublishStatusRegisters('knowledge', [cameraRegister(view)]);
  return (
    // Below `lg` the Facts camera is a vertical stack and the column has to be
    // allowed its natural height: pinned to the viewport it squeezed the
    // aperture and the ledger to nothing and painted them through the summary
    // bay beneath. The shell's `main` is the scroll container, so giving the
    // stack its real height simply makes the page scroll; from `lg` the panes
    // split the viewport and each owns its overflow again.
    <div className="flex min-h-full flex-col lg:h-full lg:min-h-0">
      {/* `flex-wrap`, because the note under the camera is prose and the
       * switcher is four 44px targets: held on one row they laid the note
       * past the right edge at 320 CSS px and at 400% zoom. */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-edge-subtle bg-surface-1 px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Knowledge</h1>
        <KnowledgeViewSwitcher active={view} onSelect={selectView} />
        <p className="min-w-0 text-2xs text-text-muted">{knowledgeViewNote(view)}</p>
      </div>
      {/* The element `aria-controls` names, present for as long as the switcher
       * is, a reference to an element that was never drawn is an invalid one,
       * which is what the accessibility gate reads it as. */}
      <div
        id={KNOWLEDGE_PANEL_ID}
        role="tabpanel"
        aria-labelledby={knowledgeTabId(view)}
        className="flex min-h-0 flex-1 flex-col"
      >
        <KnowledgeView kind={view} onSelectView={selectView} />
      </div>
    </div>
  );
}

/** The camera, applied. Exhaustive so a view added to the switcher cannot be
 * left without something to draw. */
function KnowledgeView({
  kind,
  onSelectView,
}: {
  kind: KnowledgeViewKind;
  onSelectView: (kind: KnowledgeViewKind) => void;
}) {
  switch (kind) {
    case 'facts':
      return <KnowledgeFacts onOpenGeometry={() => onSelectView('geometry')} />;
    case 'geometry':
      return <MemoryGeometry />;
    case 'curation':
      return <CurationConsole />;
    case 'oplog':
      return <MemoryOplog />;
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/**
 * The Facts camera: the provenance cameras over the ledger, with the
 * inspector beside them.
 *
 * Three verbs, kept apart. INSPECT is the fact under the pointer or under
 * keyboard focus, on a camera row or a ledger row, and it previews
 * the bounded overview row without a fetch; it is sticky until Escape so a
 * reader can move into the inspector. SELECT is a click or Enter, lives in
 * the address, and reads the canonical detail and trust audit. SEARCH is the
 * submitted query, also in the address, and re-asks the store for its bounded
 * slice. None of them changes a measured value or fires activity.
 */
function KnowledgeFacts({ onOpenGeometry }: { onOpenGeometry: () => void }) {
  const scope = useScope((state) => state.scope);
  const currentScopeKey = scopeKey(scope);
  const { selectedFactId, selectFact, query: applied, applyQuery, sort, setSort } =
    useFactsAddress();
  const [query, setQuery] = useState(applied);
  const [inspectedFactId, setInspectedFactId] = useState<string | null>(null);

  const overview = useEnvelope(
    ['memory', 'overview', applied],
    `${BASE}/?limit=100${applied ? `&q=${encodeURIComponent(applied)}` : ''}`,
    MemoryOverviewPayloadV1Schema,
  );
  // The overview histogram is the finest canonical store-wide distribution.
  // Status contributes the current four-band authority when the histogram is
  // empty, so an empty store stays distinct from a failed reading.
  const status = useEnvelope(['memory', 'status'], `${BASE}/status`, MemoryStatusPayloadV1Schema);
  const statusMemory = envelopePayload(status.data)?.memory;
  const overviewData = envelopePayload(overview.data);
  const holographic = overviewData?.holographic;
  const trust = useMemo(
    () =>
      composeTrustDistribution(
        holographic?.overview?.trust_histogram,
        statusMemory,
        holographic?.facts,
      ),
    [holographic, statusMemory],
  );

  const facts = holographic?.facts;
  const sorted = useMemo(() => sortFacts(facts ?? [], sort), [facts, sort]);
  const graph = holographic?.graph;
  const scene = useMemo(() => (graph ? composeFactScene(graph, facts ?? []) : null), [graph, facts]);

  const detail = useEnvelope(
    ['memory', 'fact', String(selectedFactId ?? '')],
    `${BASE}/fact/${encodeURIComponent(String(selectedFactId ?? ''))}`,
    MemoryFactDetailPayloadV1Schema,
    { enabled: selectedFactId != null },
  );

  // Inspection is transient and belongs to the slice it was made in.
  useEffect(() => {
    setInspectedFactId(null);
  }, [currentScopeKey, applied]);

  const select = useCallback(
    (factId: string) => {
      selectFact(factId);
      setInspectedFactId(null);
    },
    [selectFact],
  );
  const dismiss = useCallback(() => {
    if (inspectedFactId !== null) {
      setInspectedFactId(null);
      return;
    }
    if (selectedFactId !== null) selectFact(null);
  }, [inspectedFactId, selectedFactId, selectFact]);

  const shownFactId = inspectedFactId ?? selectedFactId;
  const mode =
    inspectedFactId !== null && inspectedFactId !== selectedFactId ? 'inspecting' : 'selected';
  const shownRow = shownFactId ? facts?.find((fact) => fact.fact_id === shownFactId) : undefined;
  const shownNodeId = shownFactId && scene ? scene.nodeIdByFact.get(shownFactId) : undefined;
  const shownRelations = shownNodeId && scene ? (scene.byNode.get(shownNodeId)?.degree ?? null) : null;

  const reads = holographic?.reads;
  const overviewEnvelope = overview.data?.outcome === 'envelope' ? overview.data.envelope : null;
  const overviewState = envelopeReadState(overview.isPending, overview.data, {
    loading: 'loading memory overview',
    unknown: 'memory overview has not answered',
  });

  // The two authorities the plate names on the strip, posted in their own
  // words: the memory overview envelope and its graph sub-read. The facts and
  // status sub-reads print their states on the ledger and the store summary,
  // where the width for a reason exists; the strip has room for a word.
  usePublishStatusRegisters('knowledge:facts', [
    memoryRegister(overview.isPending, overview.data),
    graphRegister(overview.isPending, overview.data),
  ]);

  return (
    <div className="flex flex-col lg:h-full lg:min-h-0" data-testid="knowledge-facts">
      {overviewEnvelope ? (
        <EnvelopeTruth
          envelope={overviewEnvelope}
          refreshing={overview.isFetching}
          onRefresh={() => void overview.refetch()}
        />
      ) : null}
      <div className="flex flex-col lg:min-h-0 lg:flex-1 lg:flex-row">
        <section
          aria-label="Facts camera"
          className="flex min-w-0 flex-col lg:min-h-0 lg:flex-1"
        >
          <ReadSection title="Memory" state={overviewState} chrome="centered">
            {(envelope) => {
              const data = envelope.payload.holographic;
              if (data.error) {
                return (
                  <div className="flex flex-1 items-center justify-center p-6">
                    <StateChip kind="unavailable" detail={`memory store unavailable: ${data.error}`} />
                  </div>
                );
              }
              return (
                <>
                  {/* The composer and the headline counts sit inside the read
                    * boundary on purpose: a search box over a store the browser
                    * never reached would invite a reader to read its empty
                    * answer as "no matches". */}
                  <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-edge-subtle bg-surface-1 px-3 py-2">
                    <div className="min-w-0 flex-1 basis-64">
                      <SearchField
                        value={query}
                        onChange={setQuery}
                        onSubmit={() => applyQuery(query)}
                        onClear={() => {
                          setQuery('');
                          applyQuery('');
                        }}
                        label="Search facts"
                        placeholder="Search facts"
                        hint="press / to focus, Esc to clear · the store answers a bounded, trust-ranked slice"
                        submitted={applied}
                      />
                    </div>
                    <StoreReadouts summary={data.overview} statusMemory={statusMemory} />
                  </div>
                  <div className="flex min-w-0 flex-col lg:min-h-0 lg:flex-1">
                    <Aperture
                      data={data}
                      scene={scene}
                      inspectedFactId={inspectedFactId}
                      selectedFactId={selectedFactId}
                      onInspect={setInspectedFactId}
                      onSelect={select}
                    />
                    <LedgerBay
                      data={data}
                      trust={trust}
                      applied={applied}
                      sorted={sorted}
                      sort={sort}
                      onSort={setSort}
                      selectedFactId={selectedFactId}
                      inspectedFactId={inspectedFactId}
                      onInspect={setInspectedFactId}
                      onSelect={select}
                    />
                  </div>
                </>
              );
            }}
          </ReadSection>
        </section>
        <aside
          aria-label="Inspector"
          className="flex w-full shrink-0 flex-col border-t border-edge-subtle bg-surface-1 lg:min-h-0 lg:w-[24rem] lg:overflow-auto lg:border-l lg:border-t-0 xl:w-[26rem]"
          data-testid="knowledge-inspector-bay"
        >
          {shownFactId ? (
            <FactInspector
              key={shownFactId}
              factId={shownFactId}
              mode={mode}
              row={shownRow}
              detail={mode === 'selected' ? detail.data : undefined}
              detailPending={mode === 'selected' && detail.isPending}
              relations={shownRelations}
              graphRead={reads?.graph}
              onSelect={select}
              onDismiss={dismiss}
              onOpenGeometry={onOpenGeometry}
            />
          ) : overviewState.kind === 'blocked' ? (
            // The summary bay hangs off the same read as the field: it reports
            // the same failure rather than a store with no distribution.
            <ReadModelState kind={overviewState.state} detail={overviewState.detail} />
          ) : (
            <StoreSummary
              summary={holographic?.overview ?? null}
              statusMemory={statusMemory}
              distribution={trust}
            />
          )}
        </aside>
      </div>
    </div>
  );
}

/** The provenance cameras, or the typed reason the graph is not drawn. The
 * graph is its own sub-read; a failed or refused graph must not render as an
 * empty field over a healthy ledger. From `lg` the aperture holds 45% of the
 * column and the ledger the rest, so the exact rows stay the larger share. */
function Aperture({
  data,
  scene,
  inspectedFactId,
  selectedFactId,
  onInspect,
  onSelect,
}: {
  data: MemoryHolographicPayloadV1;
  scene: FactScene | null;
  inspectedFactId: string | null;
  selectedFactId: string | null;
  onInspect: (factId: string) => void;
  onSelect: (factId: string) => void;
}) {
  const graphRead = data.reads?.graph;
  const drawable =
    graphRead === undefined ||
    graphRead.state === 'ready' ||
    graphRead.state === 'partial' ||
    graphRead.state === 'complete_zero_findings';
  return (
    <div
      className="relative flex shrink-0 flex-col border-b border-edge-subtle p-1.5 lg:h-[45%] lg:min-h-0"
      data-testid="knowledge-aperture"
    >
      {drawable && scene ? (
        <FactCameras
          scene={scene}
          inspectedFactId={inspectedFactId}
          selectedFactId={selectedFactId}
          onInspect={onInspect}
          onSelect={onSelect}
          graphRead={graphRead}
        />
      ) : (
        <div className="td-optic relative flex min-h-[200px] flex-col items-center justify-center gap-3 p-6 text-center lg:h-full">
          <Corners tone="signal" />
          <span className="td-title text-text-secondary">Provenance cameras</span>
          <StateChip
            kind={graphRead?.state ?? 'unknown'}
            detail={graphRead?.error ?? graphRead?.code ?? 'memory graph read'}
          />
          <p className="max-w-md text-body leading-relaxed text-text-muted">
            The memory graph sub-read did not serve a topology, so no field is drawn. The ledger
            below is read separately and stands on its own.
          </p>
        </div>
      )}
    </div>
  );
}

/** The ledger bay: coverage statements, then the rows, or the typed reason
 * there are none. */
function LedgerBay({
  data,
  trust,
  applied,
  sorted,
  sort,
  onSort,
  selectedFactId,
  inspectedFactId,
  onInspect,
  onSelect,
}: {
  data: MemoryHolographicPayloadV1;
  trust: TrustDistribution;
  applied: string;
  sorted: ReturnType<typeof sortFacts>;
  sort: Parameters<typeof sortFacts>[1];
  onSort: (sort: Parameters<typeof sortFacts>[1]) => void;
  selectedFactId: string | null;
  inspectedFactId: string | null;
  onInspect: (factId: string) => void;
  onSelect: (factId: string) => void;
}) {
  const factsRead = data.reads?.facts;
  const graphRead = data.reads?.graph;
  const factsComplete =
    data.facts_coverage.completeness === 'complete' &&
    (factsRead?.state === 'ready' || factsRead?.state === 'complete_zero_findings');
  const coverageNotice = (
    <MemoryCoverageNotices
      factsCoverage={data.facts_coverage}
      factsRead={factsRead}
      graphCoverage={data.graph.coverage}
      graphRead={graphRead}
    />
  );
  let body: ReactNode;
  if (
    factsRead &&
    factsRead.state !== 'ready' &&
    factsRead.state !== 'partial' &&
    factsRead.state !== 'complete_zero_findings'
  ) {
    body = (
      <p
        role="status"
        data-state={factsRead.state}
        className="p-6 text-center text-sm text-state-error"
      >
        Fact list read is {factsRead.state.replaceAll('_', ' ')}
        {factsRead.error ? `: ${factsRead.error}` : '.'}
      </p>
    );
  } else if (sorted.length === 0) {
    body = (
      <div className="flex flex-col">
        {coverageNotice}
        <p className="p-6 text-center text-sm text-text-muted">
          {applied
            ? `no loaded facts match "${applied}"`
            : factsComplete
              ? 'no facts recorded'
              : 'no facts were returned by this incomplete read'}
        </p>
      </div>
    );
  } else {
    body = (
      <FactLedger
        facts={sorted}
        coverageNotice={coverageNotice}
        loaded={summarizeLoadedTrust(sorted)}
        distribution={trust}
        query={applied}
        storeFactCount={data.overview?.facts ?? null}
        factsCoverage={data.facts_coverage}
        selectedFactId={selectedFactId}
        inspectedFactId={inspectedFactId}
        onInspect={onInspect}
        onSelect={onSelect}
      />
    );
  }
  return (
    // From `lg` the ledger takes whatever the aperture leaves and scrolls its
    // rows inside; below `lg` the page scrolls, so the ledger takes a fixed,
    // readable height of its own rather than a share of a viewport it is no
    // longer pinned to.
    <section
      aria-label="Fact ledger"
      className="flex min-w-0 flex-col overflow-hidden max-lg:h-[30rem] lg:min-h-[var(--pane-min-height)] lg:flex-1"
      onKeyDown={onLedgerKeyDown}
    >
      <div className="flex min-h-[var(--touch-target-min)] shrink-0 flex-wrap items-center gap-x-2.5 gap-y-1 border-b border-edge-subtle px-3">
        <span className="td-title">Fact ledger</span>
        <span aria-hidden className="td-rule" />
        <span className="text-3xs text-text-muted max-md:hidden">
          exact rows · hover inspects · click or Enter selects
        </span>
        <FactSortControl sort={sort} onSort={onSort} />
      </div>
      {/* Named, because internal scrolling is licensed for LABELLED regions
        * only, and this is the element that actually scrolls. Empty and refused
        * readings render no focusable rows, so it stays keyboard-operable. */}
      <div role="region" aria-label="Fact rows" tabIndex={0} className="min-h-0 flex-1 overflow-auto">
        {body}
      </div>
    </section>
  );
}

/** Roving arrows over the ledger rows: rows are native buttons, so Enter and
 * Space activate for free; arrows, Home, End and Page keys move focus, and
 * with it inspection, without a Tab through every row. */
function onLedgerKeyDown(event: KeyboardEvent<HTMLElement>) {
  const container = event.currentTarget;
  const rows = [...container.querySelectorAll<HTMLButtonElement>('button[aria-pressed]')];
  if (rows.length === 0) return;
  const active = document.activeElement;
  const current = active instanceof HTMLButtonElement ? rows.indexOf(active) : -1;
  const last = rows.length - 1;
  const from = current < 0 ? 0 : current;
  let next: number;
  switch (event.key) {
    case 'Home':
      next = 0;
      break;
    case 'End':
      next = last;
      break;
    case 'PageDown':
      next = Math.min(from + 10, last);
      break;
    case 'PageUp':
      next = Math.max(from - 10, 0);
      break;
    case 'ArrowDown':
      next = Math.min(current + 1, last);
      break;
    case 'ArrowUp':
      next = Math.max(current - 1, 0);
      break;
    default:
      return;
  }
  event.preventDefault();
  rows[next]?.focus();
  rows[next]?.scrollIntoView({ block: 'nearest' });
}

/** The store's headline counts and its encoding algebra, beside the search
 * field. Facts is the quantity this workspace exists to report, so it takes
 * the display tier; the algebra is store-level context that stays on screen
 * whichever fact is open. */
function StoreReadouts({
  summary,
  statusMemory,
}: {
  summary: MemoryHolographicPayloadV1['overview'] | null;
  statusMemory: MemoryStatusV1 | undefined;
}) {
  const factCount = splitCount(summary?.facts);
  const entityCount = splitCount(summary?.entities);
  return (
    <div className="flex flex-wrap items-end gap-4 border-l border-edge-subtle pl-4">
      <Readout
        label="facts"
        size="md"
        value={factCount.value}
        unit={factCount.unit}
        note={summary?.facts != null ? `${summary.facts.toLocaleString()} recorded` : 'not reported'}
      />
      <Readout label="entities" size="md" value={entityCount.value} unit={entityCount.unit} />
      <Readout
        label="categories"
        size="md"
        value={summary ? summary.categories.length.toLocaleString() : '—'}
      />
      <Readout
        label="memory algebra"
        size="sm"
        value={statusMemory ? statusMemory.algebra.name : '—'}
        note={
          statusMemory
            ? `${statusMemory.algebra.hrr_dim.toLocaleString()} dimensions · estimated capacity ${statusMemory.algebra.estimated_capacity.toLocaleString()}`
            : 'status has not answered'
        }
        className="max-sm:hidden"
      />
    </div>
  );
}

/** What the right bay shows when no fact is inspected: the store as a whole.
 * The trust distribution and its denominator, the encoding algebra, the
 * category census and the growth series, each from its own read, each
 * printing what its counts cover. */
function StoreSummary({
  summary,
  statusMemory,
  distribution,
}: {
  summary: MemoryHolographicPayloadV1['overview'] | null;
  statusMemory: MemoryStatusV1 | undefined;
  distribution: TrustDistribution;
}) {
  const categories = [...(summary?.categories ?? [])].sort((a, b) => b.count - a.count);
  const categoryCeiling = categories.reduce((max, row) => Math.max(max, row.count), 0);
  const growth = summary?.growth ?? [];
  return (
    <div className="flex flex-col" data-testid="store-summary">
      <header className="flex min-h-10 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5 py-2">
        <span className="flex min-w-0 flex-col gap-0.5">
          <span className="text-2xs uppercase tracking-[0.08em] text-text-muted">no fact inspected</span>
          <h2 className="td-title truncate">Store summary</h2>
        </span>
        <span aria-hidden className="td-rule" />
      </header>
      <div role="region" aria-label="Store summary" tabIndex={0} className="flex min-h-0 flex-1 flex-col gap-4 overflow-auto p-2.5">
        <p className="text-2xs leading-relaxed text-text-muted">
          Hover or focus a fact in the cameras or the ledger to inspect its bounded row;
          click or press Enter to select it and read its canonical detail and trust audit.
        </p>
        <TrustDistributionPlate distribution={distribution} />
        {statusMemory ? (
          <figure className="flex flex-col gap-1.5">
            <figcaption className="td-legend">feedback funnel</figcaption>
            <p className="text-3xs text-text-muted">
              {statusMemory.feedback_funnel.rated_fact_count.toLocaleString()} rated of{' '}
              {statusMemory.feedback_funnel.retrieved_fact_count.toLocaleString()} retrieved
              {' · '}
              {statusMemory.feedback_funnel.feedback_total.toLocaleString()} feedback events
            </p>
          </figure>
        ) : null}
        {categories.length > 0 ? (
          <figure className="flex flex-col gap-2">
            <figcaption className="td-legend">facts by category</figcaption>
            <div className="flex flex-col gap-2">
              {categories.map((row) => (
                <CategoryBar key={row.category} row={row} ceiling={categoryCeiling} />
              ))}
            </div>
          </figure>
        ) : null}
        {growth.length > 0 ? <GrowthChart growth={growth} /> : null}
      </div>
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
      {/* Twelve weekly dates at 9px in a narrow rail is unreadable debris, so
       * the shape carries the trend and the two endpoints are printed directly
       * underneath instead of a rotated, truncated axis. */}
      <Chart
        ariaLabel={`Cumulative facts recorded across ${growth.length} periods, from ${first.date} (${first.cumulative_facts.toLocaleString()} facts) to ${last.date} (${last.cumulative_facts.toLocaleString()} facts)`}
        height={70}
        option={option}
      />
      <div aria-hidden className="flex items-start justify-between gap-2 border-t border-edge-subtle pt-1.5">
        <Readout label={formatShortDate(first.date)} value={formatCount(first.cumulative_facts)} size="sm" />
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
 * single band there is no shape to draw, so the reading is stated instead , 
 * one full bar beside nine empty ones is the same non-information in a more
 * confident costume.
 */
function TrustDistributionPlate({ distribution }: { distribution: TrustDistribution }) {
  if (distribution.source === 'none') {
    return (
      <figure className="flex flex-col gap-1">
        <figcaption className="td-legend">trust distribution</figcaption>
        <p className="text-2xs leading-relaxed text-text-muted">
          The store reported no trust distribution, not a distribution of zero, but no reading
          at all.
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
          <span className="td-value text-text-primary">{only.label}</span>. There is no spread to
          draw.
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
            <span className="td-value w-16 shrink-0 text-3xs text-text-muted" data-cell="numeric">
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
        {distribution.total.toLocaleString()} facts across {distribution.bands.length} bands,{' '}
        {distribution.occupied} of them occupied · {trustSourceNote(distribution.source)}
      </figcaption>
    </figure>
  );
}

/** "2026-05-08" -> "May 8". The growth caption prints a date beside a facts
 * count in a narrow rail; the full ISO stamp alone (10 chars) leaves no room
 * for the count next to it before the two end labels collide. The full date
 * stays in the chart's `ariaLabel`, this is a display-only compaction, not a
 * different value. */
function formatShortDate(iso: string): string {
  const date = new Date(`${iso}T00:00:00Z`);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

/** One category's share of the loaded fact set, read the same way the fact
 * list itself is: a printed count for precision, a rail scaled to the busiest
 * category on screen for ranking. No fabricated denominator, the rail
 * measures against the largest category actually present, not an assumed
 * total. */
function CategoryBar({ row, ceiling }: { row: MemoryCategoryCountV1; ceiling: number }) {
  const fraction = ceiling > 0 ? row.count / ceiling : null;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 truncate text-2xs text-text-secondary">{row.category}</span>
        <span className="td-value text-2xs text-text-primary" data-cell="numeric">
          {formatCount(row.count)}
        </span>
      </div>
      <Meter fraction={fraction} ariaLabel={`${row.category}: ${row.count.toLocaleString()} facts`} />
    </div>
  );
}
