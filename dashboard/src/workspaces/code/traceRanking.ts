/**
 * The order the accessible list reads the field out in.
 *
 * Hop first, then call sites, then name — the same three quantities the field
 * encodes as row, channel width and label, which is what makes the list the
 * picture read aloud rather than a second arrangement of the same rows. The
 * order is a claim about the data, so it is computed here: pure, DOM-free, and
 * assertable directly instead of scraped back out of rendered markup.
 */
import type { TraceChannel, TraceNode } from '../../viz/trace/types.ts';

/**
 * Call sites incident on each drawn symbol, summed over the channels drawn.
 *
 * Both endpoints of a channel are credited with its `calls`, because a call
 * site is a fact about the edge and both ends of the edge have it. This is
 * therefore the count of call sites the FIELD draws on that symbol, not the
 * symbol's total — that total is `degree`, and the list prints it separately.
 */
export function callSiteTotals(
  channels: readonly TraceChannel[],
): ReadonlyMap<string, number> {
  const totals = new Map<string, number>();
  for (const channel of channels) {
    totals.set(channel.a, (totals.get(channel.a) ?? 0) + channel.calls);
    totals.set(channel.b, (totals.get(channel.b) ?? 0) + channel.calls);
  }
  return totals;
}

/**
 * The drawn symbols in reading order: nearest hop first, then the most-called,
 * then alphabetical.
 *
 * `Math.abs(ring)` because hop distance is the ordering key and the side is not:
 * a one-hop caller and a one-hop callee are equally close to the focus, and
 * sorting the signed ring would put every caller above every callee and read as
 * a claim that upstream matters more. A symbol with no drawn channel counts as
 * zero call sites rather than being dropped — it is on the field, so it is on
 * the list.
 */
export function orderByHopThenCallSites(
  nodes: readonly TraceNode[],
  callSites: ReadonlyMap<string, number>,
): readonly TraceNode[] {
  return [...nodes].sort(
    (a, b) =>
      Math.abs(a.ring) - Math.abs(b.ring) ||
      (callSites.get(b.id) ?? 0) - (callSites.get(a.id) ?? 0) ||
      a.name.localeCompare(b.name),
  );
}
