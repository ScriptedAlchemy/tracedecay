import type { Coverage, Freshness } from '../contracts/index.ts';
import { cn } from './cn';

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
  coverage?: Coverage | undefined;
  freshness?: Freshness | undefined;
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
      {freshness?.observed_at ? <span>as of {freshness.observed_at}</span> : null}
      {typeof citations === 'number' ? <span>{citations} citations</span> : null}
      {typeof omissions === 'number' && omissions > 0 ? (
        <span className="text-state-partial">{omissions} omitted</span>
      ) : null}
      {scoreKind ? <span className="uppercase tracking-wide">{scoreKind}</span> : null}
    </div>
  );
}

function coverageLabel(coverage?: Coverage): string {
  if (!coverage) return 'coverage unknown';
  const { examined, eligible } = coverage as { examined?: number | null; eligible?: number | null };
  if (typeof examined === 'number' && typeof eligible === 'number' && eligible >= 0) {
    return `coverage ${examined}/${eligible}`;
  }
  if (typeof examined === 'number') return `coverage ${examined}/? (denominator unknown)`;
  return 'coverage unknown';
}
