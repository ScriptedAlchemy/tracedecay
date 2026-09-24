import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react';
import { cn } from '../../ui/cn';
import { subagentElapsedSeconds } from './subagentTree.ts';
import { RingGlyph, RingHoverCard, ringCountLine, ringRadius } from './delegationRings.tsx';
import { UsageCoverageLegend, UsageLabel } from './sessionUsage.tsx';
import {
  TOPOLOGY_GEOMETRY,
  columnPitchFor,
  edgePath,
  fieldSize,
  markPosition,
  neighbourhood,
  subtree,
  type DelegationTopologyModel,
  type FittedTopology,
  type TopologyBundleMark,
  type TopologyGeometry,
  type TopologyMark,
  type TopologySessionMark,
} from './delegationTopology.ts';

/**
 * The delegation topology aperture: the subagent tree drawn left to right by
 * generation on the night-glass field, with every mark a real DOM button.
 *
 * The SVG underneath is decoration of the DOM above it. Circles, curves and
 * stubs give the field its shape; the buttons carry the names, the counts and
 * every interaction, so the picture is fully operable from the keyboard and
 * announces itself without a parallel twin. Column order is the tab order:
 * generation left to right, then top to bottom.
 *
 * Hover inspects and never selects. Pointer-over or focus on a mark raises it,
 * dims everything outside its path to the top and its drawn children, and asks
 * the page to show it in the inspector. Click or Enter selects, which is the
 * only act that persists and the only one that may cause a read. A bundle's
 * click opens it; a folded session's tail opens the generation beneath it.
 * Both are reader's acts recorded in `expanded`, never inferred from scroll or
 * viewport.
 *
 * Colour never carries a state alone. A cut edge is dashed as well as amber,
 * a cycle is hatched as well as hued, a bundle wears its count, and the
 * selected mark keeps a ring that survives a monochrome print.
 */

const HEADER_HEIGHT = 44;
const HIT = 44;

export interface TopologyInteraction {
  readonly inspectedId: string | null;
  readonly selectedId: string | null;
  readonly onInspect: (id: string | null) => void;
  readonly onSelect: (id: string) => void;
  readonly expanded: ReadonlySet<string>;
  readonly onToggleExpanded: (id: string) => void;
}

/** The aperture's own width, so columns can stretch to fill it. `null` until
 * measured, which under jsdom is forever, the default pitch then holds. */
export function useApertureWidth(ref: React.RefObject<HTMLDivElement | null>): number | null {
  const [width, setWidth] = useState<number | null>(null);
  useEffect(() => {
    const node = ref.current;
    if (node === null || typeof ResizeObserver === 'undefined') return;
    const measure = () => setWidth(node.clientWidth);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

export function DelegationTopology({
  fit,
  interaction,
}: {
  fit: FittedTopology;
  interaction: TopologyInteraction;
}) {
  const { model } = fit;
  const radiusOf = (mark: TopologyMark) => ringRadius(mark, model.maxDescendants);
  const { inspectedId, selectedId } = interaction;
  const apertureRef = useRef<HTMLDivElement | null>(null);
  const apertureWidth = useApertureWidth(apertureRef);
  const geometry = columnPitchFor(apertureWidth, model.columns);
  const size = fieldSize(model, geometry);
  const hatchId = useId();
  const byId = useMemo(() => new Map(model.marks.map((mark) => [mark.id, mark])), [model]);
  // An inspected id the model no longer draws, a bundle the reader just
  // opened, a session a refetch dropped, isolates nothing rather than
  // dimming everything.
  const keep = useMemo(
    () => (inspectedId !== null && byId.has(inspectedId) ? neighbourhood(model, inspectedId) : null),
    [model, byId, inspectedId],
  );
  // A selection lifts its subtree while nothing is being inspected; hover
  // answers its own question first and the lift returns when it ends.
  const lifted = useMemo(
    () => (keep === null && selectedId !== null && byId.has(selectedId) ? subtree(model, selectedId) : null),
    [keep, model, byId, selectedId],
  );
  const isDim = (id: string) => (keep !== null ? !keep.has(id) : lifted !== null && !lifted.has(id));

  return (
    <div className="flex min-w-0 flex-col gap-2" data-delegation-topology={model.drawnSessions}>
      <div
        ref={apertureRef}
        role="group"
        aria-label="Delegation topology field"
        // The field scrolls when a reading outgrows the aperture; a named
        // scroll container needs its own tab stop when no mark is drawn in
        // view, and the marks themselves take over as soon as they are.
        tabIndex={0}
        className="td-optic td-grain td-scanlines td-graticule relative max-h-[34rem] min-h-[16rem] overflow-auto"
        onMouseLeave={() => interaction.onInspect(null)}
        // Keyboard parity with the pointer: focus leaving the field ends
        // inspection the way the pointer leaving it does.
        onBlur={(event) => {
          const next = event.relatedTarget;
          if (!(next instanceof Node) || !event.currentTarget.contains(next)) {
            interaction.onInspect(null);
          }
        }}
      >
        <div
          className="relative"
          style={{ width: size.width, height: size.height + HEADER_HEIGHT, minWidth: '100%' }}
        >
          <GenerationHeaders model={model} fit={fit} geometry={geometry} />
          <svg
            aria-hidden
            width={size.width}
            height={size.height}
            className="absolute left-0"
            style={{ top: HEADER_HEIGHT, minWidth: '100%' }}
          >
            <defs>
              <pattern
                id={hatchId}
                width="5"
                height="5"
                patternUnits="userSpaceOnUse"
                patternTransform="rotate(45)"
              >
                <line x1="0" y1="0" x2="0" y2="5" stroke="var(--raw-state-conflicting)" strokeWidth="1.4" />
              </pattern>
            </defs>
            {model.generations.map((generation) => {
              const x = TOPOLOGY_GEOMETRY.padX + generation.generation * geometry.columnPitch;
              return (
                <line
                  key={generation.generation}
                  x1={x}
                  x2={x}
                  y1={0}
                  y2={size.height}
                  stroke="var(--raw-graph-edge)"
                  strokeOpacity={0.22}
                  strokeDasharray="1 6"
                />
              );
            })}
            {model.edges.map((edge) => {
              const from = byId.get(edge.from);
              const to = byId.get(edge.to);
              if (!from || !to) return null;
              const isolated = keep !== null && keep.has(edge.from) && keep.has(edge.to);
              const inLift = lifted !== null && lifted.has(edge.from) && lifted.has(edge.to);
              const lit = isolated || inLift;
              const dim = (keep !== null || lifted !== null) && !lit;
              return (
                <path
                  key={edge.id}
                  d={edgePath(
                    markPosition(from, geometry),
                    markPosition(to, geometry),
                    radiusOf(from),
                    radiusOf(to),
                  )}
                  fill="none"
                  stroke={lit ? 'var(--raw-graph-accent)' : 'var(--raw-graph-edge)'}
                  strokeWidth={lit ? 1.4 : 0.9}
                  strokeOpacity={dim ? 0.18 : isolated ? 1 : inLift ? 0.7 : 0.6}
                  strokeDasharray={edge.kind === 'bundle' ? '3 3' : undefined}
                  className="transition-[stroke-opacity] duration-[var(--dur-state)]"
                  data-topology-edge={edge.kind}
                  data-topology-lit={isolated ? 'true' : undefined}
                  data-topology-lifted={inLift ? 'true' : undefined}
                />
              );
            })}
            {model.stubs.map((stub) => {
              const to = byId.get(stub.to);
              if (!to) return null;
              const at = markPosition(to, geometry);
              const radius = radiusOf(to);
              const dim = isDim(stub.to);
              if (stub.kind === 'missing_parent') {
                return (
                  <g key={stub.id} opacity={dim ? 0.25 : 1} data-topology-stub="missing_parent">
                    <line
                      x1={at.x - radius - 52}
                      x2={at.x - radius}
                      y1={at.y}
                      y2={at.y}
                      stroke="var(--raw-graph-alert)"
                      strokeWidth={1.4}
                      strokeDasharray="4 3"
                    />
                    <circle
                      cx={at.x - radius - 56}
                      cy={at.y}
                      r={3.5}
                      fill="none"
                      stroke="var(--raw-graph-alert)"
                      strokeWidth={1.2}
                      strokeDasharray="2 2"
                    />
                  </g>
                );
              }
              const loop = `M${at.x - radius},${at.y - 2} C${at.x - radius - 22},${at.y - 26} ${at.x + radius + 22},${at.y - 26} ${at.x + radius},${at.y - 2}`;
              return (
                <path
                  key={stub.id}
                  d={loop}
                  fill="none"
                  stroke="var(--raw-state-conflicting)"
                  strokeWidth={1.4}
                  strokeDasharray="3 2"
                  opacity={dim ? 0.25 : 1}
                  data-topology-stub="cycle"
                />
              );
            })}
            {model.marks.map((mark) => (
              <RingGlyph
                key={mark.id}
                mark={mark}
                at={markPosition(mark, geometry)}
                radius={radiusOf(mark)}
                maxDescendants={model.maxDescendants}
                hatchId={hatchId}
                inspected={mark.id === inspectedId}
                selected={mark.id === selectedId}
                lifted={lifted !== null && lifted.has(mark.id) && mark.id !== selectedId}
                dim={isDim(mark.id)}
              />
            ))}
          </svg>
          <ul
            aria-label="Sessions and bundles by generation"
            className="absolute left-0 m-0 list-none p-0"
            style={{ top: HEADER_HEIGHT, width: size.width, height: size.height }}
          >
            {model.marks.map((mark) => (
              <li key={mark.id} className="contents">
                <MarkControl
                  mark={mark}
                  radius={radiusOf(mark)}
                  geometry={geometry}
                  interaction={interaction}
                  dim={isDim(mark.id)}
                  countLine={ringCountLine(mark)}
                />
              </li>
            ))}
          </ul>
          {inspectedId !== null && byId.has(inspectedId) ? (
            <div className="absolute left-0" style={{ top: HEADER_HEIGHT }}>
              <RingHoverCard
                mark={byId.get(inspectedId)!}
                at={markPosition(byId.get(inspectedId)!, geometry)}
                radius={radiusOf(byId.get(inspectedId)!)}
              />
            </div>
          ) : null}
        </div>
      </div>
      <RingLegend model={model} fit={fit} />
      <OpenedStrip model={model} onToggleExpanded={interaction.onToggleExpanded} />
    </div>
  );
}

/**
 * Everything the reader has opened, with a control to fold each one back.
 * Lives beside the legend rather than only in the inspector because an opened
 * bundle has no mark of its own any more, and the inspector's subject moves
 * with the pointer; this strip is reachable whatever is being inspected.
 */
export function OpenedStrip({
  model,
  onToggleExpanded,
}: {
  model: DelegationTopologyModel;
  onToggleExpanded: (id: string) => void;
}) {
  const opened: { id: string; label: string }[] = model.openedTopBundles.map((bundle) => ({
    id: bundle.id,
    label: `${bundle.sessions} × ${bundle.label}`,
  }));
  for (const mark of model.marks) {
    if (mark.kind !== 'session') continue;
    for (const bundle of mark.openedBundles) {
      opened.push({ id: bundle.id, label: `${bundle.sessions} × ${bundle.label} under ${mark.label}` });
    }
    if (mark.depthOpened) {
      opened.push({ id: mark.id, label: `${mark.node.descendants} beneath ${mark.label}` });
    }
  }
  if (opened.length === 0) return null;
  return (
    <ul
      aria-label="Opened bundles and generations"
      className="flex min-w-0 flex-wrap items-center gap-2 px-1"
      data-topology-opened={opened.length}
    >
      <li className="td-legend">opened</li>
      {opened.map((entry) => (
        <li key={entry.id} className="flex items-center">
          <button
            type="button"
            className="td-hit border border-edge-subtle bg-surface-2 px-2 text-2xs text-text-primary hover:border-accent"
            onClick={() => onToggleExpanded(entry.id)}
            aria-label={`Fold ${entry.label}`}
            data-topology-fold={entry.id}
          >
            {entry.label} · fold
          </button>
        </li>
      ))}
    </ul>
  );
}

/** Column captions across the top of the field: generation, its role, and
 * the reconciled count of what the column holds. */
function GenerationHeaders({
  model,
  fit,
  geometry,
}: {
  model: DelegationTopologyModel;
  fit: FittedTopology;
  geometry: TopologyGeometry;
}) {
  return (
    <ol
      aria-label="Generations"
      className="absolute inset-x-0 top-0 m-0 list-none p-0"
      style={{ height: HEADER_HEIGHT }}
    >
      {model.generations.map((generation) => {
        const x = TOPOLOGY_GEOMETRY.padX + generation.generation * geometry.columnPitch;
        const column = model.marks.filter((mark) => mark.generation === generation.generation);
        const tops =
          generation.generation === 0
            ? column.filter((mark): mark is TopologySessionMark => mark.kind === 'session')
            : [];
        const roots = tops.filter((mark) => mark.node.link === 'root').length;
        const cut = tops.filter((mark) => mark.node.link === 'missing_parent').length;
        const cycles = tops.filter((mark) => mark.node.link === 'cycle').length;
        const foldedHere = column.reduce(
          (sum, mark) => sum + (mark.kind === 'session' ? mark.foldedDescendants : 0),
          0,
        );
        const count =
          generation.generation === 0
            ? [
                `${roots} ${roots === 1 ? 'root' : 'roots'}`,
                cut > 0 ? `${cut} cut` : null,
                cycles > 0 ? `${cycles} ${cycles === 1 ? 'cycle' : 'cycles'}` : null,
                generation.bundled > 0 ? `${generation.bundled} bundled` : null,
              ]
                .filter(Boolean)
                .join(' · ')
            : [
                `${generation.sessions} ${generation.sessions === 1 ? 'session' : 'sessions'}`,
                generation.bundled > 0 ? `${generation.bundled} bundled` : null,
              ]
                .filter(Boolean)
                .join(' · ');
        const role =
          generation.generation === 0
            ? 'tops'
            : generation.generation === fit.depthLimit
              ? `delegates · ${foldedHere > 0 ? `${foldedHere} folded beneath` : 'deepest drawn'}`
              : 'delegates';
        return (
          <li
            key={generation.generation}
            className="absolute top-2 flex w-40 flex-col gap-0.5"
            style={{ left: x - 8, color: 'var(--raw-graph-text)' }}
            data-topology-generation={generation.generation}
          >
            <span className="td-legend" style={{ color: 'var(--raw-graph-accent)' }}>
              gen {generation.generation}
            </span>
            <span className="td-legend" style={{ color: 'var(--raw-graph-text)' }}>
              {role}
            </span>
            <span className="truncate font-mono text-3xs tabular-nums opacity-80">{count}</span>
          </li>
        );
      })}
    </ol>
  );
}

/** The operable mark: a 44px hit area over the disc, the label to its right,
 * and, for a folded session, the tail control that opens the generation
 * beneath. Hover and focus inspect; click and Enter select or open. */
function MarkControl({
  mark,
  radius,
  geometry,
  interaction,
  dim,
  countLine,
}: {
  mark: TopologyMark;
  radius: number;
  geometry: TopologyGeometry;
  interaction: TopologyInteraction;
  dim: boolean;
  /** The exact count the ring is sized by. */
  countLine: string;
}) {
  const at = markPosition(mark, geometry);
  const selected = mark.id === interaction.selectedId;
  const inspect = () => interaction.onInspect(mark.id);
  const commonStyle = { top: at.y - HIT / 2, left: at.x - HIT / 2 } as const;
  const labelOffset = Math.max(0, radius - HIT / 2) + 8;

  if (mark.kind === 'bundle') {
    // A drawn bundle is by construction a closed one: once opened, its
    // members are drawn in its place and the fold control lives on the
    // parent's inspector section.
    return (
      <button
        type="button"
        className={cn(
          'absolute flex min-h-[44px] items-center gap-0 text-left transition-opacity duration-[var(--dur-state)]',
          dim && 'opacity-40',
        )}
        style={{ ...commonStyle, color: 'var(--raw-graph-text)' }}
        aria-expanded={false}
        aria-label={`${bundleTitle(mark)}: ${mark.sessions} sessions in generation ${mark.generation}${mark.descendants > 0 ? `, ${mark.descendants} beneath them` : ''}. Open this bundle.`}
        onMouseEnter={inspect}
        onFocus={inspect}
        onClick={() => interaction.onToggleExpanded(mark.id)}
        data-topology-control="bundle"
        data-topology-id={mark.id}
      >
        <span aria-hidden className="block shrink-0" style={{ width: HIT, height: HIT }} />
        <span
          className="flex min-w-0 flex-col"
          style={{ marginLeft: labelOffset, maxWidth: geometry.columnPitch - HIT - 12 }}
        >
          <span className="truncate font-mono text-2xs tabular-nums">{bundleTitle(mark)}</span>
          <span className="td-legend" style={{ color: 'var(--raw-graph-text)', opacity: 0.75 }}>
            {countLine} · open
          </span>
          <span className="truncate text-3xs opacity-80">
            <UsageLabel mark={mark} />
          </span>
        </span>
      </button>
    );
  }

  const elapsed = subagentElapsedSeconds(mark.node);
  const detail = [
    countLine,
    mark.foldedDescendants > 0 ? `${mark.foldedDescendants} folded` : null,
    mark.node.provider,
    elapsed != null ? `${elapsed.toLocaleString()}s` : 'span unrecorded',
  ]
    .filter(Boolean)
    .join(' · ');
  const linkWord =
    mark.node.link === 'missing_parent'
      ? ', parent not in this reading'
      : mark.node.link === 'cycle'
        ? ', on a parent cycle'
        : '';
  return (
    <>
      <button
        type="button"
        className={cn(
          'absolute flex min-h-[44px] items-center text-left transition-opacity duration-[var(--dur-state)]',
          dim && 'opacity-40',
        )}
        style={{ ...commonStyle, color: 'var(--raw-graph-text)' }}
        aria-pressed={selected}
        aria-label={`${mark.label}, ${mark.node.provider} session ${mark.node.session_id}, generation ${mark.generation}${linkWord}. ${selected ? 'Selected.' : 'Select to read its token frontier.'}`}
        onMouseEnter={inspect}
        onFocus={inspect}
        onClick={() => interaction.onSelect(mark.id)}
        data-topology-control="session"
        data-topology-id={mark.id}
        data-topology-link={mark.node.link}
      >
        <span aria-hidden className="block shrink-0" style={{ width: HIT, height: HIT }} />
        <span
          className="flex min-w-0 flex-col"
          style={{ marginLeft: labelOffset, maxWidth: geometry.columnPitch - HIT - 12 }}
        >
          <span className="truncate font-mono text-2xs tabular-nums" title={mark.node.session_id}>
            {mark.label}
          </span>
          <span className="flex min-w-0 items-center gap-1 text-3xs opacity-80">
            <UsageLabel mark={mark} />
            <span className="truncate font-mono tabular-nums opacity-90">· {detail}</span>
          </span>
        </span>
      </button>
      {mark.foldedDescendants > 0 ? (
        <button
          type="button"
          className={cn(
            'absolute flex min-h-[44px] min-w-[44px] items-center justify-center transition-opacity duration-[var(--dur-state)]',
            dim && 'opacity-40',
          )}
          style={{ top: at.y - HIT / 2, left: at.x + radius + 18 - HIT / 2, color: 'var(--raw-graph-text)' }}
          aria-expanded={false}
          aria-label={`Open the ${mark.foldedDescendants} ${mark.foldedDescendants === 1 ? 'session' : 'sessions'} beneath ${mark.label}`}
          onMouseEnter={inspect}
          onFocus={inspect}
          onClick={() => interaction.onToggleExpanded(mark.id)}
          data-topology-control="fold"
          data-topology-id={mark.id}
        >
          <span className="td-legend" style={{ color: 'var(--raw-graph-accent)' }}>
            +{mark.foldedDescendants}
          </span>
        </button>
      ) : null}
    </>
  );
}

function bundleTitle(mark: TopologyBundleMark): string {
  return mark.basis === 'remainder' ? mark.label : `${mark.sessions} × ${mark.label}`;
}

/** The field's key and its population, reconciled: drawn plus folded equals
 * the reading, printed so the sum can be checked rather than trusted. */
export function TopologyPopulation({ model, fit }: { model: DelegationTopologyModel; fit: FittedTopology }) {
  const foldedGenerations = fit.maxDepth + 1 - model.columns;
  return (
    <span className="td-legend whitespace-normal text-text-secondary" data-topology-population>
      {model.totalSessions.toLocaleString()} sessions · {model.drawnSessions.toLocaleString()} drawn
      {model.bundledSessions > 0 ? ` · ${model.bundledSessions.toLocaleString()} folded` : ''} ·{' '}
      {model.columns} {model.columns === 1 ? 'generation' : 'generations'}
      {foldedGenerations > 0 ? ` of ${fit.maxDepth + 1}` : ''}
    </span>
  );
}

/** Legend for the ring marks: what the ring, core and inner glyph mean. */
function RingLegend({ model, fit }: { model: DelegationTopologyModel; fit: FittedTopology }) {
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1.5 px-1 text-3xs text-text-muted">
      <TopologyPopulation model={model} fit={fit} />
      <Swatch label="ring · sessions beneath, log band">
        <circle cx={12} cy={8} r={6} fill="var(--raw-graph-accent)" fillOpacity={0.12} stroke="var(--raw-graph-accent)" strokeWidth={1.2} />
        <circle cx={12} cy={8} r={2} fill="var(--raw-graph-accent)" />
      </Swatch>
      <Swatch label="› delegated subagent">
        <circle cx={12} cy={8} r={6} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={1.2} />
        <path d="M10 5 L13.5 8 L10 11" fill="none" stroke="var(--raw-graph-accent)" strokeWidth={1.1} />
      </Swatch>
      <Swatch label="cut edge · parent not in reading">
        <circle cx={12} cy={8} r={6} fill="none" stroke="var(--raw-graph-alert)" strokeWidth={1.2} strokeDasharray="3 2" />
      </Swatch>
      <Swatch label="parent cycle">
        <circle cx={12} cy={8} r={6} fill="none" stroke="var(--raw-state-conflicting)" strokeWidth={1.2} strokeDasharray="2 2" />
      </Swatch>
      <Swatch label="≡ bundle · folded siblings">
        <circle cx={12} cy={8} r={5} fill="none" stroke="var(--raw-graph-text)" strokeWidth={1.2} strokeDasharray="3 2" />
        <circle cx={12} cy={8} r={7} fill="none" stroke="var(--raw-graph-text)" strokeOpacity={0.5} strokeWidth={0.8} strokeDasharray="3 2" />
      </Swatch>
      <UsageCoverageLegend coverage={model.usageCoverage} />
      <span>hover shows exact counts · click selects</span>
    </div>
  );
}

export function Swatch({ label, children }: { label: string; children: ReactNode }) {
  return (
    <span className="flex items-center gap-1.5">
      <svg aria-hidden width={24} height={16} className="td-optic shrink-0 rounded-[1px]">
        {children}
      </svg>
      <span>{label}</span>
    </span>
  );
}
