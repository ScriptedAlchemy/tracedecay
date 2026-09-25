import type { ReactNode } from 'react';
import { Lock } from 'lucide-react';
import type { DeliveryProviderStateV1 } from '../../contracts/generated.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn.ts';
import {
  gradeDash,
  gradeLabel,
  providerStateKind,
  sourceClassLabel,
  type EvidenceGrade,
  type SourceClass,
} from './evidence.ts';

/**
 * Delivery's small shared marks. Every one prints its meaning as text beside
 * its shape or stroke, so grade and provider state survive monochrome,
 * forced colours and a screen reader.
 */

const GRADE_TONE: Record<EvidenceGrade, string> = {
  exact: 'text-accent',
  explicit: 'text-text-primary',
  inferred: 'text-text-secondary',
  ambiguous: 'text-state-conflicting',
  stale: 'text-state-stale',
  unavailable: 'text-state-offline',
};

/** `SOURCE / GRADE` with the ladder's stroke grammar drawn beside it. */
export function GradeMark({
  grade,
  source,
  className,
}: {
  grade: EvidenceGrade;
  source?: SourceClass | undefined;
  className?: string;
}) {
  return (
    <span
      className={cn(
        'td-legend inline-flex items-center gap-1.5 whitespace-nowrap',
        GRADE_TONE[grade],
        className,
      )}
      data-grade={grade}
    >
      <svg aria-hidden width="18" height="6" viewBox="0 0 18 6" className="shrink-0">
        <line
          x1="0"
          y1="3"
          x2="18"
          y2="3"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeDasharray={gradeDash(grade)}
        />
      </svg>
      <span>
        {source === undefined ? gradeLabel(grade) : `${sourceClassLabel(source)} / ${gradeLabel(grade)}`}
      </span>
    </span>
  );
}

export function ProviderStateChip({
  state,
  detail,
}: {
  state: DeliveryProviderStateV1;
  detail?: string;
}) {
  return (
    <StateChip
      kind={providerStateKind(state)}
      detail={detail ?? state.replaceAll('_', ' ')}
    />
  );
}

/** Provider objects are read here and never written: the badge says so where
 * a reader might expect a merge, rerun or post control. */
export function ReadOnlyProviderBadge({ className }: { className?: string }) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 border border-edge-strong px-2 py-1 font-mono text-3xs uppercase tracking-[0.14em] text-text-secondary',
        className,
      )}
    >
      <Lock aria-hidden size={11} className="text-state-locked" />
      read-only provider
    </span>
  );
}

/** A square-cornered link or button in the instrument's control register. */
export function ControlLink({
  href,
  children,
  external,
  className,
}: {
  href: string;
  children: ReactNode;
  external?: boolean;
  className?: string;
}) {
  return (
    <a
      href={href}
      {...(external ? { target: '_blank', rel: 'noreferrer noopener' } : {})}
      className={cn(
        'inline-flex min-h-[var(--touch-target-min)] items-center gap-1.5 border border-edge-strong px-3 text-xs text-text-primary hover:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent',
        className,
      )}
    >
      {children}
    </a>
  );
}

/** A control that exists in the concept but has no production write path.
 * Mounted only with the daemon-owned reason, visibly disabled. */
export function UnavailableControl({
  label,
  reason,
}: {
  label: string;
  reason: string;
}) {
  return (
    <span className="inline-flex min-h-9 items-center gap-2 border border-dashed border-edge-subtle px-3 text-xs text-text-muted">
      <span className="line-through decoration-edge-strong">{label}</span>
      <span className="font-mono text-3xs uppercase tracking-[0.1em]">unavailable · {reason}</span>
    </span>
  );
}

/** A compact key/value pair for identity rows: legend, then mono value. */
export function IdentityRow({
  label,
  value,
  href,
}: {
  label: string;
  value: string;
  href?: string | null;
}) {
  return (
    <div className="flex min-w-0 items-baseline justify-between gap-3 border-b border-edge-subtle py-1 last:border-b-0">
      <span className="td-legend shrink-0">{label}</span>
      {href ? (
        <a href={href} className="min-w-0 truncate font-mono text-3xs text-accent hover:underline">
          {value}
        </a>
      ) : (
        <span className="min-w-0 truncate font-mono text-3xs text-text-secondary" title={value}>
          {value}
        </span>
      )}
    </div>
  );
}

export function shortSha(sha: string | null | undefined, length = 12): string {
  if (sha === null || sha === undefined || sha === '') return '—';
  return sha.slice(0, length);
}

export function microsToIso(micros: number | null | undefined): string {
  if (micros === null || micros === undefined) return 'not observed';
  return new Date(Math.floor(micros / 1000)).toISOString();
}
