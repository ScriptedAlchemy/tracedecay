import { cn } from './cn';

export interface EvidenceCoverage {
  completeness?: string;
  eligible?: number | null;
  examined?: number | null;
}

export interface EvidenceFreshness {
  state?: string;
  observed_at?: string;
}

/** Always-visible truth strip (plan 11): coverage with denominator, freshness
 * age, counts. Unknown denominators NEVER render a percent or a meter. */
export function EvidenceTruthStrip({
  coverage,
  freshness,
  citations,
  omissions,
  scoreKind,
  className,
}: {
  coverage?: EvidenceCoverage | undefined;
  freshness?: EvidenceFreshness | undefined;
  citations?: number | undefined;
  omissions?: number | undefined;
  scoreKind?: string | undefined;
  className?: string;
}) {
  return (
    <div
      className={cn(
        'flex flex-wrap items-center gap-x-3 gap-y-1 text-2xs text-text-muted tabular',
        className,
      )}
      aria-label="Evidence"
    >
      <span>{coverageLabel(coverage)}</span>
      {freshness?.state ? <span>freshness {freshness.state}</span> : null}
      {freshness?.observed_at ? <span>as of {freshness.observed_at}</span> : null}
      {typeof citations === 'number' ? <span>{citations} citations</span> : null}
      {typeof omissions === 'number' && omissions > 0 ? (
        <span className="inline-flex items-center gap-1 text-text-secondary">
          {/* Partial-state hue rides the dot; the count stays AA-contrast. */}
          <span aria-hidden className="size-1.5 rounded-full bg-state-partial" />
          {omissions} omitted
        </span>
      ) : null}
      {scoreKind ? <span className="uppercase tracking-wide">{scoreKind}</span> : null}
    </div>
  );
}

function coverageLabel(coverage?: EvidenceCoverage): string {
  if (!coverage) return 'coverage unknown';
  const { completeness, examined, eligible } = coverage;
  const qualifier = completeness && completeness !== 'complete' ? ` · ${completeness}` : '';
  if (typeof examined === 'number' && typeof eligible === 'number' && eligible >= 0) {
    return `coverage ${examined}/${eligible}${qualifier}`;
  }
  if (typeof examined === 'number') {
    return `coverage ${examined}/? (denominator unknown)${qualifier}`;
  }
  return completeness ? `coverage ${completeness}` : 'coverage unknown';
}
