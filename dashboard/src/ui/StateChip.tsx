import type { LucideIcon } from 'lucide-react';
import {
  AlertTriangle,
  Ban,
  CheckCircle2,
  CircleSlash,
  Clock,
  CloudOff,
  EyeOff,
  FileQuestion,
  HelpCircle,
  Loader2,
  Lock,
  ShieldAlert,
  ShieldX,
  Split,
  XCircle,
} from 'lucide-react';
import { cn } from './cn';

/** The sixteen-state domain taxonomy (plan 11). Token + icon + label —
 * never color alone. */
export type DomainStateKind =
  | 'loading'
  | 'complete_zero_findings'
  | 'ready'
  | 'partial'
  | 'stale'
  | 'locked'
  | 'denied'
  | 'unauthorized'
  | 'redacted'
  | 'conflicting'
  | 'offline'
  | 'unknown'
  | 'cancelled'
  | 'timed_out'
  | 'error'
  | 'unsupported'
  | 'unsupported_schema';

const STATE: Record<
  DomainStateKind,
  { label: string; icon: LucideIcon; tokenClass: string; spin?: boolean }
> = {
  loading: { label: 'Loading', icon: Loader2, tokenClass: 'text-state-loading', spin: true },
  complete_zero_findings: {
    label: 'Complete · zero findings',
    icon: CheckCircle2,
    tokenClass: 'text-state-complete-zero',
  },
  ready: { label: 'Ready', icon: CheckCircle2, tokenClass: 'text-state-ready' },
  partial: { label: 'Partial', icon: AlertTriangle, tokenClass: 'text-state-partial' },
  stale: { label: 'Stale', icon: Clock, tokenClass: 'text-state-stale' },
  locked: { label: 'Locked', icon: Lock, tokenClass: 'text-state-locked' },
  denied: { label: 'Denied', icon: ShieldX, tokenClass: 'text-state-denied' },
  unauthorized: { label: 'Unauthorized', icon: ShieldAlert, tokenClass: 'text-state-unauthorized' },
  redacted: { label: 'Redacted', icon: EyeOff, tokenClass: 'text-state-redacted' },
  conflicting: { label: 'Conflicting', icon: Split, tokenClass: 'text-state-conflicting' },
  offline: { label: 'Offline', icon: CloudOff, tokenClass: 'text-state-offline' },
  unknown: { label: 'Unknown', icon: HelpCircle, tokenClass: 'text-state-unknown' },
  cancelled: { label: 'Cancelled', icon: CircleSlash, tokenClass: 'text-state-cancelled' },
  timed_out: { label: 'Timed out', icon: Clock, tokenClass: 'text-state-timed-out' },
  error: { label: 'Error', icon: XCircle, tokenClass: 'text-state-error' },
  unsupported: {
    label: 'Unsupported',
    icon: CircleSlash,
    tokenClass: 'text-state-unsupported-schema',
  },
  unsupported_schema: {
    label: 'Unsupported schema',
    icon: FileQuestion,
    tokenClass: 'text-state-unsupported-schema',
  },
};

export function StateChip({
  kind,
  detail,
  className,
}: {
  kind: DomainStateKind;
  detail?: string;
  className?: string;
}) {
  const s = STATE[kind] ?? {
    label: 'Unsupported schema',
    icon: Ban,
    tokenClass: 'text-state-unsupported-schema',
  };
  const Icon = s.icon;
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-[var(--radius-chip)] border border-edge-subtle',
        'bg-surface-2 px-2 py-0.5 text-2xs font-medium',
        className,
      )}
      data-state={kind}
    >
      {/* State hue rides the icon only; label text stays AA-contrast tokens
       * (state meaning = icon + label + data-state, never color alone). */}
      <Icon aria-hidden size={12} className={cn(s.tokenClass, s.spin && 'animate-spin')} />
      <span className="text-text-secondary">{s.label}</span>
      {detail ? <span className="text-text-muted">· {detail}</span> : null}
    </span>
  );
}
