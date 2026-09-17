import { Suspense, lazy, useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { useSearchParams } from 'react-router';
import { ExplorerSplit } from '../../ui/archetypes/ExplorerSplit.tsx';
import { CenteredState } from '../../ui/ReadSection.tsx';
import { WorkspaceHeader } from '../../ui/instrument.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { cn } from '../../ui/cn';
import { displayName } from './hubs.ts';
import { envelopePayload, useEnvelope } from '../../data/query/useEnvelope.ts';
import { usePublishStatusRegisters } from '../../data/shell/statusRegisters.ts';
import { useActivationField } from '../../viz/graph/useActivationField.ts';
import { CodeDiagnostics } from './CodeDiagnostics.tsx';
import { CortexRelief } from './CortexRelief.tsx';
import {
  CODE_VIEW_PANEL_ID,
  CodeViewSwitcher,
  codeViewControlId,
  codeViewNote,
} from './CodeViewSwitcher.tsx';
import { CompareView } from './CompareView.tsx';
import { CortexField } from './CortexField.tsx';
import { CortexInspector, type InspectMode } from './CortexInspector.tsx';
import { SymbolMatches, TopConnectedList, type RowHandlers } from './CortexLedger.tsx';
import { CortexRegister } from './CortexRegister.tsx';
import { IndexFreshness, useIndexFreshness } from './IndexFreshness.tsx';
import { SharedCodeView } from './SharedCodeView.tsx';
import { Strata } from './Strata.tsx';
import { SymbolPath } from './SymbolPath.tsx';
import type { TraceFocus } from './TraceView.tsx';
import { TraceChunkFallback } from './TraceChunkFallback.tsx';
import { graphRegister, indexRegister, selectionRegister } from './codeRegisters.ts';
import {
  codeViewBlocker,
  readCodeLocation,
  writeCodeLocation,
  type CodeView,
} from './codeView.ts';
import { readCompareSelection, writeCompareSelection } from './compareLayout.ts';
import {
  type GraphNodeV1,
  GraphOverviewPayloadV1Schema,
  GraphSearchPayloadV1Schema,
  GraphSubgraphPayloadV1Schema,
} from '../../contracts/generated.ts';

// Imports live at the top of a module; a `lazy` dynamic import is the
// documented exception, because the point is that the module is NOT fetched
// until it is needed. The trace drill-in is a thousand lines plus the whole of
// `viz/trace` — canvas renderer, spring integrator, palette — and most visits
// to this workspace never open it, so it is its own chunk rather than dead
// weight in the spine's. The `TraceFocus` import above stays a normal
// top-level type import: types are erased, so it costs nothing at runtime.
const TraceView = lazy(() =>
  import('./TraceView.tsx').then((m) => ({ default: m.TraceView })),
);

const BASE = '/api/plugins/graph';

/** What the inspector is previewing, if anything: a row the pointer or focus
 * is on. The row itself rides along when the source had one, so a caller
 * listed in the inspector — a symbol the drawn slice may not contain — can be
 * previewed without another read. */
interface Inspection {
  readonly id: string;
  readonly node: GraphNodeV1 | null;
}

/** Which reading the ledger under the aperture shows. View-local; not URL. */
type LedgerTab = 'symbols' | 'relief';

/**
 * Code: the indexed graph as one semantic cortex.
 *
 * The register across the top states the whole index; the aperture draws one
 * slice of it — the busiest connected region unseeded, the pinned symbol's
 * neighbourhood when one is pinned; the ledger beneath is the slice's exact
 * keyboard equivalent; the inspector reads the pinned or previewed symbol
 * against every independent authority. Hover inspects, click pins, and the
 * URL is the one selection authority: every pin writes it, so a pasted link
 * or a back navigation can never be retargeted by an older in-memory row.
 */
export function CodePage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const location = readCodeLocation(searchParams);
  const overview = useEnvelope(
    ['graph', 'overview'],
    `${BASE}/overview`,
    GraphOverviewPayloadV1Schema,
  );
  const freshness = useIndexFreshness();
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const search = useEnvelope(
    ['graph', 'search', submitted],
    `${BASE}/search?q=${encodeURIComponent(submitted)}&limit=100`,
    GraphSearchPayloadV1Schema,
    {
      enabled: submitted !== '',
      activity:
        submitted === ''
          ? undefined
          : {
              id: 'code-symbol-search',
              label: 'Searching indexed symbols',
              cancelable: true,
            },
    },
  );
  const [selected, setSelected] = useState<TraceFocus | null>(null);
  const [inspection, setInspection] = useState<Inspection | null>(null);
  const [ledgerTab, setLedgerTab] = useState<LedgerTab>('symbols');
  const focusId = location.focusId;
  const subgraph = useEnvelope(
    ['graph', 'subgraph', focusId ?? ''],
    `${BASE}/subgraph${focusId ? `?node_id=${encodeURIComponent(focusId)}` : ''}`,
    GraphSubgraphPayloadV1Schema,
  );
  const subgraphPayload = envelopePayload(subgraph.data);
  const resolvedFocus =
    selected !== null && selected.id === focusId
      ? selected
      : (subgraphPayload?.nodes.find((node) => node.id === focusId) ?? null);
  const navigateView = useCallback(
    (view: CodeView, node: TraceFocus | null = resolvedFocus) => {
      const nextFocus = node?.id ?? location.focusId;
      setSearchParams(
        writeCodeLocation(searchParams, {
          view,
          focusId: nextFocus,
        }),
        { replace: true },
      );
    },
    [location.focusId, resolvedFocus, searchParams, setSearchParams],
  );
  const canvasNodes = useMemo(() => {
    const payload = envelopePayload(subgraph.data);
    if (!payload) return [];
    return payload.nodes.map((node) => ({
      id: node.id,
      label: node.name ?? node.qualified_name ?? node.id,
      kind: node.kind,
      degree: node.degree ?? undefined,
    }));
  }, [subgraph.data]);
  const canvasEdges = useMemo(() => {
    const payload = envelopePayload(subgraph.data);
    if (!payload) return [];
    return payload.edges.map((edge) => ({
      source: edge.source,
      target: edge.target,
      kind: edge.kind,
    }));
  }, [subgraph.data]);
  const activation = useActivationField(3200);
  // Search results strike their nodes: querying the graph makes it fire.
  useEffect(() => {
    const payload = envelopePayload(search.data);
    if (!payload) return;
    const hits = (payload.results ?? []).map((node) => node.id);
    if (hits.length) activation.strike(hits, 0.9);
  }, [search.data, activation]);

  /** Pin a symbol: the URL identity moves, the in-memory row is kept only as a
   * cache for the same id, and the lens stays where it is unless it needs a
   * different symbol than the one it was reading. */
  const pin = useCallback(
    (node: GraphNodeV1, view: CodeView = location.view === 'atlas' ? 'cortex' : location.view) => {
      setSelected(node);
      setInspection(null);
      setSearchParams(writeCodeLocation(searchParams, { view, focusId: node.id }), {
        replace: true,
      });
    },
    [location.view, searchParams, setSearchParams],
  );
  const clearSelection = useCallback(() => {
    setSelected(null);
    setInspection(null);
    setSearchParams(writeCodeLocation(searchParams, { view: 'cortex', focusId: null }), {
      replace: true,
    });
  }, [searchParams, setSearchParams]);
  const selectFromCanvas = useCallback(
    (id: string | null) => {
      if (id == null) {
        clearSelection();
        return;
      }
      const node = envelopePayload(subgraph.data)?.nodes.find((candidate) => candidate.id === id);
      if (node) pin(node, 'cortex');
    },
    [clearSelection, pin, subgraph.data],
  );
  const inspectId = useCallback(
    (id: string | null, node?: GraphNodeV1) =>
      setInspection(id === null ? null : { id, node: node ?? null }),
    [],
  );
  const rowHandlers = useMemo<RowHandlers>(
    () => ({
      onInspect: (node) => setInspection({ id: node.id, node }),
      onPin: (node) => pin(node, 'cortex'),
    }),
    [pin],
  );
  // A copy listed in Shared Code is itself a symbol occurrence in the graph, so
  // re-centring on it is the same URL write every other selection makes: the
  // identity moves, the view stays, and the in-memory row is dropped so the
  // subgraph read resolves the new identity rather than an older row.
  const focusOccurrence = useCallback(
    (view: CodeView, symbolOccurrenceId: string) => {
      setSelected(null);
      setSearchParams(
        writeCodeLocation(searchParams, { view, focusId: symbolOccurrenceId }),
        { replace: true },
      );
    },
    [searchParams, setSearchParams],
  );
  const submitSearch = useCallback(() => {
    setSubmitted(query.trim());
    // Matches are listed in the Cortex ledger, so a search from another lens
    // opens the lens that can show its answer.
    if (location.view !== 'cortex') navigateView('cortex');
  }, [location.view, navigateView, query]);

  const focusState =
    focusId === null
      ? 'absent'
      : resolvedFocus !== null
        ? 'available'
        : subgraph.isPending
          ? 'loading'
          : 'unavailable';
  const viewBlocker = codeViewBlocker(location.view, focusState);
  const compareSelection = readCompareSelection(searchParams);

  // The inspector's subject: a preview when the pointer or focus is on a row
  // that is not the pinned symbol, otherwise the pinned symbol itself. The
  // preview resolves to a full row from whatever payload holds it.
  const inspected = useMemo<{ node: TraceFocus; mode: InspectMode } | null>(() => {
    if (inspection !== null && inspection.id !== resolvedFocus?.id) {
      const node =
        inspection.node ??
        subgraphPayload?.nodes.find((candidate) => candidate.id === inspection.id) ??
        envelopePayload(search.data)?.results.find((candidate) => candidate.id === inspection.id) ??
        envelopePayload(overview.data)?.top_connected.find(
          (candidate) => candidate.id === inspection.id,
        ) ??
        null;
      if (node) return { node, mode: 'preview' };
    }
    return resolvedFocus ? { node: resolvedFocus, mode: 'pinned' } : null;
  }, [inspection, overview.data, resolvedFocus, search.data, subgraphPayload]);

  usePublishStatusRegisters('code', [
    graphRegister(overview.isPending, overview.data),
    indexRegister(freshness.isPending, freshness.data),
    selectionRegister(resolvedFocus),
  ]);

  const inspector = inspected ? (
    <CortexInspector
      node={inspected.node}
      mode={inspected.mode}
      pinned={resolvedFocus}
      drawnEdges={subgraphPayload?.edges ?? []}
      graphResult={subgraph.data}
      tracing={location.view === 'trace'}
      onInspect={inspectId}
      onPin={(node) => pin(node)}
      onTrace={(node) => navigateView('trace', node)}
      onSharedCode={(node) => navigateView('shared-code', node)}
      onClose={clearSelection}
    />
  ) : null;

  return (
    <div
      className="flex flex-col lg:h-full lg:min-h-0"
      onKeyDown={(event) => {
        // Escape drops a hover/focus preview and nothing else: selection is
        // URL state and has its own close control.
        if (event.key === 'Escape' && inspection !== null) setInspection(null);
      }}
    >
      <WorkspaceHeader
        path="code"
        title="Code"
        note={`${location.view === 'cortex' ? 'cortex' : location.view} · ${codeViewNote(location.view)}`}
      />
      <div className="flex flex-wrap items-center gap-2 border-b border-edge-subtle bg-surface-1 pr-2">
        <CodeViewSwitcher
          active={location.view}
          focusAvailable={focusState === 'available'}
          onSelect={navigateView}
        />
        <div className="ml-auto flex min-w-56 max-w-xl flex-1 items-center py-1">
          <SearchField
            value={query}
            onChange={setQuery}
            onSubmit={submitSearch}
            onClear={() => {
              setQuery('');
              setSubmitted('');
            }}
            label="Symbol search"
            placeholder="Search symbols, paths, qualified names"
            submitted={submitted}
          />
        </div>
      </div>
      <div
        id={CODE_VIEW_PANEL_ID}
        role="region"
        aria-labelledby={codeViewControlId(location.view)}
        className="flex min-h-0 flex-1 flex-col"
      >
        {viewBlocker !== null ? (
          <CenteredState
            title={viewBlocker.title}
            detail={viewBlocker.detail}
            kind={viewBlocker.kind}
          />
        ) : location.view === 'cortex' ? (
          <CortexLens
            register={<CortexRegister pending={overview.isPending} result={overview.data} />}
            field={
              <CortexField
                pending={subgraph.isPending}
                result={subgraph.data}
                nodes={canvasNodes}
                edges={canvasEdges}
                selectedId={resolvedFocus?.id ?? null}
                inspectedId={inspection?.id ?? null}
                onSelect={selectFromCanvas}
                onInspect={inspectId}
                activation={activation}
                totalNodes={envelopePayload(overview.data)?.totals.nodes ?? null}
                seedLabel={resolvedFocus ? displayName(resolvedFocus) : null}
              />
            }
            ledgerTab={ledgerTab}
            onLedgerTab={setLedgerTab}
            ledger={
              ledgerTab === 'relief' ? (
                <CortexRelief focusPath={resolvedFocus?.file_path ?? null} />
              ) : submitted === '' ? (
                <TopConnectedList
                  overviewPending={overview.isPending}
                  overviewResult={overview.data}
                  selectedId={resolvedFocus?.id ?? null}
                  inspectedId={inspection?.id ?? null}
                  handlers={rowHandlers}
                />
              ) : (
                <SymbolMatches
                  pending={search.isPending}
                  result={search.data}
                  submitted={submitted}
                  selectedId={resolvedFocus?.id ?? null}
                  inspectedId={inspection?.id ?? null}
                  handlers={rowHandlers}
                />
              )
            }
            inspector={inspector}
          />
        ) : (
          <ExplorerSplit
            list={
              <div className="flex h-full min-h-0 flex-col">
                {location.view === 'shared-code' && resolvedFocus !== null ? (
                  <SharedCodeView
                    focus={resolvedFocus}
                    onFocusMember={(id) => focusOccurrence('shared-code', id)}
                    onTraceMember={(id) => focusOccurrence('trace', id)}
                  />
                ) : location.view === 'compare' ? (
                  <CompareView
                    selection={compareSelection}
                    onSelectionChange={(selection) =>
                      setSearchParams(writeCompareSelection(searchParams, selection), {
                        replace: true,
                      })
                    }
                  />
                ) : location.view === 'trace' && resolvedFocus !== null ? (
                  <Suspense
                    fallback={
                      <TraceChunkFallback
                        focus={resolvedFocus}
                        onClose={() => navigateView('cortex')}
                      />
                    }
                  >
                    <TraceView
                      focus={resolvedFocus}
                      onClose={() => navigateView('cortex')}
                      onFocusChange={(node) => {
                        setSelected(node);
                        navigateView('trace', node);
                      }}
                    />
                  </Suspense>
                ) : null}
              </div>
            }
            inspector={inspector ?? undefined}
          />
        )}
      </div>
    </div>
  );
}

/**
 * The Cortex lens's geometry: register over aperture over ledger in the main
 * column, the inspector and the independent authorities in a rail beside it.
 * From `lg` the lens is one fixed instrument that fills the workspace and the
 * rail scrolls; below it the regions stack and the page scrolls.
 */
function CortexLens({
  register,
  field,
  ledger,
  ledgerTab,
  onLedgerTab,
  inspector,
}: {
  register: ReactNode;
  field: ReactNode;
  ledger: ReactNode;
  ledgerTab: LedgerTab;
  onLedgerTab: (tab: LedgerTab) => void;
  inspector: ReactNode;
}) {
  return (
    <div className="flex min-h-0 flex-1 flex-col lg:flex-row" data-code-lens="cortex">
      <div className="flex min-w-0 flex-1 flex-col lg:min-h-0">
        {register}
        {field}
        <section
          aria-label="Symbol ledger"
          className="flex shrink-0 flex-col border-t border-edge-subtle lg:h-64 lg:min-h-0"
        >
          <div
            role="tablist"
            aria-label="Ledger reading"
            className="flex h-8 shrink-0 items-center gap-1 border-b border-edge-subtle bg-surface-1 px-1"
          >
            <LedgerTabButton
              tab="symbols"
              active={ledgerTab}
              onSelect={onLedgerTab}
              label="Symbols"
              note="exact rows for the field"
            />
            <LedgerTabButton
              tab="relief"
              active={ledgerTab}
              onSelect={onLedgerTab}
              label="Module relief"
              note="dependency depth terrain"
            />
          </div>
          <div
            role="tabpanel"
            id={`code-ledger-${ledgerTab}`}
            aria-labelledby={`code-ledger-tab-${ledgerTab}`}
            // Scrollable regions need keyboard operation; an empty or refused
            // ledger renders no focusable rows, so the panel takes the stop.
            tabIndex={0}
            className="min-h-0 flex-1 lg:overflow-auto"
          >
            {ledger}
          </div>
        </section>
      </div>
      <aside
        aria-label="Inspector"
        className="flex w-full shrink-0 flex-col border-t border-edge-subtle bg-surface-1 lg:w-[22rem] lg:min-h-0 lg:overflow-auto lg:border-l lg:border-t-0 xl:w-[24rem]"
      >
        {inspector ?? <NoSelection />}
        <section
          aria-label="Code authorities"
          className="flex flex-col gap-4 border-t border-edge-subtle p-2.5"
        >
          <IndexFreshness />
          <CodeDiagnostics />
          <Strata />
          <SymbolPath />
        </section>
      </aside>
    </div>
  );
}

function LedgerTabButton({
  tab,
  active,
  onSelect,
  label,
  note,
}: {
  tab: LedgerTab;
  active: LedgerTab;
  onSelect: (tab: LedgerTab) => void;
  label: string;
  note: string;
}) {
  const selected = tab === active;
  return (
    <button
      type="button"
      role="tab"
      id={`code-ledger-tab-${tab}`}
      aria-selected={selected}
      aria-controls={selected ? `code-ledger-${tab}` : undefined}
      tabIndex={selected ? 0 : -1}
      onClick={() => onSelect(tab)}
      onKeyDown={(event) => {
        if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
          event.preventDefault();
          const next: LedgerTab = tab === 'symbols' ? 'relief' : 'symbols';
          onSelect(next);
          document.getElementById(`code-ledger-tab-${next}`)?.focus();
        }
      }}
      title={note}
      className={cn(
        'flex h-full items-center gap-2 border-b-2 px-3 text-2xs uppercase tracking-[0.12em]',
        'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
        selected
          ? 'border-accent text-text-primary'
          : 'border-transparent text-text-muted hover:text-text-secondary',
      )}
    >
      {label}
    </button>
  );
}

function NoSelection() {
  return (
    <div className="flex flex-col gap-2 p-3 text-xs text-text-muted" data-inspector-empty>
      <h2 className="td-title text-text-primary">Selection</h2>
      <p>No symbol is pinned.</p>
      <p>
        Hover or focus a symbol on the field or in the ledger to inspect it here. Click or
        Enter pins it, re-seeds the field on its neighbourhood, and reads its callers and
        callees. Escape drops a preview.
      </p>
    </div>
  );
}
