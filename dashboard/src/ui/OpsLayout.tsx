import type { ReactNode } from 'react';
import type { LucideIcon } from 'lucide-react';
import { cn } from './cn';
import {
  FRESHNESS_BOUNDS,
  freshnessSteps,
  relativeAge,
  type FreshnessTier,
} from './time.ts';

/* ==========================================================================
 * The operational surfaces (Delivery, Automations) share one reading grammar:
 *
 *   1. A page header that states what the surface is and carries the single
 *      headline state on the right.
 *   2. Panels — one read model each, on surface-1, never nested more than one
 *      deep.
 *   3. Section rules — an editorial hairline label that segments a panel
 *      without spending a border box.
 *   4. State as GROUPING, not as a per-row chip. A long list stays scannable
 *      because "blocked", "paused" and "running" are headings with counts and
 *      a bracket spine, so the eye reads one column instead of a rainbow.
 *   5. Freshness as one ordered 4-step meter, used identically on both pages.
 * ========================================================================== */

export function PageHeader({
  title,
  summary,
  trailing,
}: {
  title: string;
  summary: string;
  trailing?: ReactNode;
}) {
  return (
    <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-edge-subtle bg-surface-1 px-3 py-2.5 sm:px-4">
      <h1 className="shrink-0 text-sm font-semibold tracking-tight">{title}</h1>
      {trailing ? (
        <div className="order-last ml-auto flex min-w-0 shrink items-center gap-2 md:order-none">
          {trailing}
        </div>
      ) : null}
      <p className="min-w-0 basis-full text-2xs leading-relaxed text-text-muted md:order-first md:basis-auto">
        <span className="md:hidden">{summary}</span>
        <span className="max-md:hidden">{summary}</span>
      </p>
    </header>
  );
}

/** One read model. `aria-label` comes from the title so the region is
 * addressable even when the heading is visually quiet. */
export function Panel({
  title,
  icon: Icon,
  trailing,
  footer,
  children,
  className,
}: {
  title: string;
  icon?: LucideIcon;
  trailing?: ReactNode;
  footer?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section
      aria-label={title}
      className={cn(
        'flex min-w-0 flex-col rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1',
        className,
      )}
    >
      <header className="flex items-center gap-2 px-3 py-2">
        {Icon ? <Icon aria-hidden size={13} className="shrink-0 text-text-muted" /> : null}
        <h2 className="min-w-0 truncate text-xs font-semibold tracking-tight">{title}</h2>
        {trailing ? <div className="ml-auto flex shrink-0 items-center gap-2">{trailing}</div> : null}
      </header>
      <div className="min-w-0 flex-1 px-3 pb-3">{children}</div>
      {footer ? (
        <footer className="border-t border-edge-subtle px-3 py-2 text-2xs leading-relaxed text-text-muted">
          {footer}
        </footer>
      ) : null}
    </section>
  );
}

/** Editorial hairline label: an uppercase micro-heading whose rule runs to the
 * end of the column. Segments a panel without spending another border box. */
export function SectionRule({
  label,
  trailing,
  className,
}: {
  label: string;
  trailing?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex items-center gap-2 py-1.5', className)}>
      <span className="shrink-0 text-2xs font-medium uppercase tracking-wider text-text-muted">
        {label}
      </span>
      <span aria-hidden className="h-px min-w-4 flex-1 bg-edge-subtle" />
      {trailing ? <span className="shrink-0 text-2xs text-text-muted">{trailing}</span> : null}
    </div>
  );
}

/**
 * State-as-grouping. The heading carries icon + word + count (never colour
 * alone); the rows below sit inside a bracket spine so the group reads as one
 * block at a glance, however long the list gets.
 */
export function StateGroup({
  icon: Icon,
  label,
  count,
  note,
  tone = 'neutral',
  children,
}: {
  icon: LucideIcon;
  label: string;
  count: number;
  note?: string;
  /** Tints only the group's icon and spine. The word and the count carry the
   * meaning on their own. */
  tone?: 'active' | 'held' | 'neutral' | 'attention';
  children: ReactNode;
}) {
  const iconTone =
    tone === 'active'
      ? 'text-state-ready'
      : tone === 'held'
        ? 'text-state-stale'
        : tone === 'attention'
          ? 'text-state-partial'
          : 'text-text-muted';
  const spineTone =
    tone === 'active'
      ? 'bg-state-ready/40'
      : tone === 'held'
        ? 'bg-state-stale/40'
        : tone === 'attention'
          ? 'bg-state-partial/40'
          : 'bg-edge-subtle';
  return (
    <section aria-label={`${label} (${count})`} className="min-w-0">
      <div className="flex items-center gap-2 py-1.5">
        <Icon aria-hidden size={12} className={cn('shrink-0', iconTone)} />
        <span className="shrink-0 text-2xs font-semibold uppercase tracking-wider text-text-secondary">
          {label}
        </span>
        <span className="tabular shrink-0 text-2xs text-text-muted">{count}</span>
        <span aria-hidden className="h-px min-w-4 flex-1 bg-edge-subtle" />
        {note ? <span className="shrink-0 text-2xs text-text-muted">{note}</span> : null}
      </div>
      <div className="relative min-w-0 pl-3">
        <span aria-hidden className={cn('absolute bottom-1 left-[5px] top-0 w-px', spineTone)} />
        <div className="flex min-w-0 flex-col">{children}</div>
      </div>
    </section>
  );
}

/**
 * Freshness as one ordered four-step meter. Steps rise left to right, so
 * recency is legible as a shape; the age text is always rendered beside it, so
 * the meter is never the sole carrier of the meaning.
 */
export function FreshnessMeter({
  tier,
  label,
  className,
  hideLabel,
}: {
  tier: FreshnessTier;
  /** The text shown beside the meter — normally a relative age. */
  label: string;
  className?: string;
  /** Only for legends, where the tier word is already adjacent. */
  hideLabel?: boolean;
}) {
  const filled = freshnessSteps(tier);
  const fill = filled >= 3 ? 'bg-state-ready' : 'bg-state-stale';
  return (
    <span
      className={cn('inline-flex shrink-0 items-center gap-1.5 text-2xs', className)}
      data-freshness={tier}
      title={`${tier} — ${FRESHNESS_BOUNDS[tier]}`}
    >
      <span
        aria-hidden
        className="inline-flex h-3 items-end gap-[2px]"
      >
        {[0, 1, 2, 3].map((i) => (
          <span
            key={i}
            className={cn(
              'w-[3px] rounded-[1px]',
              i < filled ? fill : 'bg-edge-subtle',
            )}
            style={{ height: `${4 + i * 2.6}px` }}
          />
        ))}
      </span>
      {hideLabel ? (
        <span className="sr-only">{label}</span>
      ) : (
        <span className="tabular text-text-secondary">{label}</span>
      )}
    </span>
  );
}

/** Age with an explicit absence: no timestamp renders as a stated gap, never
 * as a fabricated "now". */
export function AgeText({
  epochSecs,
  nowSecs,
  absent = 'not recorded',
  className,
}: {
  epochSecs: number | null | undefined;
  nowSecs: number;
  absent?: string;
  className?: string;
}) {
  const age = relativeAge(epochSecs, nowSecs);
  return (
    <span className={cn('tabular text-2xs', age ? 'text-text-secondary' : 'text-text-muted', className)}>
      {age ?? absent}
    </span>
  );
}

/** A quiet inline tag for a categorical fact carried verbatim from the wire
 * (kind, category, target). Deliberately monochrome: these are labels, not
 * states, and must not compete with the state axis. */
export function WireTag({ children, mono }: { children: ReactNode; mono?: boolean }) {
  return (
    <span
      className={cn(
        'inline-flex shrink-0 items-center rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 py-px text-2xs text-text-muted',
        mono && 'font-mono',
      )}
    >
      {children}
    </span>
  );
}

/** Big-number readout for a summary band. */
export function Readout({
  value,
  label,
  hint,
}: {
  value: ReactNode;
  label: string;
  hint?: string;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <span className="tabular text-xl font-semibold leading-none text-text-primary" data-cell="numeric">
        {value}
      </span>
      <span className="text-2xs uppercase tracking-wide text-text-muted">{label}</span>
      {hint ? <span className="text-2xs text-text-muted">{hint}</span> : null}
    </div>
  );
}
