import { useId } from 'react';
import { cn } from './cn';

/** Proportional capacity bar: filled area = used fraction; the free-page
 * region renders hatched (evidence-quality pattern axis, not a color). */
export function CapacityBar({
  usedBytes,
  freeBytes,
  className,
}: {
  usedBytes: number | null;
  freeBytes: number | null;
  className?: string;
}) {
  // One hatch pattern per instance: a fixed id makes every bar on the page
  // resolve `url(#…)` against whichever instance rendered first, and
  // `ObservatoryPage` draws one bar per store.
  const hatchId = useId();
  if (usedBytes == null) {
    return <span className="text-sm text-text-muted">size unknown</span>;
  }
  // An absent free-page figure is not zero free pages. Coercing it drew a
  // completely filled bar announcing "0.0% free pages", a measurement of a
  // full store, for a store whose pages nobody sampled.
  if (freeBytes == null) {
    return <span className="text-sm text-text-muted">free pages unknown</span>;
  }
  const freeFraction = usedBytes > 0 ? Math.min(freeBytes / usedBytes, 1) : 0;
  return (
    <div
      className={cn('relative h-2 overflow-hidden rounded-full bg-surface-3', className)}
      role="img"
      aria-label={`store size with ${(freeFraction * 100).toFixed(1)}% free pages`}
    >
      <div
        className="absolute inset-y-0 left-0 rounded-full bg-accent/70"
        style={{ width: `${(1 - freeFraction) * 100}%` }}
      />
      <svg className="absolute inset-y-0 right-0" style={{ width: `${freeFraction * 100}%` }}>
        <defs>
          <pattern id={hatchId} width="4" height="4" patternUnits="userSpaceOnUse">
            <path d="M0 4 L4 0" stroke="var(--raw-state-stale)" strokeWidth="1" />
          </pattern>
        </defs>
        <rect width="100%" height="100%" fill={`url(#${hatchId})`} opacity="0.8" />
      </svg>
    </div>
  );
}
