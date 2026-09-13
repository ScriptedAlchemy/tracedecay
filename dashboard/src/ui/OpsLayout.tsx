import { cn } from './cn';
import {
  FRESHNESS_BOUNDS,
  freshnessSteps,
  type FreshnessTier,
} from './time.ts';

/**
 * Freshness as one ordered four-step meter. The adjacent age text carries the
 * meaning in words, so colour is never the only signal.
 */
export function FreshnessMeter({
  tier,
  label,
  className,
}: {
  tier: FreshnessTier;
  label: string;
  className?: string;
}) {
  const filled = freshnessSteps(tier);
  const fill = filled >= 3 ? 'bg-state-ready' : 'bg-state-stale';
  return (
    <span
      className={cn('inline-flex shrink-0 items-center gap-1.5 text-2xs', className)}
      data-freshness={tier}
      title={`${tier} — ${FRESHNESS_BOUNDS[tier]}`}
    >
      <span aria-hidden className="inline-flex h-3 items-end gap-[2px]">
        {[0, 1, 2, 3].map((index) => (
          <span
            key={index}
            className={cn(
              'w-[3px] rounded-[1px]',
              index < filled ? fill : 'bg-edge-subtle',
            )}
            style={{ height: `${4 + index * 2.6}px` }}
          />
        ))}
      </span>
      <span className="tabular text-text-secondary">{label}</span>
    </span>
  );
}
