import { useId } from 'react';
import { gradeDashArray } from '../../../viz/temporal/palette.ts';
import type { EvidenceGrade } from '../../../viz/temporal/types.ts';
import type { WorkDagLayout, WorkDagLayoutEdge, WorkDagRelationKind } from '../workDagLayout.ts';
import type { Emphasis } from './dagField.ts';

/**
 * The relation paths beneath the layered cards. Dashes belong to the evidence
 * grade, as on every other field, so a relation's KIND rides its end marker:
 * an arrowhead for a gating dependency, a bar for an informational relation,
 * a diamond for a causal candidate. A climb inside a declared cycle takes the
 * conflicting hue, isolation cyan and the effort-weighted critical path amber.
 * Hidden from the accessibility tree; the exact table restates every relation.
 */

export type RelationMarker = 'arrowhead' | 'bar' | 'diamond';

export function relationMarker(kind: WorkDagRelationKind): RelationMarker {
  switch (kind) {
    case 'gating':
      return 'arrowhead';
    case 'informational':
      return 'bar';
    case 'causal':
      return 'diamond';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** The grade the Work graph authority gives each declared relation kind. */
export function relationGrade(kind: WorkDagRelationKind): Extract<EvidenceGrade, 'exact' | 'explicit'> {
  switch (kind) {
    case 'gating':
      return 'exact';
    case 'informational':
    case 'causal':
      return 'explicit';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

export function relationLabel(kind: WorkDagRelationKind): string {
  switch (kind) {
    case 'gating':
      return 'gating dependency';
    case 'informational':
      return 'informational relation';
    case 'causal':
      return 'causal candidate';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

const MARKER_PATH: Readonly<Record<RelationMarker, string>> = {
  arrowhead: 'M 0 0 L 8 4 L 0 8 z',
  bar: 'M 5.5 0 H 8 V 8 H 5.5 z',
  diamond: 'M 0 4 L 4 0.5 L 8 4 L 4 7.5 z',
};

const TONES = ['edge', 'accent', 'alert', 'conflicting'] as const;
const MARKERS: readonly RelationMarker[] = ['arrowhead', 'bar', 'diamond'];

/** A legend swatch: the grade's stroke ending in the kind's marker. */
export function RelationSample({ kind }: { kind: WorkDagRelationKind }) {
  return (
    <svg aria-hidden width={22} height={8} viewBox="0 0 22 8" className="shrink-0 text-text-secondary">
      <line x1={0} y1={4} x2={15} y2={4} stroke="currentColor" strokeWidth={1.1} strokeDasharray={gradeDashArray(relationGrade(kind)) || undefined} />
      <path d={MARKER_PATH[relationMarker(kind)]} transform="translate(14 0)" fill="currentColor" />
    </svg>
  );
}

export function RelationLayer({
  layout,
  isolation,
  critical,
}: {
  layout: WorkDagLayout;
  isolation: Emphasis | null;
  critical: Emphasis | null;
}) {
  const ids = useId();
  return (
    <svg
      aria-hidden
      className="pointer-events-none absolute inset-0"
      width={layout.width}
      height={layout.height}
      viewBox={`0 0 ${layout.width} ${layout.height}`}
    >
      <defs>
        {TONES.flatMap((tone) =>
          MARKERS.map((shape) => (
            <marker
              key={`${tone}-${shape}`}
              id={`${ids}-${tone}-${shape}`}
              viewBox="0 0 8 8"
              refX="7"
              refY="4"
              markerWidth="7"
              markerHeight="7"
              orient="auto-start-reverse"
            >
              <path d={MARKER_PATH[shape]} fill={toneColor(tone)} />
            </marker>
          )),
        )}
      </defs>
      {layout.strata.map((stratum) => (
        <text
          key={stratum.depth}
          x={6}
          y={stratum.y + 11}
          fill="var(--raw-graph-dim)"
          style={{ fontSize: 'var(--text-3xs)' }}
          fontFamily="var(--font-mono)"
        >
          {String(stratum.depth).padStart(2, '0')}
        </text>
      ))}
      {layout.edges.map((edge, index) => {
        const tone = edgeTone(edge, isolation, critical);
        const isolated = isolation !== null && isolation.edges.has(edge.id);
        const dimmed = isolation !== null && !isolated;
        const pathId = `${ids}-edge-${index}`;
        return (
          <g key={edge.id}>
            <path
              id={pathId}
              d={edge.path}
              fill="none"
              stroke={toneColor(tone)}
              strokeWidth={tone === 'alert' || tone === 'accent' ? 1.75 : 1.1}
              strokeDasharray={gradeDashArray(relationGrade(edge.kind)) || undefined}
              markerEnd={`url(#${ids}-${tone}-${relationMarker(edge.kind)})`}
              opacity={dimmed ? 0.22 : tone === 'edge' ? 0.85 : 1}
              className="transition-opacity duration-[var(--dur-state)]"
              data-work-dag-edge={edge.id}
              data-work-dag-relation={edge.kind}
              data-work-dag-marker={relationMarker(edge.kind)}
              data-work-dag-emphasis={dimmed ? 'dimmed' : tone}
            />
            {isolated ? (
              <text
                dy={-4}
                fill="var(--raw-graph-text)"
                stroke="var(--raw-graph-substrate)"
                strokeWidth={3}
                paintOrder="stroke"
                fontFamily="var(--font-sans)"
                style={{ fontSize: 'var(--text-3xs)' }}
                data-work-dag-edge-label={edge.kind}
              >
                <textPath href={`#${pathId}`} startOffset="50%" textAnchor="middle">
                  {relationLabel(edge.kind)}
                </textPath>
              </text>
            ) : null}
          </g>
        );
      })}
    </svg>
  );
}

type EdgeTone = (typeof TONES)[number];

function edgeTone(
  edge: WorkDagLayoutEdge,
  isolation: Emphasis | null,
  critical: Emphasis | null,
): EdgeTone {
  if (edge.climb) return 'conflicting';
  if (isolation !== null && isolation.edges.has(edge.id)) return 'accent';
  if (critical !== null && critical.edges.has(edge.id)) return 'alert';
  return 'edge';
}

function toneColor(tone: EdgeTone): string {
  switch (tone) {
    case 'edge':
      return 'var(--raw-graph-edge)';
    case 'accent':
      return 'var(--raw-graph-accent)';
    case 'alert':
      return 'var(--raw-graph-alert)';
    case 'conflicting':
      return 'var(--raw-state-conflicting)';
    default: {
      const unhandled: never = tone;
      return unhandled;
    }
  }
}
