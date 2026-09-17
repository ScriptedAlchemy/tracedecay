import { cn } from './cn';

/**
 * The evidence-grade ladder from the V2 design system.
 *
 * Grade is an ordered description of the support behind a displayed claim; it
 * is never a confidence percentage. Every chip prints the grade as text and
 * pairs it with a glyph whose LINE STYLE carries the same meaning, so the
 * ladder survives monochrome, forced colours, and a screen reader alike:
 *
 *   EXACT        solid     a direct fact read from its owning authority
 *   EXPLICIT     solid     a persisted claim or decision, attributed
 *   INFERRED     dashed    a correlation the browser or a projector derived
 *   AMBIGUOUS    dotted    several candidates remain; none was chosen
 *   STALE        hatched   a source exists but its freshness window elapsed
 *   UNAVAILABLE  hollow    missing, denied, private, or not ingested
 *
 * `source` is orthogonal metadata — where the evidence was persisted or
 * observed (`GRAPH`, `KANBAN`, `STREAM`, …) — and never replaces the grade.
 */
export type EvidenceGradeKind =
  | 'exact'
  | 'explicit'
  | 'inferred'
  | 'ambiguous'
  | 'stale'
  | 'unavailable';

const LABEL: Record<EvidenceGradeKind, string> = {
  exact: 'EXACT',
  explicit: 'EXPLICIT',
  inferred: 'INFERRED',
  ambiguous: 'AMBIGUOUS',
  stale: 'STALE',
  unavailable: 'UNAVAILABLE',
};

const INK: Record<EvidenceGradeKind, string> = {
  exact: 'text-text-secondary',
  explicit: 'text-text-secondary',
  inferred: 'text-text-muted',
  ambiguous: 'text-state-partial',
  stale: 'text-state-stale',
  unavailable: 'text-text-muted',
};

function Glyph({ grade }: { grade: EvidenceGradeKind }) {
  switch (grade) {
    case 'exact':
    case 'explicit':
      return <span aria-hidden className="h-px w-3 shrink-0 bg-current" />;
    case 'inferred':
      return (
        <span
          aria-hidden
          className="h-px w-3 shrink-0"
          style={{
            backgroundImage:
              'repeating-linear-gradient(90deg, currentColor 0 3px, transparent 3px 5px)',
          }}
        />
      );
    case 'ambiguous':
      return (
        <span
          aria-hidden
          className="h-px w-3 shrink-0"
          style={{
            backgroundImage:
              'repeating-linear-gradient(90deg, currentColor 0 1px, transparent 1px 3px)',
          }}
        />
      );
    case 'stale':
      return (
        <span
          aria-hidden
          className="h-1.5 w-3 shrink-0"
          style={{
            backgroundImage:
              'repeating-linear-gradient(45deg, currentColor 0 1px, transparent 1px 3px)',
          }}
        />
      );
    case 'unavailable':
      return <span aria-hidden className="h-1.5 w-3 shrink-0 border border-dashed border-current" />;
    default: {
      const unhandled: never = grade;
      return unhandled;
    }
  }
}

export function EvidenceGrade({
  grade,
  source,
  className,
}: {
  grade: EvidenceGradeKind;
  /** Source class printed beside the grade: `GRAPH`, `KANBAN`, `STREAM`. */
  source?: string;
  className?: string;
}) {
  return (
    <span
      className={cn('td-legend inline-flex flex-wrap items-center gap-1.5 whitespace-normal', INK[grade], className)}
      data-evidence-grade={grade}
    >
      <Glyph grade={grade} />
      {source ? <span>{source} / </span> : null}
      <span>{LABEL[grade]}</span>
    </span>
  );
}
