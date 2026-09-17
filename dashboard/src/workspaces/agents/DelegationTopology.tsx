import { useId, useMemo, type ReactNode } from 'react';
import { cn } from '../../ui/cn';
import { subagentElapsedSeconds } from './subagentTree.ts';
import {
  TOPOLOGY_GEOMETRY,
  edgePath,
  fieldSize,
  markPosition,
  markRadius,
  neighbourhood,
  type DelegationTopologyModel,
  type FittedTopology,
  type TopologyBundleMark,
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

export function DelegationTopology({
  fit,
  interaction,
}: {
  fit: FittedTopology;
  interaction: TopologyInteraction;
}) {
  const { model } = fit;
  const { inspectedId, selectedId } = interaction;
  const size = fieldSize(model);
  const hatchId = useId();
  const keep = useMemo(
    () => (inspectedId === null ? null : neighbourhood(model, inspectedId)),
    [model, inspectedId],
  );
  const byId = useMemo(() => new Map(model.marks.map((mark) => [mark.id, mark])), [model]);
  const isDim = (id: string) => keep !== null && !keep.has(id);

  return (
    <div className="flex min-w-0 flex-col gap-2" data-delegation-topology={model.drawnSessions}>
      <div
        role="group"
        aria-label="Delegation topology field"
        // The field scrolls when a reading outgrows the aperture; a named
        // scroll container needs its own tab stop when no mark is drawn in
        // view, and the marks themselves take over as soon as they are.
        tabIndex={0}
        className="td-optic td-grain td-scanlines td-graticule relative max-h-[34rem] min-h-[14rem] overflow-auto"
        onMouseLeave={() => interaction.onInspect(null)}
      >
        <div
          className="relative"
          style={{ width: size.width, height: size.height + HEADER_HEIGHT, minWidth: '100%' }}
        >
          <GenerationHeaders model={model} fit={fit} />
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
              const x = TOPOLOGY_GEOMETRY.padX + generation.generation * TOPOLOGY_GEOMETRY.columnPitch;
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
              const lit = keep !== null && keep.has(edge.from) && keep.has(edge.to);
              const dim = keep !== null && !lit;
              return (
                <path
                  key={edge.id}
                  d={edgePath(
                    markPosition(from),
                    markPosition(to),
                    markRadius(from, model.maxDescendants),
                    markRadius(to, model.maxDescendants),
                  )}
                  fill="none"
                  stroke={lit ? 'var(--raw-graph-accent)' : 'var(--raw-graph-edge)'}
                  strokeWidth={lit ? 1.8 : 1.2}
                  strokeOpacity={dim ? 0.18 : lit ? 1 : 0.75}
                  strokeDasharray={edge.kind === 'bundle' ? '3 3' : undefined}
                  className="transition-[stroke-opacity] duration-[var(--dur-state)]"
                  data-topology-edge={edge.kind}
                  data-topology-lit={lit ? 'true' : undefined}
                />
              );
            })}
            {model.stubs.map((stub) => {
              const to = byId.get(stub.to);
              if (!to) return null;
              const at = markPosition(to);
              const radius = markRadius(to, model.maxDescendants);
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
              <MarkGlyph
                key={mark.id}
                mark={mark}
                model={model}
                hatchId={hatchId}
                inspected={mark.id === inspectedId}
                selected={mark.id === selectedId}
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
                  model={model}
                  interaction={interaction}
                  dim={isDim(mark.id)}
                />
              </li>
            ))}
          </ul>
        </div>
      </div>
      <TopologyLegend model={model} fit={fit} />
    </div>
  );
}

/** Column captions across the top of the field: generation, its role, and
 * the reconciled count of what the column holds. */
function GenerationHeaders({ model, fit }: { model: DelegationTopologyModel; fit: FittedTopology }) {
  return (
    <ol
      aria-label="Generations"
      className="absolute inset-x-0 top-0 m-0 list-none p-0"
      style={{ height: HEADER_HEIGHT }}
    >
      {model.generations.map((generation) => {
        const x = TOPOLOGY_GEOMETRY.padX + generation.generation * TOPOLOGY_GEOMETRY.columnPitch;
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
            <span className="td-value truncate text-3xs opacity-80">{count}</span>
          </li>
        );
      })}
    </ol>
  );
}

/** The drawn body of one mark: session disc, bundle hatch, source ring,
 * selection ring, inspection halo and the folded tail. Purely visual. */
function MarkGlyph({
  mark,
  model,
  hatchId,
  inspected,
  selected,
  dim,
}: {
  mark: TopologyMark;
  model: DelegationTopologyModel;
  hatchId: string;
  inspected: boolean;
  selected: boolean;
  dim: boolean;
}) {
  const at = markPosition(mark);
  const radius = markRadius(mark, model.maxDescendants);
  const source =
    mark.kind === 'session' &&
    mark.generation === 0 &&
    mark.node.link === 'root' &&
    (mark.drawnChildren > 0 || mark.foldedDescendants > 0);
  const fill =
    mark.kind === 'bundle'
      ? `url(#${hatchId})`
      : mark.node.link === 'missing_parent'
        ? 'var(--raw-graph-alert)'
        : mark.node.link === 'cycle'
          ? `url(#${hatchId})`
          : 'var(--raw-graph-accent)';
  const stroke =
    mark.kind === 'bundle'
      ? 'var(--raw-graph-text)'
      : mark.node.link === 'cycle'
        ? 'var(--raw-state-conflicting)'
        : mark.node.link === 'missing_parent'
          ? 'var(--raw-graph-alert)'
          : 'var(--raw-graph-accent)';
  return (
    <g
      opacity={dim ? 0.3 : 1}
      className="transition-opacity duration-[var(--dur-state)]"
      data-topology-mark={mark.kind}
    >
      {source ? (
        <circle
          cx={at.x}
          cy={at.y}
          r={radius + 7}
          fill="none"
          stroke="var(--raw-graph-accent)"
          strokeOpacity={0.45}
          strokeWidth={1}
        />
      ) : null}
      {selected ? (
        <circle
          cx={at.x}
          cy={at.y}
          r={radius + 5}
          fill="none"
          stroke="var(--raw-graph-accent)"
          strokeWidth={2}
          data-topology-selected="true"
        />
      ) : null}
      {inspected && !selected ? (
        <circle
          cx={at.x}
          cy={at.y}
          r={radius + 4}
          fill="none"
          stroke="var(--raw-graph-text)"
          strokeOpacity={0.7}
          strokeWidth={1}
        />
      ) : null}
      <circle
        cx={at.x}
        cy={at.y}
        r={radius}
        fill={fill}
        fillOpacity={mark.kind === 'bundle' ? 1 : source ? 0.95 : 0.8}
        stroke={stroke}
        strokeWidth={mark.kind === 'bundle' ? 1.2 : 1}
        strokeDasharray={mark.kind === 'bundle' ? '3 2' : undefined}
      />
      {mark.kind === 'session' && mark.foldedDescendants > 0 ? (
        <g data-topology-folded={mark.foldedDescendants}>
          <line
            x1={at.x + radius}
            x2={at.x + radius + 18}
            y1={at.y}
            y2={at.y}
            stroke="var(--raw-graph-edge)"
            strokeWidth={1.2}
            strokeDasharray="2 3"
          />
          <line
            x1={at.x + radius + 18}
            x2={at.x + radius + 18}
            y1={at.y - 4}
            y2={at.y + 4}
            stroke="var(--raw-graph-edge)"
            strokeWidth={1.2}
          />
        </g>
      ) : null}
    </g>
  );
}

/** The operable mark: a 44px hit area over the disc, the label to its right,
 * and — for a folded session — the tail control that opens the generation
 * beneath. Hover and focus inspect; click and Enter select or open. */
function MarkControl({
  mark,
  model,
  interaction,
  dim,
}: {
  mark: TopologyMark;
  model: DelegationTopologyModel;
  interaction: TopologyInteraction;
  dim: boolean;
}) {
  const at = markPosition(mark);
  const radius = markRadius(mark, model.maxDescendants);
  const selected = mark.id === interaction.selectedId;
  const inspect = () => interaction.onInspect(mark.id);
  const commonStyle = { top: at.y - HIT / 2, left: at.x - HIT / 2 } as const;
  const labelOffset = Math.max(0, radius - HIT / 2) + 8;

  if (mark.kind === 'bundle') {
    const expanded = interaction.expanded.has(mark.id);
    return (
      <button
        type="button"
        className={cn(
          'absolute flex min-h-[44px] items-center gap-0 text-left transition-opacity duration-[var(--dur-state)]',
          dim && 'opacity-40',
        )}
        style={{ ...commonStyle, color: 'var(--raw-graph-text)' }}
        aria-expanded={expanded}
        aria-label={`${bundleTitle(mark)}: ${mark.sessions} sessions in generation ${mark.generation}${mark.descendants > 0 ? `, ${mark.descendants} beneath them` : ''}. ${expanded ? 'Fold' : 'Open'} this bundle.`}
        onMouseEnter={inspect}
        onFocus={inspect}
        onClick={() => interaction.onToggleExpanded(mark.id)}
        data-topology-control="bundle"
        data-topology-id={mark.id}
      >
        <span aria-hidden className="block shrink-0" style={{ width: HIT, height: HIT }} />
        <span className="flex min-w-0 flex-col" style={{ marginLeft: labelOffset }}>
          <span className="td-value truncate text-2xs">{bundleTitle(mark)}</span>
          <span className="td-legend" style={{ color: 'var(--raw-graph-text)', opacity: 0.75 }}>
            {mark.descendants > 0 ? `${mark.descendants} beneath · ` : ''}
            {expanded ? 'fold' : 'open'}
          </span>
        </span>
      </button>
    );
  }

  const elapsed = subagentElapsedSeconds(mark.node);
  const detail = [
    mark.node.provider,
    mark.drawnChildren > 0
      ? `${mark.node.descendants} beneath`
      : mark.foldedDescendants > 0
        ? `${mark.foldedDescendants} folded`
        : null,
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
        <span className="flex min-w-0 max-w-[9.5rem] flex-col" style={{ marginLeft: labelOffset }}>
          <span className="td-value truncate text-2xs" title={mark.node.session_id}>
            {mark.label}
          </span>
          <span className="td-value truncate text-3xs opacity-75">{detail}</span>
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
function TopologyLegend({ model, fit }: { model: DelegationTopologyModel; fit: FittedTopology }) {
  const folded = fit.depthLimit !== Number.POSITIVE_INFINITY;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1.5 px-1 text-3xs text-text-muted">
      <span className="td-legend text-text-secondary" data-topology-population>
        {model.totalSessions.toLocaleString()} sessions · {model.drawnSessions.toLocaleString()} drawn
        {model.bundledSessions > 0 ? ` · ${model.bundledSessions.toLocaleString()} folded` : ''} ·{' '}
        {model.columns} {model.columns === 1 ? 'generation' : 'generations'}
        {folded ? ` of ${fit.maxDepth + 1}` : ''}
      </span>
      <Swatch label="linked delegation">
        <path d="M2 8 C10 8 10 8 18 8" stroke="var(--raw-graph-edge)" strokeWidth={1.4} fill="none" />
        <circle cx={20} cy={8} r={3.5} fill="var(--raw-graph-accent)" />
      </Swatch>
      <Swatch label="cut edge · parent not in reading">
        <path d="M2 8 H14" stroke="var(--raw-graph-alert)" strokeWidth={1.4} strokeDasharray="3 2" />
        <circle cx={19} cy={8} r={3.5} fill="var(--raw-graph-alert)" />
      </Swatch>
      <Swatch label="parent cycle">
        <circle cx={12} cy={9} r={4} fill="none" stroke="var(--raw-state-conflicting)" strokeWidth={1.4} strokeDasharray="2 1.5" />
        <path d="M8 7 C6 1 18 1 16 7" fill="none" stroke="var(--raw-state-conflicting)" strokeWidth={1.2} />
      </Swatch>
      <Swatch label="bundle · folded siblings">
        <circle cx={12} cy={8} r={5} fill="none" stroke="var(--raw-graph-text)" strokeWidth={1.2} strokeDasharray="3 2" />
        <path d="M9 5 L15 11 M9 11 L15 5" stroke="var(--raw-graph-text)" strokeWidth={0.8} />
      </Swatch>
      <span>size = sessions beneath, log band</span>
      <span>hover inspects · click selects · Escape clears</span>
    </div>
  );
}

function Swatch({ label, children }: { label: string; children: ReactNode }) {
  return (
    <span className="flex items-center gap-1.5">
      <svg aria-hidden width={24} height={16} className="td-optic shrink-0 rounded-[1px]">
        {children}
      </svg>
      <span>{label}</span>
    </span>
  );
}
