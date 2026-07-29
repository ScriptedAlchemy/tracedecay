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
  Unplug,
  XCircle,
} from 'lucide-react';
import { cn } from './cn';

/** The eighteen-state domain taxonomy (plan 11). Token + icon + label —
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
  /** A reachable authority said this one source cannot answer. The near
   * neighbour of `offline`, where nothing was reached at all, and paired with
   * it the way `unsupported` is paired with `unsupported_schema`: one shared
   * hue, told apart by icon and label so a source-level refusal never reads as
   * a dashboard that lost its connection. */
  | 'unavailable'
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
  unavailable: {
    label: 'Source unavailable',
    icon: Unplug,
    tokenClass: 'text-state-offline',
  },
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

/** The lamp bar down the chip's leading edge. Spelled out per state (rather
 * than derived from `tokenClass`) because Tailwind resolves utilities by
 * scanning literal source text — a computed class name would never be built. */
const LAMP: Record<DomainStateKind, string> = {
  loading: 'bg-state-loading',
  complete_zero_findings: 'bg-state-complete-zero',
  ready: 'bg-state-ready',
  partial: 'bg-state-partial',
  stale: 'bg-state-stale',
  locked: 'bg-state-locked',
  denied: 'bg-state-denied',
  unauthorized: 'bg-state-unauthorized',
  redacted: 'bg-state-redacted',
  conflicting: 'bg-state-conflicting',
  unavailable: 'bg-state-offline',
  offline: 'bg-state-offline',
  unknown: 'bg-state-unknown',
  cancelled: 'bg-state-cancelled',
  timed_out: 'bg-state-timed-out',
  error: 'bg-state-error',
  unsupported: 'bg-state-unsupported-schema',
  unsupported_schema: 'bg-state-unsupported-schema',
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
  const lampClass = LAMP[kind] ?? 'bg-state-unsupported-schema';
  const Icon = s.icon;
  return (
    <span
      className={cn(
        // An indicator segment, not a pill: square, hairline-bezelled, with the
        // state hue carried by a lamp bar down its leading edge so the chip
        // reads at a glance across a dense panel.
        //
        // flex-wrap: when the detail text does not fit next to the icon +
        // label on one row, the whole detail span drops to its own line
        // (nearly the chip's full width) instead of every sibling staying
        // pinned to one nowrap row and squeezing the detail text into
        // whatever sliver is left — that sliver could be ~30px in a narrow
        // rail, which wrapped the detail text one word per line.
        'relative inline-flex flex-wrap items-center gap-x-1.5 gap-y-0.5 border border-edge-subtle bg-surface-2',
        'py-[3px] pl-2.5 pr-2 text-3xs font-medium',
        // Wrap as a block, not as a column. Without this the detail text kept
        // its narrow slot beside the label in a constrained rail and broke one
        // word per line into a four-line ribbon; wrapping lets it drop to its
        // own full-width line under the label instead.
        'max-w-full flex-wrap',
        className,
      )}
      data-state={kind}
    >
      <span
        aria-hidden
        className={cn('absolute inset-y-0 left-0 w-[2px]', lampClass)}
      />
      {/* State hue rides the lamp and icon; label text stays AA-contrast tokens
       * (state meaning = icon + label + data-state, never color alone). */}
      <Icon aria-hidden size={11} className={cn(s.tokenClass, s.spin && 'animate-spin')} />
      <span className="uppercase tracking-[0.1em] text-text-secondary">{s.label}</span>
      {detail ? (
        <span className="min-w-0 tracking-[0.02em] text-text-muted">· {detail}</span>
      ) : null}
    </span>
  );
}
