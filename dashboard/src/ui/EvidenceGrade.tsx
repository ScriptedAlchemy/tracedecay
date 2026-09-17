import { cn } from './cn';
import type { EvidenceQuality } from './EvidencePattern.tsx';

/**
 * The evidence-grade ladder: exactly one grade per displayed claim, describing
 * the support behind it. Ordered, and not a confidence percentage.
 *
 *   EXACT        a direct source fact or stable identity from its authority
 *   EXPLICIT     a persisted user or agent claim, attributed to its speaker
 *   INFERRED     a relation derived by correlation, with its basis named
 *   AMBIGUOUS    several plausible candidates remain; none is chosen silently
 *   STALE        a source exists but its freshness window has elapsed
 *   UNAVAILABLE  missing, denied, private or not ingested — an honest gap
 *
 * Source class (`RETAINED`, `OBSERVED`, `TRANSCRIPT`, …) is orthogonal metadata
 * that may ride beside the grade; it never replaces one.
 */
export type EvidenceGradeKind =
  | 'EXACT'
  | 'EXPLICIT'
  | 'INFERRED'
  | 'AMBIGUOUS'
  | 'STALE'
  | 'UNAVAILABLE';

/** Grade rendered on the pattern axis, so it survives monochrome and forced
 * colours: solid for direct facts, hatched for correlation or elapsed
 * freshness, dotted for unresolved candidates, dashed for a gap. */
const PATTERN: Record<EvidenceGradeKind, EvidenceQuality> = {
  EXACT: 'measured',
  EXPLICIT: 'measured',
  INFERRED: 'associated',
  STALE: 'associated',
  AMBIGUOUS: 'predicted',
  UNAVAILABLE: 'unknown',
};

const SWATCH: Record<EvidenceQuality, string> = {
  measured: 'var(--ev-measured)',
  associated: 'var(--ev-associated)',
  predicted: 'var(--ev-predicted)',
  unknown: 'var(--ev-unknown)',
};

export function EvidenceGrade({
  grade,
  source,
  className,
}: {
  grade: EvidenceGradeKind;
  /** Where or how the evidence was persisted or observed. */
  source?: string | undefined;
  className?: string;
}) {
  const quality = PATTERN[grade];
  return (
    <span
      className={cn('inline-flex items-center gap-1.5 whitespace-nowrap', className)}
      data-evidence-grade={grade}
    >
      <span
        aria-hidden
        className="h-2 w-4 shrink-0 border border-edge-subtle opacity-80"
        style={{
          backgroundImage: SWATCH[quality],
          backgroundSize: quality === 'predicted' ? '4px 4px' : undefined,
        }}
      />
      <span className="td-legend text-text-secondary">
        {source ? `${source} / ${grade}` : grade}
      </span>
    </span>
  );
}
