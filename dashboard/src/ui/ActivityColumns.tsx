import { useState } from 'react';
import { cn } from './cn';

export interface ActivityBucket {
  label: string;
  value: number;
  hint?: string;
}

/** A calm-density activity column strip: inline SVG, token-driven, direct
 * hover labeling, accessible summary — the plan-11a micro-viz idiom. */
export function ActivityColumns({
  buckets,
  className,
  height = 56,
}: {
  buckets: ActivityBucket[];
  className?: string;
  height?: number;
}) {
  const [active, setActive] = useState<number | null>(null);
  if (buckets.length === 0) return null;
  const max = Math.max(...buckets.map((b) => b.value), 1);
  const gap = 2;
  const barWidth = 8;
  const width = buckets.length * (barWidth + gap);
  const activeBucket = active != null ? buckets[active] : undefined;
  const total = buckets.reduce((sum, b) => sum + b.value, 0);

  return (
    <figure className={cn('flex flex-col gap-1', className)}>
      <figcaption className="flex items-baseline justify-between text-2xs text-text-muted">
        {activeBucket ? (
          <>
            <span>{activeBucket.label}</span>
            <span className="tabular text-text-secondary">
              {activeBucket.value.toLocaleString()}
              {activeBucket.hint ? ` · ${activeBucket.hint}` : ''}
            </span>
          </>
        ) : (
          <>
            <span>{buckets.length} days</span>
            <span className="tabular">{total.toLocaleString()} total</span>
          </>
        )}
      </figcaption>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        className="w-full"
        style={{ height }}
        role="img"
        aria-label={`Activity over ${buckets.length} days, ${total.toLocaleString()} total`}
        onMouseLeave={() => setActive(null)}
      >
        {buckets.map((bucket, i) => {
          const h = Math.max(2, (bucket.value / max) * (height - 4));
          return (
            <rect
              key={i}
              x={i * (barWidth + gap)}
              y={height - h}
              width={barWidth}
              height={h}
              rx={1.5}
              className={cn(
                'transition-opacity duration-[var(--dur-state)]',
                active === null || active === i ? 'opacity-100' : 'opacity-35',
              )}
              fill={active === i ? 'var(--raw-accent)' : 'var(--raw-accent)'}
              fillOpacity={active === i ? 1 : 0.55}
              onMouseEnter={() => setActive(i)}
            />
          );
        })}
      </svg>
    </figure>
  );
}

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
  if (usedBytes == null) {
    return <span className="text-2xs text-text-muted">size unknown</span>;
  }
  const free = freeBytes ?? 0;
  const freeFraction = usedBytes > 0 ? Math.min(free / usedBytes, 1) : 0;
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
          <pattern id="td-hatch" width="4" height="4" patternUnits="userSpaceOnUse">
            <path d="M0 4 L4 0" stroke="var(--raw-state-stale)" strokeWidth="1" />
          </pattern>
        </defs>
        <rect width="100%" height="100%" fill="url(#td-hatch)" opacity="0.8" />
      </svg>
    </div>
  );
}
