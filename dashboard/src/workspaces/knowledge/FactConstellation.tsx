/**
 * The FACT CONSTELLATION aperture: the memory graph the overview served, drawn
 * on the night glass with its axes printed.
 *
 * Everything drawn here is a reading of `composeConstellation`, angle is
 * category, radius is trust, satellites sit where their relations put them.
 * The SVG is one `role="img"` whose label is the model's own description; the
 * fact ledger beside it is the exact accessible equivalent and the keyboard
 * path. Pointer work on a body follows the design system's grammar: hover
 * inspects (and dims the material it is not wired to), click selects, and
 * neither changes a measured value or fires activity.
 *
 * Two things this surface refuses to do. It never draws a relation to a node
 * the payload did not include, a dangling edge is counted in the footer, not
 * drawn to the centre. And a fact whose trust the store did not report sits
 * hollow, past the outer ring, rather than being given a radius that reads as
 * a low score.
 */
import { useId, useMemo, useState, type ReactNode } from 'react';

import type { MemoryReadStatusV1 } from '../../contracts/generated.ts';
import { cn } from '../../ui/cn';
import { Corners } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import {
  CONSTELLATION_RIM,
  CONSTELLATION_WORLD,
  constellationDescription,
  nodeGlyph,
  relationStyle,
  type ConstellationLink,
  type ConstellationModel,
  type ConstellationNode,
  type TrustBandId,
} from './constellation.ts';

const LABEL_CHARS = 30;

/** Opacity a fact body is drawn at: trust as luminance, with a floor so a
 * low-trust fact remains a body rather than vanishing into the field. */
function bodyOpacity(trust: number | null): number {
  if (trust == null) return 0.55;
  return 0.42 + Math.max(0, Math.min(1, trust)) * 0.58;
}

const TONE_STROKE: Record<ReturnType<typeof relationStyle>['tone'], string> = {
  signal: 'var(--raw-graph-accent)',
  conflict: 'var(--raw-state-conflicting)',
  stale: 'var(--raw-state-stale)',
  quiet: 'var(--raw-graph-edge)',
};

const TONE_OPACITY: Record<ReturnType<typeof relationStyle>['tone'], number> = {
  signal: 0.7,
  conflict: 0.85,
  stale: 0.8,
  quiet: 0.5,
};

/** Fill per trust band for the legend swatches, luminance steps of the one
 * signal hue, paired with the printed range so the band is never colour alone. */
const BAND_SWATCH_OPACITY: Record<TrustBandId, number> = {
  b80: 1,
  b60: 0.8,
  b40: 0.62,
  b20: 0.48,
  b00: 0.36,
  unmeasured: 0,
};

export function FactConstellation({
  model,
  inspectedFactId,
  selectedFactId,
  onInspect,
  onSelect,
  graphRead,
}: {
  model: ConstellationModel;
  /** The fact the reader is inspecting (hover or focus, sticky until Escape). */
  inspectedFactId: string | null;
  /** The fact the reader has selected. Persistent; drawn as a ring, not a glow. */
  selectedFactId: string | null;
  onInspect: (factId: string) => void;
  onSelect: (factId: string) => void;
  /** The daemon's own reading of the graph sub-read, printed beside coverage. */
  graphRead: MemoryReadStatusV1 | undefined;
}) {
  const haloId = useId();
  // A satellite under the pointer dims the field to its wiring but has no fact
  // to inspect, so it is local state rather than a page-level inspection.
  const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null);
  const description = useMemo(() => constellationDescription(model), [model]);

  const inspectedNodeId =
    inspectedFactId == null ? null : (model.nodeIdByFact.get(inspectedFactId) ?? null);
  const selectedNodeId =
    selectedFactId == null ? null : (model.nodeIdByFact.get(selectedFactId) ?? null);
  // Inspection dims what it is not wired to; selection does not. A selected
  // fact is marked by its ring and stays legible in a field drawn at full
  // strength, so a reader who has chosen one is not left looking at a dimmed
  // store until they press Escape.
  const focusNodeId =
    hoveredNodeId ?? (inspectedNodeId !== selectedNodeId ? inspectedNodeId : null);
  const focusSet = useMemo(() => {
    if (focusNodeId == null) return null;
    const set = new Set<string>([focusNodeId]);
    for (const neighbour of model.neighbours.get(focusNodeId) ?? []) set.add(neighbour);
    return set;
  }, [focusNodeId, model.neighbours]);

  const dimmed = (id: string) =>
    focusSet !== null && !focusSet.has(id) && id !== selectedNodeId;

  const empty = model.nodes.length === 0;

  return (
    <figure
      className="td-optic td-grain td-scanlines relative flex flex-col"
      data-testid="fact-constellation"
    >
      <Corners tone="signal" />
      <figcaption className="pointer-events-none relative z-10 flex flex-wrap items-start justify-between gap-2 px-3 pt-2">
        <span className="flex flex-col gap-1">
          <span className="td-title text-text-secondary">Fact constellation</span>
          <span className="text-3xs tracking-[0.04em] text-text-muted">
            angle · category &nbsp; radius · trust, centre is 1.00
          </span>
        </span>
        <span className="flex items-baseline gap-3 bg-surface-0/60 px-2 py-1 backdrop-blur-sm">
          <Hud label="facts drawn" value={model.coverage.drawnFacts} />
          <Hud label="relations" value={model.coverage.drawnRelations} />
          <Hud label="satellites" value={model.coverage.drawnSatellites} />
        </span>
      </figcaption>
      {empty ? (
        <div className="relative z-10 flex min-h-[220px] items-center justify-center p-6">
          <p className="max-w-md text-center text-xs leading-relaxed text-text-muted">
            The memory graph returned no roots to draw
            {model.coverage.completeness === 'complete'
              ? ', the complete read holds no fact roots.'
              : `, this ${model.coverage.completeness} read carried none; whole-store topology is not known from it.`}
          </p>
        </div>
      ) : (
        // The key sits beside the field from `lg`, in the width the 5:3 drawing
        // leaves free at the sides of a wider aperture, and beneath it below.
        // The field is one of two readings of the same rows, and the ledger
        // under it must stay in view at 1440x900 rather than below the fold.
        <div className="relative z-10 flex flex-col lg:flex-row lg:items-stretch">
        <svg
          role="img"
          aria-label={description}
          viewBox={`0 0 ${CONSTELLATION_WORLD.width} ${CONSTELLATION_WORLD.height}`}
          preserveAspectRatio="xMidYMid meet"
          className="block h-[clamp(180px,30vh,360px)] w-full select-none lg:min-w-0 lg:flex-1"
          data-testid="fact-constellation-svg"
          onPointerLeave={() => setHoveredNodeId(null)}
        >
          <defs>
            <filter id={haloId} x="-60%" y="-60%" width="220%" height="220%">
              <feGaussianBlur stdDeviation="4" />
            </filter>
          </defs>
          <TrustRings model={model} />
          <Sectors model={model} />
          <g data-layer="links">
            {model.links.map((link) => (
              <Link key={link.id} link={link} dimmed={dimmed(link.source) || dimmed(link.target)} />
            ))}
          </g>
          <g data-layer="bodies">
            {model.nodes.map((node) => (
              <Body
                key={node.id}
                node={node}
                haloId={haloId}
                dimmed={dimmed(node.id)}
                inspected={node.id === focusNodeId}
                selected={node.id === selectedNodeId}
                onEnter={() => {
                  // Movement, not entry, for the same reason the ledger rows
                  // inspect on move: a field scrolling under a parked pointer
                  // must not steal inspection from the keyboard.
                  if (hoveredNodeId === node.id) return;
                  setHoveredNodeId(node.id);
                  if (node.factId != null) onInspect(node.factId);
                }}
                onLeave={() => setHoveredNodeId((current) => (current === node.id ? null : current))}
                onClick={() => {
                  if (node.factId != null) onSelect(node.factId);
                }}
              />
            ))}
          </g>
          <g data-layer="labels" className="max-md:hidden">
            {model.nodes
              .filter((node) => node.labelled)
              .map((node) => (
                <text
                  key={`label:${node.id}`}
                  x={node.labelSide === 'right' ? node.x + node.r + 6 : node.x - node.r - 6}
                  y={node.y + 3.5}
                  textAnchor={node.labelSide === 'right' ? 'start' : 'end'}
                  fontSize={10.5}
                  fontFamily="var(--font-mono)"
                  fill="var(--raw-graph-text)"
                  opacity={dimmed(node.id) ? 0.25 : 0.9}
                  className="pointer-events-none transition-opacity duration-[var(--dur-state)]"
                >
                  {elide(node.label)}
                </text>
              ))}
          </g>
        </svg>
        <div className="flex flex-wrap items-start justify-between gap-x-6 gap-y-1.5 px-3 pb-2 pt-1 lg:w-44 lg:shrink-0 lg:flex-col lg:justify-start lg:gap-y-3 lg:pt-2">
          <TrustLegend model={model} />
          <RelationLegend model={model} />
        </div>
        </div>
      )}
      <CoverageFooter model={model} graphRead={graphRead} />
    </figure>
  );
}

function Hud({ label, value }: { label: string; value: number }) {
  return (
    <span className="flex flex-col-reverse gap-0.5">
      <span className="td-legend">{label}</span>
      <span className="td-display text-base text-text-primary" data-cell="numeric">
        {value.toLocaleString()}
      </span>
    </span>
  );
}

function elide(label: string): string {
  const line = label.split('\n')[0] ?? '';
  return line.length > LABEL_CHARS ? `${line.slice(0, LABEL_CHARS - 1)}…` : line;
}

/** Concentric rings at each trust band's inner edge, labelled up the twelve
 * o'clock ray so the radius axis can be read off the field. */
function TrustRings({ model }: { model: ConstellationModel }) {
  const cx = CONSTELLATION_WORLD.width / 2;
  const cy = CONSTELLATION_WORLD.height / 2;
  return (
    <g data-layer="rings" aria-hidden>
      {model.bands.map((band) =>
        band.ring == null ? null : (
          <g key={band.id}>
            <circle
              cx={cx}
              cy={cy}
              r={band.ring}
              fill="none"
              stroke="var(--raw-graph-edge)"
              strokeOpacity={0.42}
              strokeWidth={0.8}
            />
            <text
              x={cx + 3}
              y={cy - band.ring - 3}
              fontSize={8.5}
              fontFamily="var(--font-mono)"
              fill="var(--raw-graph-text)"
              opacity={0.55}
            >
              {band.lower?.toFixed(2)}
            </text>
          </g>
        ),
      )}
    </g>
  );
}

/** Category sector boundaries and their engraved names on the rim. */
function Sectors({ model }: { model: ConstellationModel }) {
  const cx = CONSTELLATION_WORLD.width / 2;
  const cy = CONSTELLATION_WORLD.height / 2;
  const rim = CONSTELLATION_RIM;
  if (model.sectors.length === 0) return null;
  return (
    <g data-layer="sectors" aria-hidden>
      {model.sectors.map((sector) => (
        <g key={sector.category}>
          {model.sectors.length > 1 ? (
            <line
              x1={cx + Math.cos(sector.start) * 40}
              y1={cy + Math.sin(sector.start) * 40}
              x2={cx + Math.cos(sector.start) * rim}
              y2={cy + Math.sin(sector.start) * rim}
              stroke="var(--raw-graph-edge)"
              strokeOpacity={0.3}
              strokeWidth={0.8}
              strokeDasharray="2 6"
            />
          ) : null}
          <text
            x={sector.labelX}
            y={sector.labelY}
            textAnchor={sector.labelAnchor}
            fontSize={9}
            fontFamily="var(--font-display)"
            letterSpacing="0.14em"
            fill="var(--raw-graph-text)"
            opacity={0.7}
            className="uppercase"
          >
            {sector.category} · {sector.count}
          </text>
        </g>
      ))}
    </g>
  );
}

function Link({ link, dimmed }: { link: ConstellationLink; dimmed: boolean }) {
  const style = relationStyle(link.kind);
  return (
    <line
      x1={link.x1}
      y1={link.y1}
      x2={link.x2}
      y2={link.y2}
      stroke={TONE_STROKE[style.tone]}
      strokeOpacity={dimmed ? 0.08 : TONE_OPACITY[style.tone]}
      strokeWidth={style.tone === 'quiet' ? 0.9 : 1.3}
      strokeDasharray={style.dash}
      strokeLinecap="round"
      data-relation={link.kind}
      className="transition-opacity duration-[var(--dur-state)]"
    />
  );
}

function Body({
  node,
  haloId,
  dimmed,
  inspected,
  selected,
  onEnter,
  onLeave,
  onClick,
}: {
  node: ConstellationNode;
  haloId: string;
  dimmed: boolean;
  inspected: boolean;
  selected: boolean;
  onEnter: () => void;
  onLeave: () => void;
  onClick: () => void;
}) {
  const glyph = nodeGlyph(node.kind);
  const opacity = dimmed ? 0.22 : bodyOpacity(node.trust);
  const hollow = node.kind === 'fact' && node.trust == null;
  const fill = node.kind === 'fact' ? 'var(--raw-graph-accent)' : 'var(--raw-graph-text)';
  return (
    <g
      data-node={node.id}
      data-node-kind={node.kind}
      data-fact-id={node.factId ?? undefined}
      data-inspected={inspected || undefined}
      data-selected={selected || undefined}
      onPointerMove={onEnter}
      onPointerLeave={onLeave}
      onClick={onClick}
      className={cn(
        'transition-opacity duration-[var(--dur-state)]',
        node.factId != null ? 'cursor-pointer' : 'cursor-default',
      )}
      opacity={opacity}
    >
      {/* The luminous body: one soft disc under the core, scaled by the same
        * trust that sets the core's brightness. It is the measurement drawn
        * larger, not atmosphere. */}
      {node.kind === 'fact' && !hollow ? (
        <circle cx={node.x} cy={node.y} r={node.r * 2.4} fill={fill} opacity={0.12} />
      ) : null}
      {inspected ? (
        <circle
          cx={node.x}
          cy={node.y}
          r={node.r + 7}
          fill="none"
          stroke="var(--raw-graph-accent)"
          strokeWidth={2.5}
          opacity={0.7}
          filter={`url(#${haloId})`}
        />
      ) : null}
      {selected ? (
        <circle
          cx={node.x}
          cy={node.y}
          r={node.r + 4.5}
          fill="none"
          stroke="var(--raw-graph-accent)"
          strokeWidth={1.4}
        />
      ) : null}
      <Glyph shape={glyph.shape} node={node} fill={fill} hollow={hollow} />
      {/* The hit area: a body is a few units wide and the pointer is not. */}
      <circle
        cx={node.x}
        cy={node.y}
        r={Math.max(node.r + 6, 10)}
        fill="transparent"
        stroke="none"
      >
        <title>
          {glyph.label}: {elide(node.label)}
          {node.trust != null ? ` · trust ${node.trust.toFixed(2)}` : ''}
          {node.degree > 0 ? ` · ${node.degree} relation${node.degree === 1 ? '' : 's'}` : ''}
        </title>
      </circle>
    </g>
  );
}

function Glyph({
  shape,
  node,
  fill,
  hollow,
}: {
  shape: ReturnType<typeof nodeGlyph>['shape'];
  node: ConstellationNode;
  fill: string;
  hollow: boolean;
}) {
  const { x, y, r } = node;
  switch (shape) {
    case 'disc':
      return hollow ? (
        <circle cx={x} cy={y} r={r} fill="none" stroke={fill} strokeWidth={1.2} strokeDasharray="2 2" />
      ) : (
        <circle cx={x} cy={y} r={r} fill={fill} />
      );
    case 'diamond':
      return (
        <polygon
          points={`${x},${y - r} ${x + r},${y} ${x},${y + r} ${x - r},${y}`}
          fill={fill}
          opacity={0.9}
        />
      );
    case 'square':
      return <rect x={x - r} y={y - r} width={r * 2} height={r * 2} fill={fill} opacity={0.9} />;
    case 'tick':
      return (
        <path
          d={`M ${x - r} ${y} L ${x + r} ${y} M ${x} ${y - r} L ${x} ${y + r}`}
          stroke={fill}
          strokeWidth={1.2}
          fill="none"
        />
      );
    default: {
      const unhandled: never = shape;
      return unhandled;
    }
  }
}

/** The legends run as ruled rows rather than stacked lists: the field is one
 * of two readings of the same rows and the ledger beneath it has to stay in
 * view, so the key spends width, of which there is plenty, not height. */
function TrustLegend({ model }: { model: ConstellationModel }) {
  return (
    <dl aria-label="Trust bands" className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <dt className="td-legend">trust</dt>
      {model.bands.map((band) => (
        <dd key={band.id} className="flex items-center gap-1.5 text-3xs text-text-muted">
          <span
            aria-hidden
            className={cn(
              'size-2 shrink-0 rounded-full',
              band.id === 'unmeasured' ? 'border border-dashed border-text-muted' : 'bg-accent',
            )}
            style={
              band.id === 'unmeasured' ? undefined : { opacity: BAND_SWATCH_OPACITY[band.id] }
            }
          />
          <span className="td-value" data-cell="numeric">
            {band.label}
          </span>
          <span className="td-value text-text-secondary" data-cell="numeric">
            {band.count.toLocaleString()}
          </span>
        </dd>
      ))}
    </dl>
  );
}

function RelationLegend({ model }: { model: ConstellationModel }) {
  const satelliteKinds = [...new Set(model.nodes.filter((node) => node.kind !== 'fact').map((node) => node.kind))];
  if (model.relationKinds.length === 0 && satelliteKinds.length === 0) {
    return (
      <p className="max-w-xs text-3xs leading-relaxed text-text-muted">
        no relation was returned in this read, so bodies stand alone; the wire, not the
        drawing, decides what is connected
      </p>
    );
  }
  return (
    <dl aria-label="Relations and bodies" className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <dt className="td-legend">relations · bodies</dt>
      {model.relationKinds.map((kind) => {
        const style = relationStyle(kind);
        return (
          <dd key={kind} className="flex items-center gap-1.5 text-3xs text-text-muted">
            <svg aria-hidden width="22" height="6" viewBox="0 0 22 6" className="shrink-0">
              <line
                x1="1"
                y1="3"
                x2="21"
                y2="3"
                stroke={TONE_STROKE[style.tone]}
                strokeOpacity={TONE_OPACITY[style.tone] + 0.2}
                strokeWidth={style.tone === 'quiet' ? 1 : 1.4}
                strokeDasharray={style.dash}
                strokeLinecap="round"
              />
            </svg>
            <span>{style.label}</span>
          </dd>
        );
      })}
      {satelliteKinds.map((kind) => {
        const glyph = nodeGlyph(kind);
        return (
          <dd key={kind} className="flex items-center gap-1.5 text-3xs text-text-muted">
            <svg aria-hidden width="22" height="10" viewBox="0 0 22 10" className="shrink-0">
              <Glyph
                shape={glyph.shape}
                fill="var(--raw-graph-text)"
                hollow={false}
                node={{
                  id: kind,
                  kind,
                  factId: null,
                  label: '',
                  category: null,
                  trust: null,
                  payloadAccess: null,
                  x: 11,
                  y: 5,
                  r: 3.5,
                  degree: 0,
                  band: 'unmeasured',
                  labelled: false,
                  labelSide: 'right',
                }}
              />
            </svg>
            <span>{glyph.label}</span>
          </dd>
        );
      })}
    </dl>
  );
}

/** What this drawing covers, in the daemon's own accounting. */
function CoverageFooter({
  model,
  graphRead,
}: {
  model: ConstellationModel;
  graphRead: MemoryReadStatusV1 | undefined;
}) {
  const { coverage } = model;
  const readState = graphRead?.state;
  const readComplete = readState === 'ready' || readState === 'complete_zero_findings';
  const parts: ReactNode[] = [
    <span key="roots">
      {coverage.drawnFacts.toLocaleString()} of {coverage.factUniverse.toLocaleString()} facts in
      the store drawn
    </span>,
    <span key="relations">
      {coverage.drawnRelations.toLocaleString()} of {coverage.relationCount.toLocaleString()}{' '}
      relations drawn, limit {coverage.relationLimit.toLocaleString()}
    </span>,
  ];
  if (coverage.unavailableFactCandidates > 0) {
    parts.push(
      <span key="unavailable" className="text-state-partial">
        {coverage.unavailableFactCandidates.toLocaleString()} fact{' '}
        {coverage.unavailableFactCandidates === 1 ? 'candidate' : 'candidates'} unavailable and
        not drawn
      </span>,
    );
  }
  if (coverage.danglingRelations > 0) {
    parts.push(
      <span key="dangling" className="text-state-partial">
        {coverage.danglingRelations.toLocaleString()}{' '}
        {coverage.danglingRelations === 1 ? 'relation names' : 'relations name'} a body this
        read did not include
      </span>,
    );
  }
  return (
    <footer
      className="relative z-10 flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-edge-subtle/60 px-3 py-1.5 text-3xs text-text-muted"
      data-testid="fact-constellation-coverage"
    >
      {readState && !readComplete ? (
        <StateChip
          kind={readState}
          detail={graphRead?.error ?? graphRead?.code ?? 'memory graph read'}
        />
      ) : null}
      <span className={cn('td-value', coverage.completeness !== 'complete' && 'text-state-partial')}>
        graph coverage {coverage.completeness}
        {coverage.omissionReasons.length > 0 ? `: ${coverage.omissionReasons.join(', ')}` : ''}
      </span>
      {parts.map((part, index) => (
        <span key={index} className="flex items-center gap-3">
          <span aria-hidden className="h-3 w-px bg-edge-subtle" />
          {part}
        </span>
      ))}
    </footer>
  );
}
