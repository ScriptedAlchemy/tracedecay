/**
 * The Cortex ledger: the field's exact, keyboard-reachable equivalent.
 *
 * Two readings share the strip under the aperture. With no search applied it
 * lists the graph's most connected symbols, the endpoint's twelve degree
 * leaders, drawn as one spine and a ranked grid of cards. With a search
 * applied it lists the matches, capped as the route caps them and said so.
 *
 * Every row obeys the field's grammar: hover or focus INSPECTS (the inspector
 * previews the row without moving selection), click PINS (the URL identity
 * changes and the field re-seeds). Nothing here fires activity.
 */
import { useMemo } from 'react';

import type {
  GraphNodeV1,
  GraphOverviewPayloadV1,
  GraphSearchPayloadV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { DataRow } from '../../ui/archetypes/ExplorerSplit.tsx';
import { CenteredState, ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { cn } from '../../ui/cn';
import { elideStart } from '../../ui/format.ts';
import { FigureRail } from '../../ui/instrument.tsx';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { ambiguityNote, annotateHubs, displayName } from './hubs.ts';

/** How a row reports itself to the page: inspect on hover/focus, pin on click. */
export interface RowHandlers {
  onInspect: (node: GraphNodeV1) => void;
  onPin: (node: GraphNodeV1) => void;
}

/** The symbol search results, as the accessible equivalent of the canvas above
 * them. The header states whether the rows are the whole match set, because the
 * route caps them and a capped page presented as a total would understate the
 * graph. */
export function SymbolMatches({
  pending,
  result,
  submitted,
  selectedId,
  inspectedId,
  handlers,
}: {
  pending: boolean;
  result: EnvelopeResult<GraphSearchPayloadV1> | undefined;
  submitted: string;
  selectedId: string | null;
  inspectedId: string | null;
  handlers: RowHandlers;
}) {
  return (
    <ReadSection
      title="Symbols"
      chrome="centered"
      state={envelopeReadState(pending, result, {
        loading: 'searching symbols',
        transport: 'symbol search could not be read',
      })}
    >
      {(envelope) => {
        const data = envelope.payload;
        const rows = data.results ?? [];
        if (rows.length === 0)
          return (
            <CenteredState
              title={`No symbol matches ${submitted}`}
              kind="complete_zero_findings"
            />
          );
        const capped = data.total != null && data.total > rows.length;
        const degreeCeiling = rows.reduce((max, node) => Math.max(max, node.degree ?? 0), 0);
        return (
          <VirtualList
            items={rows}
            getKey={(node) => node.id}
            header={
              <p className="td-legend border-b border-edge-subtle px-3 py-2">
                {capped
                  ? `${rows.length} of ${data.total} matches`
                  : `${data.total ?? rows.length} matches`}
              </p>
            }
            renderItem={(node) => (
              <SymbolRow
                node={node}
                degreeCeiling={degreeCeiling}
                selected={selectedId === node.id}
                inspected={inspectedId === node.id}
                handlers={handlers}
              />
            )}
          />
        );
      }}
    </ReadSection>
  );
}

/** Default reading when no search is active: the graph's most connected
 * symbols, drawn as two joined instruments.
 *
 *   the SPINE   every hub as a mark on one shared degree axis, so position is
 *               the measurement and the shape of the drop is directly visible.
 *               Hue is the symbol's kind, from the same `kindColor` rule the
 *               canvas above paints with.
 *
 *   the FIELD   the same hubs as cards on a dense grid, where the NAME's type
 *               size falls with rank, magnitude read as typography, costing
 *               no horizontal column.
 *
 * The endpoint serves at most twelve generation-pinned degree leaders. So the
 * spine is captioned as those hubs only, its axis is anchored at zero and
 * labelled with the real extremes, and nothing here claims to be the whole
 * graph's degree distribution. Search reaches everything else. */
export function TopConnectedList({
  overviewPending,
  overviewResult,
  selectedId,
  inspectedId,
  handlers,
}: {
  overviewPending: boolean;
  overviewResult: EnvelopeResult<GraphOverviewPayloadV1> | undefined;
  selectedId: string | null;
  inspectedId: string | null;
  handlers: RowHandlers;
}) {
  return (
    <ReadSection
      title="Most connected symbols"
      chrome="centered"
      state={envelopeReadState(overviewPending, overviewResult, {
        loading: 'reading connected symbols',
        transport: 'connected symbols could not be read',
      })}
    >
      {(envelope) => {
        const payload = envelope.payload;
        const hubs = payload.top_connected;
        if (hubs.length === 0)
          return (
            <CenteredState
              title="No connected symbols are indexed for this project"
              kind="complete_zero_findings"
            />
          );
        return (
          <HubField
            hubs={hubs}
            indexedNodes={payload.totals.nodes}
            selectedId={selectedId}
            inspectedId={inspectedId}
            handlers={handlers}
          />
        );
      }}
    </ReadSection>
  );
}

/** How a hub's degree is drawn on the spine, and how big its name is set in
 * the field below. Both derive from the same rank/degree pair, so the two
 * instruments cannot disagree about which symbol matters most. */
function markDiameter(degree: number, ceiling: number): number {
  // Area, not diameter, tracks the value: a disc twice as wide reads as four
  // times as much, so degree goes through a square root before it becomes a
  // width. Floor of 6px keeps the twelfth hub a body rather than a speck.
  return 6 + 12 * Math.sqrt(ceiling > 0 ? Math.max(0, degree) / ceiling : 0);
}

/** Type tier by rank. Discrete steps rather than a continuous scale: four
 * legible sizes read as a hierarchy, whereas twelve near-identical ones read
 * as sloppy typesetting. */
function nameTier(rank: number): string {
  if (rank === 0) return 'td-display text-xl';
  if (rank < 3) return 'td-value text-base font-medium';
  if (rank < 6) return 'td-value text-sm';
  return 'td-value text-xs';
}

function HubField({
  hubs,
  indexedNodes,
  selectedId,
  inspectedId,
  handlers,
}: {
  hubs: GraphNodeV1[];
  indexedNodes: number | null;
  selectedId: string | null;
  inspectedId: string | null;
  handlers: RowHandlers;
}) {
  // The endpoint already orders by degree, but the view's whole grammar is
  // rank, so it establishes rank itself rather than trusting arrival order.
  const ranked = useMemo(
    () =>
      hubs
        .filter((node) => typeof node.degree === 'number')
        .sort((a, b) => (b.degree ?? 0) - (a.degree ?? 0)),
    [hubs],
  );
  if (ranked.length === 0)
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        the graph reported hubs without connection counts, so there is nothing
        to rank
      </p>
    );
  const ceiling = ranked[0]?.degree ?? 0;
  const floor = ranked[ranked.length - 1]?.degree ?? 0;
  const leadName = displayName(ranked[0]);
  const tailName = displayName(ranked[ranked.length - 1]);
  // Eight of the twelve hubs on a real Rust graph are language primitives or
  // one-word generics, `path`, `json`, `u64`, `Value`, `trim`, `kind`, and
  // two of them are literally the same word. `qualified_name` is not served on
  // this route, so the file is the only thing that can tell them apart.
  const annotated = annotateHubs(ranked);
  const ambiguity = ambiguityNote(annotated);

  return (
    <div className="flex flex-col">
      <div className="flex items-center gap-2.5 border-b border-edge-subtle px-3 py-2">
        <span className="td-legend">most connected symbols</span>
        <span aria-hidden className="td-rule" />
        <span className="td-legend shrink-0 normal-case tracking-normal max-sm:hidden">
          {indexedNodes != null
            ? `top ${ranked.length} of ${indexedNodes.toLocaleString()} by degree`
            : `top ${ranked.length} by degree`}
        </span>
      </div>

      {/* ---- the spine ---------------------------------------------------
       * One axis, one mark per hub, position = connection count. The marks
       * are not controls: reading them is the point, and the ranked cards
       * below are their exact accessible and keyboard equivalent. */}
      <figure className="flex flex-col gap-1.5 border-b border-edge-subtle px-3 pb-1.5 pt-2">
        <div
          className="flex items-center gap-2"
          role="img"
          aria-label={`Connectivity spine: ${ranked.length} most connected symbols plotted by connection count on one axis from 0 to ${ceiling.toLocaleString()}. ${leadName} leads with ${ceiling.toLocaleString()} connections; the lowest ranked, ${tailName}, has ${floor.toLocaleString()}. The ranked cards below carry the same symbols.`}
        >
          <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
            0
          </span>
          <span className="relative mx-1.5 h-6 min-w-0 flex-1">
            <span
              aria-hidden
              className="absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-edge-strong"
            />
            <span
              aria-hidden
              className="absolute inset-x-0 top-1/2 h-1.5 opacity-70"
              style={{
                backgroundImage:
                  'repeating-linear-gradient(to right, var(--raw-edge-strong) 0 1px, transparent 1px 10%)',
              }}
            />
            {ranked.map((node, rank) => {
              const degree = node.degree ?? 0;
              const size = markDiameter(degree, ceiling);
              return (
                <span
                  key={node.id ?? rank}
                  aria-hidden
                  title={`${displayName(node)} · ${node.kind} · ${degree.toLocaleString()} deg, click to pin`}
                  onClick={() => handlers.onPin(node)}
                  onPointerEnter={() => handlers.onInspect(node)}
                  className="absolute top-1/2 cursor-pointer rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
                  style={{
                    ...kindColorVars(node.kind),
                    left: `${ceiling > 0 ? (degree / ceiling) * 100 : 0}%`,
                    width: size,
                    height: size,
                    transform: 'translate(-50%, -50%)',
                    zIndex: rank + 1,
                    boxShadow: '0 0 0 1.5px var(--raw-surface-0)',
                  }}
                />
              );
            })}
          </span>
          <span
            className="td-value shrink-0 text-3xs text-text-secondary"
            data-cell="numeric"
          >
            {ceiling.toLocaleString()}
            <span className="td-unit ml-1">deg</span>
          </span>
        </div>
        <figcaption className="flex items-baseline gap-3 leading-tight">
          <span className="min-w-0 flex-1 truncate text-3xs text-text-muted">
            position = connections · size = connections · hue = kind, on the
            same scale as the field above · axis anchored at zero
          </span>
          <span className="td-value shrink-0 text-2xs text-text-secondary">
            {leadName}
          </span>
        </figcaption>
      </figure>

      {ambiguity ? (
        <p className="border-b border-edge-subtle px-3 py-1.5 text-3xs leading-relaxed text-text-muted">
          {ambiguity}
        </p>
      ) : null}

      <ol className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 xl:grid-cols-4">
        {annotated.map(({ hub: node, display, module, file, ambiguous }, rank) => (
          <li
            key={node.id ?? rank}
            className={cn(
              'min-w-0 border-b border-l border-edge-subtle',
              rank === 0 && 'sm:col-span-2 xl:col-span-1',
            )}
          >
            <HubCard
              node={node}
              display={display}
              rank={rank}
              module={module}
              file={file}
              ambiguous={ambiguous}
              selected={selectedId === node.id}
              inspected={inspectedId === node.id}
              handlers={handlers}
            />
          </li>
        ))}
      </ol>
    </div>
  );
}

function HubCard({
  node,
  display,
  rank,
  module,
  file,
  ambiguous,
  selected,
  inspected,
  handlers,
}: {
  node: GraphNodeV1;
  /** The headline, already resolved by `annotateHubs`. */
  display: string;
  rank: number;
  /** Directory the symbol lives in, trailing slash included. */
  module: string;
  /** File name alone, the part that actually disambiguates. */
  file: string;
  /** Another card in this set carries the same name. */
  ambiguous: boolean;
  selected: boolean;
  inspected: boolean;
  handlers: RowHandlers;
}) {
  const degree = node.degree ?? 0;
  return (
    <button
      type="button"
      onClick={() => handlers.onPin(node)}
      onPointerEnter={() => handlers.onInspect(node)}
      onFocus={() => handlers.onInspect(node)}
      aria-pressed={selected}
      data-hub={node.id}
      data-inspected={inspected || undefined}
      className={cn(
        'relative flex h-full min-h-[var(--touch-target-min)] w-full flex-col gap-0.5 px-3 py-1.5 text-left',
        selected ? 'bg-surface-2' : 'bg-surface-0 hover:bg-surface-1',
        inspected && !selected && 'bg-surface-1',
        'focus-visible:bg-surface-1 focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
      )}
    >
      <span
        aria-hidden
        className={cn(
          'absolute inset-y-0 left-0 w-[2px]',
          selected ? 'bg-accent' : inspected ? 'bg-accent/40' : 'bg-transparent',
        )}
      />
      <span className="flex min-w-0 items-baseline gap-2 leading-tight">
        <span className="td-legend shrink-0" data-cell="numeric">
          {String(rank + 1).padStart(2, '0')}
        </span>
        <span
          aria-hidden
          className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
          style={kindColorVars(node.kind)}
        />
        <span
          className={cn('min-w-0 flex-1 truncate text-text-primary', nameTier(rank))}
          title={node.qualified_name ?? undefined}
        >
          {display}
        </span>
        <span
          className={cn(
            'td-value shrink-0 self-baseline text-text-secondary',
            rank === 0 ? 'text-sm' : 'text-2xs',
          )}
          data-cell="numeric"
        >
          {degree.toLocaleString()}
          <span className="td-unit ml-1">deg</span>
        </span>
      </span>
      <span className="flex min-w-0 items-baseline gap-2 pl-6 leading-tight">
        <span className="td-legend max-w-20 shrink-0 truncate">{node.kind}</span>
        <span
          className={cn(
            'td-value max-w-[60%] shrink-0 truncate text-2xs',
            ambiguous ? 'text-text-primary' : 'text-text-secondary',
          )}
          title={node.file_path ?? undefined}
        >
          {file || '—'}
        </span>
        <span
          className="td-value min-w-0 flex-1 truncate text-right text-3xs text-text-muted max-2xl:hidden"
          title={node.file_path ?? undefined}
        >
          {elideStart(module.replace(/\/$/, ''), 24)}
        </span>
      </span>
    </button>
  );
}

function SymbolRow({
  node,
  degreeCeiling,
  selected,
  inspected,
  handlers,
}: {
  node: GraphNodeV1;
  degreeCeiling: number;
  selected: boolean;
  inspected: boolean;
  handlers: RowHandlers;
}) {
  return (
    <div
      onPointerEnter={() => handlers.onInspect(node)}
      onFocus={() => handlers.onInspect(node)}
      data-symbol-row={node.id}
      data-inspected={inspected || undefined}
      className={cn(inspected && !selected && 'bg-surface-1')}
    >
      <DataRow selected={selected} onSelect={() => handlers.onPin(node)}>
        <span className="td-legend w-20 shrink-0 truncate max-md:hidden">{node.kind}</span>
        {/* Qualified name first, deliberately the reverse of `displayName`: the
         * search route does serve it, and in a list of matches the module path
         * is what separates two hits that share a bare name. */}
        <span className="td-value min-w-0 flex-1 truncate text-text-primary">
          {node.qualified_name ?? node.name ?? node.id}
        </span>
        {node.degree != null ? (
          <FigureRail
            value={node.degree}
            unit="deg"
            fraction={degreeCeiling > 0 ? node.degree / degreeCeiling : null}
          />
        ) : null}
        <span
          className="td-value w-52 shrink-0 truncate text-right text-2xs text-text-muted max-md:hidden"
          title={node.file_path ?? undefined}
        >
          {elideStart(node.file_path, 29)}
        </span>
      </DataRow>
    </div>
  );
}
