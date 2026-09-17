import { cn } from './cn';

/**
 * The evidence-grade ladder from the V2 design system: an ordered description
 * of the support behind a displayed claim, never a confidence percentage.
 * `source` is orthogonal provenance (`REGISTRY`, `CAS RECEIPT`, `RUN JOURNAL`)
 * and rides beside the grade; it never replaces one.
 */
export type EvidenceGrade =
  | 'EXACT'
  | 'EXPLICIT'
  | 'INFERRED'
  | 'AMBIGUOUS'
  | 'STALE'
  | 'UNAVAILABLE';

/** Hue plus a line pattern per grade, so the ladder survives monochrome and
 * forced colours: solid for direct facts, dashed for inferred, hatched for
 * degraded, crosshatched for unavailable. */
const GRADE: Record<EvidenceGrade, { lamp: string; pattern: string }> = {
  EXACT: { lamp: 'bg-state-ready', pattern: 'border-solid' },
  EXPLICIT: { lamp: 'bg-accent', pattern: 'border-solid' },
  INFERRED: { lamp: 'bg-state-loading', pattern: 'border-dashed' },
  AMBIGUOUS: { lamp: 'bg-state-partial', pattern: 'border-dotted' },
  STALE: { lamp: 'bg-state-stale', pattern: 'border-dashed' },
  UNAVAILABLE: { lamp: 'bg-state-locked', pattern: 'border-dotted' },
};

export function GradeTag({
  grade,
  source,
  className,
}: {
  grade: EvidenceGrade;
  source?: string;
  className?: string;
}) {
  const visual = GRADE[grade];
  return (
    <span
      className={cn(
        'inline-flex max-w-full items-center gap-1.5 border border-edge-subtle px-1.5 py-px text-3xs',
        visual.pattern,
        className,
      )}
      data-grade={grade}
      data-evidence-grade={grade}
    >
      <span aria-hidden className={cn('size-1.5 shrink-0', visual.lamp)} />
      {source ? (
        <span className="truncate tracking-[0.1em] text-text-muted">{source.toUpperCase()}</span>
      ) : null}
      {source ? <span aria-hidden className="text-text-muted">/</span> : null}
      <span className="uppercase tracking-[0.12em] text-text-secondary">{grade}</span>
    </span>
  );
}

/** A named absence: the field the plate would show, and the reason the
 * contract does not carry it. Always text — never a blank cell. */
export function Absence({
  field,
  reason,
  className,
}: {
  field: string;
  reason: string;
  className?: string;
}) {
  return (
    <div
      className={cn('flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5 text-3xs', className)}
      data-absence={field}
    >
      <span className="td-legend text-text-muted">{field}</span>
      <GradeTag grade="UNAVAILABLE" />
      <span className="min-w-0 text-text-muted">{reason}</span>
    </div>
  );
}

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
  grade: EvidenceGradeKind | EvidenceGrade;
  source?: string;
  className?: string;
}) {
  if (grade === grade.toUpperCase()) {
    return <GradeTag grade={grade as EvidenceGrade} source={source} className={className} />;
  }
  const normalized = grade as EvidenceGradeKind;
  return (
    <span
      className={cn(
        'td-legend inline-flex flex-wrap items-center gap-1.5 whitespace-normal',
        INK[normalized],
        className,
      )}
      data-evidence-grade={grade}
    >
      <Glyph grade={normalized} />
      {source ? <span>{source} / </span> : null}
      <span>{LABEL[normalized]}</span>
    </span>
  );
}

export function EvidenceGradeTag({
  grade,
  sourceClass,
  className,
}: {
  grade: EvidenceGrade;
  sourceClass?: string;
  className?: string;
}) {
  return <GradeTag grade={grade} source={sourceClass} className={className} />;
}
