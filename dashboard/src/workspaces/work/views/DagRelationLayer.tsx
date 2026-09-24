import { useId } from 'react';
import type { WorkDagLayout, WorkDagLayoutEdge, WorkDagRelationKind } from '../workDagLayout.ts';
import type { Emphasis } from './dagField.ts';

/**
 * The relation paths beneath the layered cards: gating solid, informational
 * dashed, causal dotted, a climb inside a declared cycle in the conflicting
 * hue, isolation cyan and the effort-weighted critical path amber. Hidden
 * from the accessibility tree; the exact table restates every relation.
 */

export function relationDash(kind: WorkDagRelationKind): string | undefined {
  switch (kind) {
    case 'gating':
      return undefined;
    case 'informational':
      return '6 4';
    case 'causal':
      return '2 3';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
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
  const markers = useId();
  return (
    <svg
      aria-hidden
      className="pointer-events-none absolute inset-0"
      width={layout.width}
      height={layout.height}
      viewBox={`0 0 ${layout.width} ${layout.height}`}
    >
      <defs>
        {(['edge', 'accent', 'alert', 'conflicting'] as const).map((tone) => (
          <marker
            key={tone}
            id={`${markers}-${tone}`}
            viewBox="0 0 8 8"
            refX="7"
            refY="4"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M 0 0 L 8 4 L 0 8 z" fill={toneColor(tone)} />
          </marker>
        ))}
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
      {layout.edges.map((edge) => {
        const tone = edgeTone(edge, isolation, critical);
        const dimmed = isolation !== null && !isolation.edges.has(edge.id);
        return (
          <path
            key={edge.id}
            d={edge.path}
            fill="none"
            stroke={toneColor(tone)}
            strokeWidth={tone === 'alert' || tone === 'accent' ? 1.75 : 1.1}
            strokeDasharray={relationDash(edge.kind)}
            markerEnd={`url(#${markers}-${tone})`}
            opacity={dimmed ? 0.22 : tone === 'edge' ? 0.85 : 1}
            className="transition-opacity duration-[var(--dur-state)]"
            data-work-dag-edge={edge.id}
            data-work-dag-relation={edge.kind}
            data-work-dag-emphasis={dimmed ? 'dimmed' : tone}
          />
        );
      })}
    </svg>
  );
}

type EdgeTone = 'edge' | 'accent' | 'alert' | 'conflicting';

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
