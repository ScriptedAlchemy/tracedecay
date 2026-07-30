import type { ReactNode } from 'react';
import { StateChip, type DomainStateKind } from './StateChip';
import { Corners, Readout } from './instrument.tsx';
import type { LegacyResult } from '../data/query/legacy.ts';

/** Renders truthful states around a legacy fetch; children render only on ok. */
export function LegacyBoundary<T>({
  title,
  pending,
  result,
  children,
}: {
  title: string;
  pending: boolean;
  result: LegacyResult<T> | undefined;
  children: (data: T) => ReactNode;
}) {
  if (pending) return <CenteredState title={title} kind="loading" />;
  if (!result) return <CenteredState title={title} kind="unknown" />;
  if (result.outcome === 'ok') return <>{children(result.data)}</>;
  return <CenteredState title={title} kind={failureKind(result)} detail={failureDetail(result)} />;
}

/** The line under the chip: whatever the source said about this state, and
 * nothing where it said nothing.
 *
 * `unavailable` is the state that most needs it. Its chip word is the same for
 * a registry that is missing and one that failed to open, and the payload's
 * `status`/`error` is the only thing that tells them apart. */
function failureDetail(result: Exclude<LegacyResult<unknown>, { outcome: 'ok' }>): string | undefined {
  if (result.outcome === 'error') return result.detail;
  if (result.outcome === 'unavailable') return result.reason ?? result.status;
  return undefined;
}

/** The domain state a non-ok legacy read renders as.
 *
 * Exhaustive over the failure outcomes, so a new one added to `LegacyResult`
 * fails to build here rather than falling into whichever arm a chain of
 * ternaries happened to end on — which is how 401 and 403 spent their whole
 * life rendering as a generic error whose only discriminator was status text. */
function failureKind(result: Exclude<LegacyResult<unknown>, { outcome: 'ok' }>): DomainStateKind {
  switch (result.outcome) {
    case 'offline':
      return 'offline';
    case 'unauthorized':
      return 'unauthorized';
    case 'denied':
      return 'denied';
    case 'error':
      return 'error';
    case 'unsupported_schema':
      return 'unsupported_schema';
    case 'unavailable':
      return 'unavailable';
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

/** Crafted truthful states (plan 11a): one sentence of what this state means
 * here plus the next action, not a bare chip. Workspace-specific sentences
 * come from the caller; these are the designed defaults per state.
 *
 * Partial on purpose. Every state listed has one next action that holds
 * wherever it appears — start the daemon, authenticate, update the build,
 * retry. `unavailable` has none: what to do about a source that cannot serve
 * depends on which source and on the reason it reported, both of which reach
 * the chip as its detail. A generic sentence here would either restate the
 * chip or invent a remedy this surface cannot know, so the state renders as
 * chip plus reported reason and nothing is added. */
const STATE_GUIDANCE: Partial<Record<DomainStateKind, { sentence: string; action: string }>> = {
  loading: { sentence: 'Reading from the daemon.', action: 'This resolves on its own.' },
  offline: {
    sentence: 'The daemon is not reachable from this browser.',
    action: 'Start it with `tracedecay daemon run`, then refresh.',
  },
  error: {
    sentence: 'The read failed and nothing is being invented in its place.',
    action: 'Retry, or check the daemon log if it persists.',
  },
  // Split from `error` because the two refusals need opposite next actions,
  // and neither of them is "retry": no identity was accepted at all, versus an
  // identity that was accepted and is not allowed to see this.
  unauthorized: {
    sentence: 'The daemon accepted no identity for this read, so it refused to answer.',
    action: 'Authenticate to the daemon, then refresh.',
  },
  denied: {
    sentence: 'The daemon knows this identity and does not permit it to read this scope.',
    action: 'Switch to a scope you hold, or grant this one access.',
  },
  unknown: {
    sentence: 'No response has been recorded for this surface yet.',
    action: 'Refresh once the daemon is serving.',
  },
  unsupported_schema: {
    sentence: 'The daemon answered with a shape this build does not understand.',
    action: 'Update the dashboard build to match the daemon.',
  },
};

export function CenteredState({
  title,
  kind,
  detail,
}: {
  title: string;
  kind: DomainStateKind;
  detail?: string | undefined;
}) {
  const guidance = STATE_GUIDANCE[kind];
  // A dead channel on an instrument still shows its ruled field and its bezel:
  // the reader can see the surface is present and simply carrying no signal.
  return (
    <div className="td-graticule flex h-full min-h-48 items-center justify-center bg-surface-0 p-8">
      <div className="relative flex max-w-md flex-col items-center gap-3 border border-edge-subtle bg-surface-1 px-8 py-6 text-center">
        <Corners />
        <h1 className="text-2xs font-semibold uppercase tracking-[0.2em] text-text-primary">
          {title}
        </h1>
        <span aria-hidden className="h-px w-10 bg-edge-strong" />
        <StateChip kind={kind} detail={detail} />
        {guidance ? (
          <p className="max-w-xs text-xs leading-relaxed text-text-muted">
            {guidance.sentence}{' '}
            <span className="text-text-secondary">{guidance.action}</span>
          </p>
        ) : null}
      </div>
    </div>
  );
}

/** A read that failed, said where its reading would have gone.
 *
 * The counterpart to `CenteredState` for a read whose failure is local to one
 * plate: the surrounding surface still has readings to show, so this is a line
 * in place of the missing one rather than a panel over the whole channel.
 *
 * `band` is the page-width form for the rows that sit outside a card, directly
 * under the readout whose figures the failure explains; the default is the
 * inline form a card body carries. The two are a class list rather than a `cn`
 * merge because they disagree on text size, and `cn` is a plain joiner with no
 * conflict resolution. */
export function ReadFailure({
  label,
  detail,
  band = false,
}: {
  label: string;
  detail?: string | null | undefined;
  band?: boolean;
}) {
  return (
    <p
      role="status"
      className={
        band
          ? 'border-b border-state-error/30 bg-state-error/5 px-4 py-2 text-xs text-state-error'
          : 'text-2xs leading-relaxed text-state-error'
      }
    >
      {label}
      {detail ? `: ${detail}` : '.'}
    </p>
  );
}

/** Compact readout tile. Kept as a named export because a dozen workspaces
 * call it; the presentation is now the instrument readout — engraved legend,
 * monospaced tabular value, quiet annotation — inside a hairline cell. */
export function StatTile({
  label,
  value,
  hint,
  dense,
}: {
  label: string;
  value: ReactNode;
  /** Widened from `string` so a tile can annotate its value with the shared
   * evidence-class marker, which `Readout`'s own `note` already accepts. */
  hint?: ReactNode;
  /** Narrow-rail variant: smaller numerals that never clip. */
  dense?: boolean;
}) {
  // A readout is something you look AT, so the tile sits on the raised plane
  // rather than flush with the panel behind it. The values these carry are
  // small counts, not brain-scale magnitudes, so they stay off the display
  // tier -- a repository count set at 34px would be shouting a three.
  return (
    <div className="td-raised min-w-0 border border-edge-subtle px-3 py-2">
      <Readout label={label} value={value} note={hint} size={dense ? 'sm' : 'lg'} />
    </div>
  );
}
