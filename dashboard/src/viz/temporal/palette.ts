/**
 * Token sampling and the evidence-grade stroke vocabulary for the temporal
 * execution field.
 *
 * Canvas2D cannot read CSS custom properties, so the field's colour is sampled
 * once per mount and once per theme flip and handed to the renderer as
 * resolved strings. The grade ladder is a LINE-STYLE axis first and a colour
 * axis second: every grade has a distinct dash so the ladder survives
 * monochrome and forced-colors, and only ambiguous/stale borrow the alert hue.
 */
import type { EvidenceGrade } from './types.ts';

export interface TemporalPalette {
  field: string;
  substrate: string;
  text: string;
  textMuted: string;
  edge: string;
  grid: string;
  signal: string;
  signalHot: string;
  amber: string;
  danger: string;
  violet: string;
  ready: string;
  dim: string;
  light: boolean;
}

/** Fallbacks are only reached if a token is missing; they mirror the dark set. */
const FALLBACK: Record<string, string> = {
  '--raw-graph-substrate': '#0b0f1a',
  '--raw-graph-text': '#c3d0e2',
  '--raw-text-muted': '#9aa1b0',
  '--raw-graph-edge': '#3a5876',
  '--raw-grid': '#2a2f38',
  '--raw-graph-accent': '#5fd0e0',
  '--raw-accent-emphasis': '#3fb2c4',
  '--raw-graph-alert': '#e0b45f',
  '--raw-state-error': '#d3564d',
  '--raw-state-locked': '#a08ac8',
  '--raw-state-ready': '#5fc38a',
  '--raw-graph-dim': '#2f3b4d',
};

export function resolveTemporalPalette(element: HTMLElement): TemporalPalette {
  const style = getComputedStyle(element);
  const token = (name: string): string =>
    style.getPropertyValue(name).trim() || FALLBACK[name] || '#888888';
  const light = document.documentElement.dataset['theme'] === 'light';
  const signal = token('--raw-graph-accent');
  return {
    field: token('--raw-graph-substrate'),
    substrate: token('--raw-graph-substrate'),
    text: token('--raw-graph-text'),
    textMuted: token('--raw-text-muted'),
    edge: token('--raw-graph-edge'),
    grid: token('--raw-grid'),
    signal,
    signalHot: style.getPropertyValue('--raw-accent-emphasis').trim() || signal,
    amber: token('--raw-graph-alert'),
    danger: token('--raw-state-error'),
    violet: token('--raw-state-locked'),
    ready: token('--raw-state-ready'),
    dim: token('--raw-graph-dim'),
    light,
  };
}

const GRADE_DASH: Readonly<Record<EvidenceGrade, readonly number[]>> = {
  exact: [],
  explicit: [],
  inferred: [5, 3],
  ambiguous: [3, 3],
  stale: [7, 2, 1, 2],
  unavailable: [1, 4],
};

export function gradeStroke(
  grade: EvidenceGrade,
  palette: TemporalPalette,
): { color: string; dash: readonly number[]; width: number } {
  switch (grade) {
    case 'exact':
      return { color: palette.signal, dash: GRADE_DASH.exact, width: 1.6 };
    case 'explicit':
      return { color: palette.signal, dash: GRADE_DASH.explicit, width: 1.2 };
    case 'inferred':
      return { color: palette.text, dash: GRADE_DASH.inferred, width: 1.2 };
    case 'ambiguous':
      return { color: palette.amber, dash: GRADE_DASH.ambiguous, width: 1.4 };
    case 'stale':
      return { color: palette.amber, dash: GRADE_DASH.stale, width: 1.2 };
    case 'unavailable':
      return { color: palette.dim, dash: GRADE_DASH.unavailable, width: 1.2 };
    default: {
      const exhaustive: never = grade;
      throw new Error(`unknown evidence grade: ${String(exhaustive)}`);
    }
  }
}

/** SVG `stroke-dasharray` for a grade; `''` when the stroke is solid. */
export function gradeDashArray(grade: EvidenceGrade): string {
  return GRADE_DASH[grade].join(' ');
}

/** The same grade colour as `gradeStroke`, as a CSS variable for SVG marks. */
export function gradeColorVar(grade: EvidenceGrade): string {
  switch (grade) {
    case 'exact':
    case 'explicit':
      return 'var(--raw-graph-accent)';
    case 'inferred':
      return 'var(--raw-graph-text)';
    case 'ambiguous':
    case 'stale':
      return 'var(--raw-graph-alert)';
    case 'unavailable':
      return 'var(--raw-graph-dim)';
    default: {
      const exhaustive: never = grade;
      throw new Error(`unknown evidence grade: ${String(exhaustive)}`);
    }
  }
}
