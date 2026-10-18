/**
 * Vocabulary for the TRACE surface, a selected symbol's call neighbourhood.
 *
 * `model.ts` turns the neighbors wire payload into a `TraceModel`; `plate.ts`
 * lays that model out as the anatomy plate and decides nothing about the
 * data. Every drawn quantity has to be traceable to a field on these records.
 *
 * The one rule that governs every field below: if the wire did not carry it,
 * it is absent here and the surface says so in a caption. Nothing in this file
 * has a plausible default.
 */

/** Which side of the focus a drawn channel lies on. */
export type TraceChannelDirection =
  /** Caller side. */
  | 'up'
  /** Callee side. */
  | 'down'
  /** A lateral move between two members of the same membrane. */
  | 'in'
  /** A channel whose far end is a symbol this frame does not draw. */
  | 'lost';

/** A symbol drawn on the field. */
export interface TraceNode {
  readonly id: string;
  /** Display name, already resolved from the payload's fallback chain. */
  readonly name: string;
  /** Symbol kind, straight off the payload, feeds `kindColor`. */
  readonly kind: string;
  /**
   * Total (in + out) edge count, as the neighbors endpoint reports it in
   * `degree`. `null` when the payload omitted it, an unmeasured degree is
   * never coerced to zero.
   */
  readonly degree: number | null;
  /** `file_path` from the payload, or null when the row carried none. */
  readonly filePath: string | null;
  /** `start_line`, or null. */
  readonly startLine: number | null;
  /**
   * Signed hop ring: negative on the caller side, positive on the callee side,
   * 0 for the focus. This is the hop at which the symbol was FETCHED, which is
   * exactly what the plate column encodes, not elevation, not importance.
   */
  readonly ring: number;
  /**
   * Edges incident on this node that this frame does NOT draw, derived as
   * `degree - drawn incident call sites`. `null` when
   * `degree` is absent, because an unmeasured degree cannot be differenced.
   */
  readonly undrawnEdges: number | null;
  /**
   * Call sites where this symbol calls itself. A self-call is a real `calls`
   * row and is reported, but it is not a channel: it couples no two symbols.
   * Printed on the row instead.
   */
  readonly selfCalls: number;
}

/** A drawn `calls` channel. */
export interface TraceChannel {
  readonly a: string;
  readonly b: string;
  /**
   * Call sites on this one edge: the number of `calls` rows the endpoint
   * returned for this ordered pair. This is the length of its plate bar.
   */
  readonly calls: number;
  readonly dir: TraceChannelDirection;
}

/**
 * A type enclosure derived from `contains` edges in the neighbors payload.
 *
 * Only emitted when the payload actually carried `contains` edges whose
 * container encloses at least two drawn members; otherwise the surface counts
 * none and says so, rather than inventing an enclosure from
 * shared file paths (wire-honesty: do not invent an enclosure).
 */
export interface TraceMembrane {
  readonly id: string;
  /** Container name as the edge row reported it (`source_name`). */
  readonly label: string;
  /** Drawn member ids, in layout order. */
  readonly of: readonly string[];
}

/**
 * Everything the surface knows about what it is NOT showing. Every figure here
 * is counted from rows the endpoint returned; nothing is estimated.
 */
export interface TraceCoverage {
  /** Hops actually fetched. 2 when hop-2 expansion ran, 1 when it did not. */
  readonly hopsFetched: number;
  /** Symbols drawn on the field, focus included. */
  readonly drawn: number;
  /**
   * Distinct symbols that the fetched neighbor lists named but that this frame
   * does not draw. Counted from rows in hand, symbols beyond the fetched hops
   * were never named to us and are deliberately NOT in this number.
   */
  readonly namedButNotDrawn: number;
  /**
   * Hop-1 neighbors whose own neighbors were never fetched, because the
   * expansion budget stopped first. Their further symbols are unknown, not
   * zero.
   */
  readonly unexpandedNeighbors: number;
  /**
   * Node ids whose caller or callee list came back exactly at the endpoint's
   * `limit`, so the list is a prefix and the true count is unknown.
   */
  readonly cappedAt: number | null;
  /** True when at least one fetched list hit `limit`. */
  readonly capped: boolean;
  /**
   * Whether the payload carried `contains` edges at all. When false the
   * surface draws no membranes and the caption states that the wire did not
   * carry them, it does not imply the code has no types.
   */
  readonly membranesAvailable: boolean;
}

/** The complete drawable field: pure data, no DOM, no colour, no clock. */
export interface TraceModel {
  readonly focusId: string;
  readonly nodes: readonly TraceNode[];
  readonly channels: readonly TraceChannel[];
  readonly membranes: readonly TraceMembrane[];
  readonly coverage: TraceCoverage;
}
