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
