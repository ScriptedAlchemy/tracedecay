import { cn } from './cn';

export type EvidenceQuality = 'measured' | 'associated' | 'predicted' | 'unknown';

const PATTERN: Record<EvidenceQuality, string> = {
  measured: 'var(--ev-measured)',
  associated: 'var(--ev-associated)',
  predicted: 'var(--ev-predicted)',
  unknown: 'var(--ev-unknown)',
};

/** Evidence quality rendered as the plan's PATTERN axis (solid, hatched,
 * dotted, dashed) — never a color — with the label alongside so the meaning
 * survives monochrome, forced-colors, and screen readers alike. */
export function EvidencePattern({
  quality,
  className,
}: {
  quality: EvidenceQuality;
  className?: string;
}) {
  return (
    <span className={cn('inline-flex items-center gap-1.5 text-2xs text-text-muted', className)}>
      <span
        aria-hidden
        className="h-2.5 w-5 rounded-[2px] border border-edge-subtle opacity-80"
        style={{
          backgroundImage: PATTERN[quality],
          backgroundSize: quality === 'predicted' ? '4px 4px' : undefined,
        }}
      />
      {quality}
    </span>
  );
}
