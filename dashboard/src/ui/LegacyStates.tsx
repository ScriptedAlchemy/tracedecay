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
  const kind: DomainStateKind =
    result.outcome === 'offline'
      ? 'offline'
      : result.outcome === 'error'
        ? 'error'
        : 'unsupported_schema';
  const detail = result.outcome === 'error' ? result.detail : undefined;
  return <CenteredState title={title} kind={kind} detail={detail} />;
}

/** Crafted truthful states (plan 11a): one sentence of what this state means
 * here plus the next action, not a bare chip. Workspace-specific sentences
 * come from the caller; these are the designed defaults per state. */
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
