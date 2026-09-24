import { subagentElapsedSeconds } from './subagentTree.ts';
import type { TopologyMark } from './delegationTopology.ts';

/**
 * The ring mark: the plate's hollow, descendant-scaled ring with a faint
 * core, drawn over the same generation-column layout as the disc field.
 *
 * Size is the one measured quantity the hierarchy authority serves per node,
 * sessions beneath it. Token counts would read better on a ring, and the plate
 * prints them, but `AnalyticsSubagentNodeV1` carries none, so the ring never
 * claims one. The glyph inside the ring names what the reading says the mark
 * is, not what it did.
 */

/** Core fill opacity from sessions beneath, on the same log band as the
 * radius: a leaf's core is a whisper, the widest subtree's reads as filled.
 * Measured, never decorative, so a ring with nothing beneath never glows. */
export function ringCoreAlpha(mark: TopologyMark, maxDescendants: number): number {
  const beneath = mark.kind === 'bundle' ? mark.sessions + mark.descendants : mark.node.descendants;
  const ceiling = Math.max(maxDescendants, beneath);
  const fraction = ceiling <= 0 || beneath <= 0 ? 0 : Math.log1p(beneath) / Math.log1p(ceiling);
  return Math.round((0.06 + 0.3 * fraction) * 1000) / 1000;
}

export const RING_GEOMETRY = {
  minRadius: 6,
  maxRadius: 20,
} as const;

/** Radius from sessions beneath, on a log band against the widest drawn. A
 * bundle is sized by everything it hides, so a closed group is never smaller
 * than the sessions folded into it. */
export function ringRadius(mark: TopologyMark, maxDescendants: number): number {
  const { minRadius, maxRadius } = RING_GEOMETRY;
  const beneath = mark.kind === 'bundle' ? mark.sessions + mark.descendants : mark.node.descendants;
  const ceiling = Math.max(maxDescendants, beneath);
  if (ceiling <= 0 || beneath <= 0) return minRadius;
  return minRadius + (maxRadius - minRadius) * (Math.log1p(beneath) / Math.log1p(ceiling));
}

/** What the glyph inside a ring says. `origin` is a session the store records
 * as no one's subagent; `delegate` is a linked child; the other three are the
 * reading's own typed states. */
export type RingKind = 'origin' | 'delegate' | 'bundle' | 'cut' | 'cycle';

export function ringKind(mark: TopologyMark): RingKind {
  if (mark.kind === 'bundle') return 'bundle';
  switch (mark.node.link) {
    case 'missing_parent':
      return 'cut';
    case 'cycle':
      return 'cycle';
    case 'root':
    case 'linked':
      return mark.node.is_subagent ? 'delegate' : 'origin';
    default: {
      const unhandled: never = mark.node.link;
      return unhandled;
    }
  }
}

function ringTone(kind: RingKind): { stroke: string; dash: string | undefined } {
  switch (kind) {
    case 'origin':
    case 'delegate':
      return { stroke: 'var(--raw-graph-accent)', dash: undefined };
    case 'bundle':
      return { stroke: 'var(--raw-graph-text)', dash: '3 2' };
    case 'cut':
      return { stroke: 'var(--raw-graph-alert)', dash: '3 2' };
    case 'cycle':
      return { stroke: 'var(--raw-state-conflicting)', dash: '2 2' };
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** The monoline glyph inside the ring, 6px tall whatever the ring's size. */
function KindGlyph({ kind, x, y, stroke }: { kind: RingKind; x: number; y: number; stroke: string }) {
  switch (kind) {
    case 'origin':
      return <circle cx={x} cy={y} r={2.2} fill={stroke} />;
    case 'delegate':
      return <path d={`M${x - 2} ${y - 3} L${x + 1.5} ${y} L${x - 2} ${y + 3}`} fill="none" stroke={stroke} strokeWidth={1.2} />;
    case 'bundle':
      return (
        <path
          d={`M${x - 3} ${y - 2.5} H${x + 3} M${x - 3} ${y} H${x + 3} M${x - 3} ${y + 2.5} H${x + 3}`}
          stroke={stroke}
          strokeWidth={1}
        />
      );
    case 'cut':
      return <path d={`M${x - 3} ${y} H${x + 3}`} stroke={stroke} strokeWidth={1.2} strokeDasharray="1.5 1.5" />;
    case 'cycle':
      return (
        <path
          d={`M${x - 2.5} ${y + 1.5} A2.6 2.6 0 1 1 ${x + 2.5} ${y + 1.5}`}
          fill="none"
          stroke={stroke}
          strokeWidth={1.1}
        />
      );
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

export function RingGlyph({
  mark,
  at,
  radius,
  maxDescendants,
  hatchId,
  inspected,
  selected,
  lifted,
  dim,
}: {
  mark: TopologyMark;
  at: { x: number; y: number };
  radius: number;
  maxDescendants: number;
  hatchId: string;
  inspected: boolean;
  selected: boolean;
  /** Beneath the selection: a restrained cyan halo, no glow. */
  lifted: boolean;
  dim: boolean;
}) {
  const core = ringCoreAlpha(mark, maxDescendants);
  const kind = ringKind(mark);
  const tone = ringTone(kind);
  return (
    <g
      opacity={dim ? 0.3 : 1}
      className="transition-opacity duration-[var(--dur-state)]"
      data-topology-mark={mark.kind}
      data-topology-ring={kind}
      data-topology-core={core}
    >
      {selected ? (
        <circle cx={at.x} cy={at.y} r={radius + 5} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={2} data-topology-selected="true" />
      ) : null}
      {lifted ? (
        <circle cx={at.x} cy={at.y} r={radius + 3.5} fill="none" stroke="var(--raw-graph-accent)" strokeOpacity={0.35} strokeWidth={1} data-topology-halo="true" />
      ) : null}
      {inspected && !selected ? (
        <circle cx={at.x} cy={at.y} r={radius + 4} fill="none" stroke="var(--raw-graph-text)" strokeOpacity={0.55} strokeWidth={1} />
      ) : null}
      <circle
        cx={at.x}
        cy={at.y}
        r={radius * 0.72}
        fill={kind === 'cycle' ? `url(#${hatchId})` : tone.stroke}
        fillOpacity={kind === 'cycle' ? 0.8 : inspected || selected ? Math.min(0.5, core + 0.1) : core}
      />
      <circle cx={at.x} cy={at.y} r={radius} fill="none" stroke={tone.stroke} strokeWidth={1.4} strokeDasharray={tone.dash} />
      {kind === 'bundle' ? (
        <circle cx={at.x} cy={at.y} r={radius + 2.5} fill="none" stroke={tone.stroke} strokeOpacity={0.5} strokeWidth={0.8} strokeDasharray={tone.dash} />
      ) : null}
      <KindGlyph kind={kind} x={at.x} y={at.y} stroke={tone.stroke} />
      {mark.kind === 'session' && mark.foldedDescendants > 0 ? (
        <g data-topology-folded={mark.foldedDescendants}>
          <line x1={at.x + radius} x2={at.x + radius + 18} y1={at.y} y2={at.y} stroke="var(--raw-graph-edge)" strokeWidth={1} strokeDasharray="2 3" />
          <line x1={at.x + radius + 18} x2={at.x + radius + 18} y1={at.y - 4} y2={at.y + 4} stroke="var(--raw-graph-edge)" strokeWidth={1} />
        </g>
      ) : null}
    </g>
  );
}

/** The ring label's second line: the exact count the ring is sized by. */
export function ringCountLine(mark: TopologyMark): string {
  if (mark.kind === 'bundle') {
    return `${mark.sessions + mark.descendants} sessions folded`;
  }
  if (mark.node.descendants > 0) return `${mark.node.descendants.toLocaleString()} beneath`;
  return 'leaf';
}

/** Hover inspects: the exact counts behind the ring, beside the mark. */
export function RingHoverCard({ mark, at, radius }: { mark: TopologyMark; at: { x: number; y: number }; radius: number }) {
  const rows: [string, string][] =
    mark.kind === 'bundle'
      ? [
          ['sessions', mark.sessions.toLocaleString()],
          ['beneath them', mark.descendants.toLocaleString()],
          ['basis', mark.basis],
        ]
      : [
          ['beneath', mark.node.descendants.toLocaleString()],
          ['drawn children', mark.drawnChildren.toLocaleString()],
          ['folded', mark.foldedDescendants.toLocaleString()],
          ['span', (() => {
            const elapsed = subagentElapsedSeconds(mark.node);
            return elapsed === null ? 'absent' : `${elapsed.toLocaleString()} s`;
          })()],
          ['tokens', 'absent'],
        ];
  return (
    <div
      aria-hidden
      className="pointer-events-none absolute z-10 flex min-w-56 flex-col gap-0.5 border border-edge-strong bg-surface-2 px-2 py-1.5 shadow-lg rounded-[var(--radius-panel)]"
      style={{ left: at.x + radius + 10, top: at.y + radius + 6 }}
      data-topology-hovercard={mark.id}
    >
      {rows.map(([label, value]) => (
        <span key={label} className="flex items-baseline justify-between gap-3">
          <span className="td-legend">{label}</span>
          <span className="td-value whitespace-nowrap text-2xs tabular-nums">{value}</span>
        </span>
      ))}
    </div>
  );
}
