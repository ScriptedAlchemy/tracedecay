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
