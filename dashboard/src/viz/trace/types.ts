/**
 * Vocabulary for the TRACE surface — the drill-in that floods a selected
 * symbol's call topography (plan 11b, "Topography round one — coordinator
 * verdict").
 *
 * Three modules share these types and nothing else: `model.ts` turns the
 * neighbors wire payload into a `TraceModel` (measurement → layout), `sim.ts`
 * turns that model into forces (measurement → sensation), and `render.ts`
 * draws whatever the other two produced and decides nothing. The split is the
 * honesty boundary named in the plan's "Rendering strategy": every felt or
 * drawn quantity has to be traceable to a field on one of these records.
 *
 * The one rule that governs every field below: if the wire did not carry it,
 * it is absent here and the surface says so in a caption. Nothing in this file
 * has a plausible default.
 */

/** Which side of the focus a drawn channel lies on. Drawing direction only —
 * the simulation treats all four as the same undirected spring. */
export type TraceChannelDirection =
  /** Caller side: a tributary flowing into the focus. */
  | 'up'
  /** Callee side: the delta fanning out of it. */
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
  /** Symbol kind, straight off the payload — feeds `kindColor`. */
  readonly kind: string;
  /**
   * Total (in + out) edge count, as the neighbors endpoint reports it in
   * `degree`. This is the node's MASS in the simulation and the width of its
   * sill in the renderer. `null` when the payload omitted it — an unmeasured
   * degree is never coerced to zero.
   */
  readonly degree: number | null;
  /** `file_path` from the payload, or null when the row carried none. */
  readonly filePath: string | null;
  /** `start_line`, or null. */
  readonly startLine: number | null;
  /**
   * Signed hop ring: negative on the caller side, positive on the callee side,
   * 0 for the focus. This is the hop at which the symbol was FETCHED, which is
   * exactly what the row position encodes — not elevation, not importance.
   */
  readonly ring: number;
  /** Layout anchor in world coordinates. The simulation holds the node here. */
  readonly x0: number;
  readonly y0: number;
  /**
   * Edges incident on this node that this frame does NOT draw, derived as
   * `degree - drawn incident channels`. Drawn as a dashed mouth. `null` when
   * `degree` is absent, because an unmeasured degree cannot be differenced.
   */
  readonly undrawnEdges: number | null;
  /**
   * Call sites where this symbol calls itself. A self-call is a real `calls`
   * row and is reported, but it is not a channel: it couples no two bodies, so
   * it carries no spring and no ribbon. Printed on the node instead.
   */
  readonly selfCalls: number;
}

/** A drawn `calls` channel. */
export interface TraceChannel {
  readonly a: string;
  readonly b: string;
  /**
   * Call sites on this one edge: the number of `calls` rows the endpoint
   * returned for this ordered pair. This is the channel's WIDTH and its spring
   * STIFFNESS — the felt channel and the drawn channel are the same number.
   */
  readonly calls: number;
  readonly dir: TraceChannelDirection;
}

/**
 * A type enclosure derived from `contains` edges in the neighbors payload.
 *
 * Only emitted when the payload actually carried `contains` edges whose
 * container encloses at least two drawn members; otherwise the surface omits
 * membranes entirely and says so, rather than inventing an enclosure from
 * shared file paths (plan 11b wire-honesty rule).
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
   * does not draw. Counted from rows in hand — symbols beyond the fetched hops
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
   * carry them — it does not imply the code has no types.
   */
  readonly membranesAvailable: boolean;
  /**
   * Every distinct field name observed on the neighbour rows this model was
   * built from, sorted.
   *
   * Recorded because the sensory contract has five channels and this route
   * serves the measurement behind two of them. Which two is a property of the
   * PAYLOAD, not of a capability list someone typed here: the schemas are
   * passthrough, so the day a producer starts sending a complexity or churn
   * field, that field appears in this array and the corresponding channel goes
   * live without a copy edit. Understating what the wire carries would be as
   * false as overstating it, so neither is asserted — both are read.
   */
  readonly rowFields: readonly string[];
}

/** Whether a sensory channel can be driven by the payload actually in hand. */
export type SensoryChannelState =
  /** The measurement arrived on this payload; the channel is live. */
  | 'measured'
  /**
   * No field on this payload carries the measurement. Absence of a field is
   * not absence of the property — this says the wire was silent, nothing more.
   */
  | 'not-on-this-wire'
  /**
   * The measurement exists but only at a coarser scope than this field draws,
   * so binding it to a symbol here would be a fabricated join.
   */
  | 'coarser-scope';

/**
 * One channel of the app-wide sensory contract as it stands on THIS field.
 *
 * The contract is "sensation encodes a stated measurement". A channel with no
 * measurement behind it must therefore be inert AND said out loud, because a
 * surface that quietly animates four channels and drives two is claiming two
 * measurements it does not have.
 */
export interface SensoryChannel {
  /** The felt quantity, in the contract's own words. */
  readonly feel: string;
  /** The measurement it is bound to, in the contract's own words. */
  readonly measurement: string;
  readonly state: SensoryChannelState;
  /** How this channel reads when motion is reduced. */
  readonly staticEquivalent: string;
  /**
   * The mechanism when measured, or why it is inert when not. Never empty:
   * an unexplained inert channel is indistinguishable from a broken one.
   */
  readonly note: string;
}

/** The complete drawable field: pure data, no DOM, no colour, no clock. */
export interface TraceModel {
  readonly focusId: string;
  readonly world: { readonly width: number; readonly height: number };
  /** Ring → world y. Keys are the signed rings present in `nodes`. */
  readonly rows: ReadonlyMap<number, number>;
  readonly nodes: readonly TraceNode[];
  readonly channels: readonly TraceChannel[];
  readonly membranes: readonly TraceMembrane[];
  readonly coverage: TraceCoverage;
}

/**
 * Resolved theme tokens. Canvas2D cannot read CSS custom properties, so the
 * composing component samples `tokens.css` once per theme flip and hands the
 * resolved strings in. That keeps the stylesheet the single source of the
 * instrument's colour without a `getComputedStyle` call inside the draw loop.
 */
export interface TracePalette {
  readonly surface0: string;
  readonly surface1: string;
  readonly textPrimary: string;
  readonly textMuted: string;
  readonly edgeSubtle: string;
  readonly edgeStrong: string;
  readonly grid: string;
  readonly accent: string;
  readonly upstream: string;
  readonly downstream: string;
  readonly statePartial: string;
  readonly stateUnknown: string;
  readonly membraneFill: string;
  /** Whether the field is drawn against a light medium. */
  readonly light: boolean;
}

/** One painted frame's worth of state. Everything here comes from the sim. */
export interface TraceFrame {
  /** Flat `[x0,y0,x1,y1,…]` in `model.nodes` order. */
  readonly positions: Float64Array;
  /** Signed px off rest length, in `model.channels` order. */
  readonly stretches: Float64Array;
  /** Hover bloom per node in `[0,1]`, in `model.nodes` order. */
  readonly bloom: Float64Array;
  readonly draggingId: string | null;
  readonly hoveredId: string | null;
  /**
   * Static-equivalent mode: the same settled positions, with tension drawn as
   * thickness instead of as motion. A rendering mode, not a degradation.
   */
  readonly reducedMotion: boolean;
}
