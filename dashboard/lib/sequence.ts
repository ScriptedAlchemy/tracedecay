export interface Sequence {
  /** Start a request: returns a ticket to check on completion. */
  next: () => number;
  /** True while `ticket` is still the latest issued one. */
  isCurrent: (ticket: number) => boolean;
  /** Invalidate every outstanding ticket without starting a request. */
  invalidate: () => void;
}

/**
 * Stale-response guard for overlapping async requests: take a ticket before
 * firing, and drop the response if a newer ticket was issued meanwhile.
 *
 *   const seq = useRef(makeSequence()).current;
 *   const ticket = seq.next();
 *   const data = await fetchThing();
 *   if (!seq.isCurrent(ticket)) return; // a newer request superseded this one
 *
 * Also exposed on the shell SDK (`utils.makeSequence`) for unbundled plugins.
 */
export function makeSequence(): Sequence {
  let current = 0;
  return {
    next: () => ++current,
    isCurrent: (ticket: number) => ticket === current,
    invalidate: () => {
      current += 1;
    },
  };
}
