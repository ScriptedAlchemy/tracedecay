/**
 * Monoline event glyphs and the DOM legend for the temporal execution field.
 *
 * Every event kind has its own SHAPE and its own colour. Colour is spent on
 * evidence grade, so kind has to be legible in monochrome and under
 * forced-colors. Glyphs are drawn in a 16x16 box centred at the origin so the
 * scene can translate and scale them without knowing what they are.
 */
import type { JSX, ReactNode } from 'react';
import { gradeColorVar, gradeDashArray } from './palette.ts';
import { JOURNEY_EVENT_KINDS, type EvidenceGrade, type JourneyEventKind, type SceneGap } from './types.ts';

const GRADES: readonly EvidenceGrade[] = [
  'exact',
  'explicit',
  'inferred',
  'ambiguous',
  'stale',
  'unavailable',
];

function glyphShape(kind: JourneyEventKind): JSX.Element {
  switch (kind) {
    case 'session_start':
      return <circle r={3} fill="currentColor" />;
    case 'session_end':
      return <rect x={-4} y={-4} width={8} height={8} />;
    case 'message_user':
      return <path d="M-6 -5 H6 V3 H-2 L-5 6 V3 H-6 Z" strokeLinejoin="round" />;
    case 'message_assistant':
      return (
        <>
          <rect x={-6} y={-5} width={12} height={10} />
          <line x1={-3.5} y1={-1.5} x2={3.5} y2={-1.5} />
          <line x1={-3.5} y1={1.5} x2={1.5} y2={1.5} />
        </>
      );
    case 'message_other':
      return (
        <>
          <circle r={4} />
          <circle r={1} fill="currentColor" />
        </>
      );
    case 'tool_call':
      return (
        <>
          <line x1={-5.5} y1={5.5} x2={1.5} y2={-1.5} strokeLinecap="round" />
          <circle cx={3.5} cy={-3.5} r={2.5} />
        </>
      );
    case 'spawn':
      return (
        <>
          <line x1={-3} y1={-6} x2={-3} y2={6} strokeLinecap="round" />
          <line x1={-3} y1={1} x2={4} y2={-5} strokeLinecap="round" />
          <circle cx={4} cy={-5} r={1.5} fill="currentColor" />
        </>
      );
    case 'commit':
      return (
        <>
          <circle r={4} />
          <line x1={-8} y1={0} x2={-4} y2={0} />
          <line x1={4} y1={0} x2={8} y2={0} />
        </>
      );
    default: {
      const exhaustive: never = kind;
      throw new Error(`unknown event kind: ${String(exhaustive)}`);
    }
  }
}

export function EventGlyph({ kind, size = 16 }: { kind: JourneyEventKind; size?: number }): JSX.Element {
  return (
    <g
      transform={`scale(${size / 16})`}
      stroke="currentColor"
      fill="none"
      strokeWidth={1.3}
      pointerEvents="none"
      data-glyph={kind}
    >
      {glyphShape(kind)}
    </g>
  );
}

export function glyphLabel(kind: JourneyEventKind): string {
  switch (kind) {
    case 'session_start':
      return 'session start';
    case 'session_end':
      return 'session end';
    case 'message_user':
      return 'user message';
    case 'message_assistant':
      return 'assistant message';
    case 'message_other':
      return 'message';
    case 'tool_call':
      return 'tool call';
    case 'spawn':
      return 'spawn';
    case 'commit':
      return 'commit';
    default: {
      const exhaustive: never = kind;
      throw new Error(`unknown event kind: ${String(exhaustive)}`);
    }
  }
}

function LineSwatch({ grade }: { grade: EvidenceGrade }): JSX.Element {
  return (
    <svg width={28} height={8} aria-hidden="true" className="block shrink-0">
      <line
        x1={0}
        y1={4}
        x2={28}
        y2={4}
        stroke={gradeColorVar(grade)}
        strokeWidth={1.4}
        strokeDasharray={gradeDashArray(grade) || undefined}
      />
    </svg>
  );
}

export function TemporalLegend({
  gaps,
  children,
}: {
  gaps: readonly SceneGap[];
  /** The field's own encodings, printed beside the grade ladder. */
  children?: ReactNode;
}): JSX.Element {
  const pageWide = gaps.filter((gap) => gap.laneId === null);
  return (
    <div className="flex flex-col gap-1.5 border-t border-edge-subtle px-2 py-1.5">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
        <span className="td-legend">Legend</span>
        {GRADES.map((grade) => (
          <span key={grade} className="flex items-center gap-1.5">
            <LineSwatch grade={grade} />
            <span className="td-legend">{grade.toUpperCase()}</span>
          </span>
        ))}
      </div>
      {children}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
        {JOURNEY_EVENT_KINDS.map((kind) => (
          <span key={kind} className="flex items-center gap-1.5 text-text-muted">
            <svg width={16} height={16} viewBox="-8 -8 16 16" aria-hidden="true" className="block shrink-0">
              <EventGlyph kind={kind} />
            </svg>
            <span className="td-legend">{glyphLabel(kind)}</span>
          </span>
        ))}
      </div>
      {pageWide.length > 0 && (
        <ul aria-label="Evidence gaps" className="flex flex-wrap gap-x-4 gap-y-1">
          {pageWide.map((gap) => (
            <li key={gap.id} className="text-3xs text-text-muted" data-legend-gap={gap.kind}>
              {`${gap.kind.replaceAll('_', ' ')} · ${gap.detail}`}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
