import { cn } from './cn';

/**
 * The evidence-grade ladder: an ordered description of the support behind a
 * displayed claim, never a confidence percentage. Every grade is carried by
 * its text and its border pattern together, so it survives monochrome,
 * forced colours and a screen reader alike.
 */
export type EvidenceGrade = 'EXACT' | 'EXPLICIT' | 'INFERRED' | 'AMBIGUOUS' | 'STALE' | 'UNAVAILABLE';

const PATTERN: Record<EvidenceGrade, string> = {
  EXACT: 'border-solid border-edge-strong text-text-secondary',
  EXPLICIT: 'border-solid border-edge-subtle text-text-secondary',
  INFERRED: 'border-dashed border-edge-strong text-text-secondary',
  AMBIGUOUS: 'border-dotted border-edge-strong text-text-secondary',
  STALE: 'border-dashed border-state-stale text-text-secondary',
  UNAVAILABLE: 'border-dashed border-edge-subtle text-text-muted',
};

/** One graded claim's tag, with the optional source class printed beside the
 * grade (`TRANSCRIPT / EXACT`). Source class says where the evidence was
 * persisted or observed; it never replaces the grade. */
export function EvidenceGradeTag({
  grade,
  sourceClass,
  className,
}: {
  grade: EvidenceGrade;
  sourceClass?: string | undefined;
  className?: string;
}) {
  return (
    <span
      data-evidence-grade={grade}
      className={cn(
        'inline-flex shrink-0 items-baseline gap-1 border px-1 py-px text-3xs uppercase tracking-[0.12em] tabular',
        PATTERN[grade],
        className,
      )}
    >
      {sourceClass ? <span className="text-text-muted">{sourceClass} /</span> : null}
      <span>{grade}</span>
    </span>
  );
}
