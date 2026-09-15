import type { ReactNode } from 'react';
import { Readout } from './instrument.tsx';

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
