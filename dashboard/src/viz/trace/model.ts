/**
 * Measurement → layout for the TRACE surface.
 *
 * This module turns what `GET /api/plugins/graph/node/{id}/neighbors` actually
 * returns into a `TraceModel`. It is pure, DOM-free and deterministic, and it
 * is where every honesty decision about the picture is made and recorded, so a
 * test can hold the caption to the data.
 *
 * What that endpoint carries (src/dashboard/graph_service.rs
 * `neighbors_payload`, src/dashboard/graph_queries.rs `caller_rows` /
 * `callee_rows` / `neighborhood_edge_rows`), and what follows from it:
 *
 * - `callers` / `callees` are `calls` edges ONLY, one ROW PER EDGE. A caller
 *   with three call sites appears three times with different `edge_line`, so
 *   the call-site count of a pair is the number of its rows. That count is the
 *   channel's width AND its spring stiffness — the drawn channel and the felt
 *   channel are the same number by construction.
 * - `degree` is the node's total (in + out) edge count over ALL edge kinds.
 *   Subtracting the call sites this frame draws gives the edges it does not,
 *   which is what a dashed mouth reports.
 * - `edges` carries every edge kind incident on the focus, including
 *   `contains`. Membranes are derived from those rows and from nothing else —
 *   no shared-file-path guessing. When the payload carries no `contains` rows,
 *   `coverage.membranesAvailable` is false, the field draws no enclosures, and
 *   the caption says the wire did not carry them.
 * - Both lists are truncated at `limit` (max 200). A list that comes back
 *   exactly at `limit` is a prefix, and `coverage.capped` records it.
 *
 * Depth: the caller fetches hop 1 for the focus and then hop 1 for as many of
 * the drawn hop-1 neighbours as the budget allows. There is no server-side
 * depth-2 query, so hop 2 is assembled here — bounded, deduped, and counted.
 */
import type {
  SensoryChannel,
  TraceChannel,
  TraceChannelDirection,
  TraceCoverage,
  TraceMembrane,
  TraceModel,
  TraceNode,
} from './types.ts';
import type { SimSpec } from './sim.ts';

/**
 * The world layout anchors live in.
 *
 * Narrower than the static sheet's 1440x1160, and deliberately: the drill-in
 * occupies the workspace's list column, not a full-bleed page, so a 1440-wide
 * world was being scaled to roughly half size and every label with it. Sizing
 * the world near the column's real width keeps the scale factor close to 1,
 * which is what makes the type legible without the renderer having to fight its
 * own transform.
 */
export const TRACE_WORLD = Object.freeze({ width: 1200, height: 1040 });

/** Vertical band the hop rings are spread across. */
const ROW_TOP = 128;
const ROW_BOTTOM = 940;
/** Horizontal band nodes are placed in, leaving room for ring labels. */
const COL_LEFT = 172;
const COL_RIGHT = 1092;

/**
 * Drawing budget. The plan caps a readable subgraph at 80–250 nodes; this
 * surface sits far below that on purpose, because every node here carries a
 * name and a degree in type and a row of ten labels collides at any width the
 * workspace actually offers. Whatever these rules exclude is COUNTED, never
 * silently dropped.
 */
export const TRACE_BUDGET = Object.freeze({
  /** Drawn hop-1 symbols per side, ranked by call sites on their channel. */
  hop1PerSide: 7,
  /** Drawn hop-2 symbols per side, same ranking. */
  hop2PerSide: 9,
  /** Hop-1 neighbours whose own neighbours are fetched to build hop 2. */
  expand: 12,
});

/* ---- wire shapes -------------------------------------------------------- */

/** One `nodes` row as the neighbors endpoint serves it. */
export interface NeighborRow {
  id?: string | null;
  kind?: string | null;
  name?: string | null;
  qualified_name?: string | null;
  file_path?: string | null;
  start_line?: number | null;
  degree?: number | null;
  edge_line?: number | null;
  [key: string]: unknown;
}

/** One `edges` row (`neighborhood_edge_rows`). */
export interface NeighborEdgeRow {
  source?: string | null;
  target?: string | null;
  kind?: string | null;
  source_name?: string | null;
  target_name?: string | null;
  [key: string]: unknown;
}

export interface NeighborsPayload {
  node_id?: string | null;
  depth?: number | null;
  limit?: number | null;
  callers?: NeighborRow[] | null;
  callees?: NeighborRow[] | null;
  edges?: NeighborEdgeRow[] | null;
  edges_by_kind?: Array<{ kind?: string | null; count?: number | null }> | null;
}

/** The focus symbol, as the Code workspace already holds it. */
export interface TraceFocus {
  id: string;
  kind?: string | null;
  name?: string | null;
  qualified_name?: string | null;
  file_path?: string | null;
  start_line?: number | null;
  degree?: number | null;
}

export interface TraceModelInput {
  readonly focus: TraceFocus;
  /** The focus's own neighbors payload. */
  readonly root: NeighborsPayload;
  /** Hop-1 neighbours that were expanded, keyed by node id. */
  readonly expanded: ReadonlyMap<string, NeighborsPayload>;
}

/* ---- helpers ------------------------------------------------------------ */

function rowName(row: { name?: string | null; qualified_name?: string | null; id?: string | null }): string {
  return row.name ?? row.qualified_name ?? row.id ?? '—';
}

function finite(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function rows(list: NeighborRow[] | null | undefined): NeighborRow[] {
  return (list ?? []).filter((row): row is NeighborRow => !!row && typeof row.id === 'string');
}

/**
 * Distinct ids in a row list, with the number of rows each appeared in — which
 * is that pair's call-site count, because the endpoint emits one row per edge.
 */
function callSites(list: NeighborRow[]): Map<string, { row: NeighborRow; calls: number }> {
  const out = new Map<string, { row: NeighborRow; calls: number }>();
  for (const row of list) {
    const id = row.id as string;
    const seen = out.get(id);
    if (seen) seen.calls += 1;
    else out.set(id, { row, calls: 1 });
  }
  return out;
}

/**
 * Order a ranked list so the highest-ranked member sits nearest the centre of
 * its row. A row that simply reads left-to-right by strength puts the strongest
 * channel at the far edge, where its ribbon has to cross the whole field.
 */
function centreOut<T>(items: readonly T[]): T[] {
  const out: T[] = [];
  items.forEach((item, i) => {
    if (i % 2 === 0) out.push(item);
    else out.unshift(item);
  });
  return out;
}

/* ---- the build ---------------------------------------------------------- */

interface Draft {
  id: string;
  name: string;
  kind: string;
  degree: number | null;
  filePath: string | null;
  startLine: number | null;
  ring: number;
}

/**
 * Build the drawable field.
 *
 * Every exclusion this makes lands in `coverage`, and every number in
 * `coverage` is counted from rows in hand. Nothing is estimated, and symbols
 * beyond the fetched hops are never counted — they were never named to us.
 */
export function buildTraceModel(input: TraceModelInput): TraceModel {
  const { focus, root, expanded } = input;

  const drafts = new Map<string, Draft>();
  drafts.set(focus.id, {
    id: focus.id,
    name: rowName(focus),
    kind: focus.kind ?? 'unknown',
    degree: finite(focus.degree),
    filePath: focus.file_path ?? null,
    startLine: finite(focus.start_line),
    ring: 0,
  });

  /** Ordered pair "a→b" → call sites. Collected over every fetched payload. */
  const pairCalls = new Map<string, { from: string; to: string; calls: number }>();
  /** Every symbol any fetched list named, drawn or not. */
  const named = new Set<string>([focus.id]);
  /**
   * Field names actually observed on neighbour rows. Read, never assumed —
   * this is what decides which sensory channels this field may drive.
   */
  const rowFields = new Set<string>();

  function recordPair(from: string, to: string, calls: number): void {
    // NUL separator, written as an escape rather than as a raw byte: a literal
    // control character in the source made git classify this file as binary
    // (no diffs, no blame) and made every grep-class tool skip it silently.
    const key = `${from}\0${to}`;
    const seen = pairCalls.get(key);
    // The same ordered pair is reported by both endpoints of the fetch (as a
    // callee of one and a caller of the other) with identical row counts. Take
    // the maximum rather than the sum, or a channel drawn from two directions
    // would report twice its call sites.
    if (seen) seen.calls = Math.max(seen.calls, calls);
    else pairCalls.set(key, { from, to, calls });
  }

  function absorb(payload: NeighborsPayload, ownerId: string): void {
    for (const row of [...rows(payload.callers), ...rows(payload.callees)]) {
      for (const key of Object.keys(row)) rowFields.add(key);
    }
    for (const [id, entry] of callSites(rows(payload.callers))) {
      named.add(id);
      recordPair(id, ownerId, entry.calls);
    }
    for (const [id, entry] of callSites(rows(payload.callees))) {
      named.add(id);
      recordPair(ownerId, id, entry.calls);
    }
  }

  /** Rank a payload's neighbours by call sites, strongest first. */
  function ranked(
    payload: NeighborsPayload,
    side: 'callers' | 'callees',
  ): Array<{ id: string; row: NeighborRow; calls: number }> {
    return [...callSites(rows(payload[side]))]
      .map(([id, entry]) => ({ id, row: entry.row, calls: entry.calls }))
      .sort((a, b) => b.calls - a.calls || a.id.localeCompare(b.id));
  }

  function admit(candidate: { id: string; row: NeighborRow }, ring: number): boolean {
    if (drafts.has(candidate.id)) return false;
    drafts.set(candidate.id, {
      id: candidate.id,
      name: rowName(candidate.row),
      kind: candidate.row.kind ?? 'unknown',
      degree: finite(candidate.row.degree),
      filePath: candidate.row.file_path ?? null,
      startLine: finite(candidate.row.start_line),
      ring,
    });
    return true;
  }

  absorb(root, focus.id);

  // Hop 1. Callers go up, callees go down; strongest channels first, and a
  // symbol that is both a caller and a callee is drawn once, on the side that
  // reached it first (callers), which the caption states.
  const upSeeds: string[] = [];
  const downSeeds: string[] = [];
  for (const candidate of ranked(root, 'callers')) {
    if (upSeeds.length >= TRACE_BUDGET.hop1PerSide) break;
    if (admit(candidate, -1)) upSeeds.push(candidate.id);
  }
  for (const candidate of ranked(root, 'callees')) {
    if (downSeeds.length >= TRACE_BUDGET.hop1PerSide) break;
    if (admit(candidate, 1)) downSeeds.push(candidate.id);
  }

  // Hop 2, assembled client-side from the expanded neighbours' own payloads.
  // A symbol first reached through an upstream neighbour is drawn upstream,
  // whichever arm of that neighbour named it — the row is hop DISTANCE, and
  // the side is which arm of the search got there first.
  let expandedCount = 0;
  let upTwo = 0;
  let downTwo = 0;
  for (const seed of [...upSeeds, ...downSeeds]) {
    const payload = expanded.get(seed);
    if (!payload) continue;
    expandedCount += 1;
    absorb(payload, seed);
    const upstream = upSeeds.includes(seed);
    const ring = upstream ? -2 : 2;
    const budget = TRACE_BUDGET.hop2PerSide;
    for (const candidate of [...ranked(payload, 'callers'), ...ranked(payload, 'callees')].sort(
      (a, b) => b.calls - a.calls || a.id.localeCompare(b.id),
    )) {
      if ((upstream ? upTwo : downTwo) >= budget) break;
      if (!admit(candidate, ring)) continue;
      if (upstream) upTwo += 1;
      else downTwo += 1;
    }
  }

  /* ---- channels: only pairs whose BOTH ends are drawn ------------------- */
  const drawnChannels: TraceChannel[] = [];
  const callSitesOn = new Map<string, number>();
  const selfCallsOn = new Map<string, number>();
  for (const { from, to, calls } of pairCalls.values()) {
    const a = drafts.get(from);
    const b = drafts.get(to);
    if (!a || !b) continue;
    if (from === to) {
      // Recursion. A real `calls` row, and a self-loop spring is undefined
      // (zero length, no second body), so it is counted on the node and
      // printed there rather than drawn as a channel or quietly discarded.
      selfCallsOn.set(from, (selfCallsOn.get(from) ?? 0) + calls);
      callSitesOn.set(from, (callSitesOn.get(from) ?? 0) + calls);
      continue;
    }
    drawnChannels.push({ a: from, b: to, calls, dir: directionOf(a.ring, b.ring) });
    callSitesOn.set(from, (callSitesOn.get(from) ?? 0) + calls);
    callSitesOn.set(to, (callSitesOn.get(to) ?? 0) + calls);
  }
  drawnChannels.sort((x, y) => x.a.localeCompare(y.a) || x.b.localeCompare(y.b));

  /* ---- membranes: `contains` rows, or nothing --------------------------- */
  const containsRows: NeighborEdgeRow[] = [];
  let containsSeen = 0;
  for (const payload of [root, ...expanded.values()]) {
    for (const entry of payload.edges_by_kind ?? []) {
      if (entry?.kind === 'contains') containsSeen += finite(entry.count) ?? 0;
    }
    for (const edge of payload.edges ?? []) {
      if (edge?.kind === 'contains') containsRows.push(edge);
    }
  }
  const byContainer = new Map<string, { label: string; of: string[] }>();
  for (const edge of containsRows) {
    const container = edge.source;
    const member = edge.target;
    if (typeof container !== 'string' || typeof member !== 'string') continue;
    if (!drafts.has(member)) continue;
    const entry = byContainer.get(container);
    const label = edge.source_name ?? container;
    if (entry) {
      if (!entry.of.includes(member)) entry.of.push(member);
    } else {
      byContainer.set(container, { label, of: [member] });
    }
  }
  const membranes: TraceMembrane[] = [...byContainer]
    // A one-member enclosure is a true `contains` edge but not an enclosure a
    // reader can see the flow enter and leave, so it is not drawn as one. It
    // remains a counted `contains` edge on its member's mouth.
    .filter(([, entry]) => entry.of.length >= 2)
    .map(([id, entry]) => ({ id, label: entry.label, of: entry.of }))
    .sort((a, b) => a.id.localeCompare(b.id));

  const membraneOf = new Map<string, string>();
  for (const membrane of membranes) {
    for (const member of membrane.of) membraneOf.set(member, membrane.id);
  }

  /* ---- layout ----------------------------------------------------------- */
  const ringsPresent = [...new Set([...drafts.values()].map((d) => d.ring))].sort((a, b) => a - b);
  const rowY = new Map<number, number>();
  ringsPresent.forEach((ring, i) => {
    const span = ringsPresent.length > 1 ? (ROW_BOTTOM - ROW_TOP) / (ringsPresent.length - 1) : 0;
    rowY.set(ring, ROW_TOP + span * i);
  });

  // Channel adjacency, weighted by call sites, for the barycentre pass below.
  const neighboursOf = new Map<string, Array<{ other: string; calls: number }>>();
  for (const channel of drawnChannels) {
    if (!neighboursOf.has(channel.a)) neighboursOf.set(channel.a, []);
    if (!neighboursOf.has(channel.b)) neighboursOf.set(channel.b, []);
    neighboursOf.get(channel.a)!.push({ other: channel.b, calls: channel.calls });
    neighboursOf.get(channel.b)!.push({ other: channel.a, calls: channel.calls });
  }

  const nodes: TraceNode[] = [];
  const placedX = new Map<string, number>();
  const centre = (COL_LEFT + COL_RIGHT) / 2;

  /**
   * Rings are laid out from the focus outward, and each ring is ordered by the
   * call-site-weighted mean x of the neighbours already placed on the ring
   * inside it — a one-pass barycentre ordering.
   *
   * Without it, ordering a ring by raw strength puts a symbol nowhere near the
   * symbols it actually calls, and every channel has to cross the field to
   * reach its partner. The resulting picture is a legible watershed instead of
   * a hairball, and no measurement is touched: barycentre decides only which of
   * several equally-valid x slots a node occupies within the row its HOP
   * DISTANCE already assigned it.
   */
  const byDistance = [...ringsPresent].sort((a, b) => Math.abs(a) - Math.abs(b) || a - b);
  for (const ring of byDistance) {
    const inRing = [...drafts.values()].filter((d) => d.ring === ring);
    const keyOf = (id: string): number => {
      let weight = 0;
      let sum = 0;
      for (const { other, calls } of neighboursOf.get(id) ?? []) {
        const x = placedX.get(other);
        if (x === undefined) continue;
        weight += calls;
        sum += x * calls;
      }
      return weight > 0 ? sum / weight : centre;
    };
    const keys = new Map(inRing.map((draft) => [draft.id, keyOf(draft.id)]));
    // Membrane siblings share their group's mean key, so a type's members stay
    // adjacent and its enclosure is a compact box rather than a band spanning
    // the whole field.
    for (const membrane of membranes) {
      const members = membrane.of.filter((id) => keys.has(id));
      if (members.length < 2) continue;
      const mean = members.reduce((sum, id) => sum + keys.get(id)!, 0) / members.length;
      for (const id of members) keys.set(id, mean);
    }
    const ordered = inRing.sort(
      (a, b) =>
        keys.get(a.id)! - keys.get(b.id)! ||
        (callSitesOn.get(b.id) ?? 0) - (callSitesOn.get(a.id) ?? 0) ||
        a.id.localeCompare(b.id),
    );
    // The focus is the one node whose slot is not negotiable: it is the basin
    // the whole field drains toward, so it sits dead centre.
    const placed = ring === 0 ? centreOut(ordered) : ordered;
    const y = rowY.get(ring)!;
    placed.forEach((draft, i) => {
      const span = placed.length > 1 ? (COL_RIGHT - COL_LEFT) / (placed.length - 1) : 0;
      const x = placed.length > 1 ? COL_LEFT + span * i : centre;
      const finalX = draft.id === focus.id ? centre : x;
      placedX.set(draft.id, finalX);
      const drawn = callSitesOn.get(draft.id) ?? 0;
      nodes.push({
        id: draft.id,
        name: draft.name,
        kind: draft.kind,
        degree: draft.degree,
        filePath: draft.filePath,
        startLine: draft.startLine,
        ring: draft.ring,
        x0: finalX,
        y0: y,
        undrawnEdges: draft.degree == null ? null : Math.max(0, draft.degree - drawn),
        selfCalls: selfCallsOn.get(draft.id) ?? 0,
      });
    });
  }

  /* ---- channel direction refinement: same membrane is a lateral move ---- */
  const channels: TraceChannel[] = drawnChannels.map((channel) => {
    const home = membraneOf.get(channel.a);
    if (home && home === membraneOf.get(channel.b)) return { ...channel, dir: 'in' as const };
    return channel;
  });

  /* ---- coverage --------------------------------------------------------- */
  const limit = finite(root.limit);
  let capped = false;
  for (const payload of [root, ...expanded.values()]) {
    const cap = finite(payload.limit);
    if (cap == null) continue;
    if (rows(payload.callers).length >= cap || rows(payload.callees).length >= cap) capped = true;
  }
  let namedButNotDrawn = 0;
  for (const id of named) if (!drafts.has(id)) namedButNotDrawn += 1;

  const drawnHop1 = upSeeds.length + downSeeds.length;
  const coverage: TraceCoverage = {
    hopsFetched: expandedCount > 0 ? 2 : 1,
    drawn: nodes.length,
    namedButNotDrawn,
    unexpandedNeighbors: Math.max(0, drawnHop1 - expandedCount),
    cappedAt: limit,
    capped,
    membranesAvailable: containsSeen > 0 || containsRows.length > 0,
    rowFields: [...rowFields].sort(),
  };

  return {
    focusId: focus.id,
    world: TRACE_WORLD,
    rows: rowY,
    nodes,
    channels,
    membranes,
    coverage,
  };
}

/**
 * Drawing direction from the two rings. Equal rings are a lateral move; a pair
 * straddling the focus can only be reached through it, so the side of the
 * outer endpoint decides.
 */
function directionOf(ringA: number, ringB: number): TraceChannelDirection {
  if (ringA === ringB) return 'in';
  const outer = Math.abs(ringA) >= Math.abs(ringB) ? ringA : ringB;
  return outer < 0 ? 'up' : 'down';
}

/**
 * Field names that would carry each unbound sensory measurement.
 *
 * These are candidate names, matched against `coverage.rowFields` — the fields
 * the payload actually delivered. The point of matching rather than asserting is
 * that a producer which starts serving one of these makes the channel go live
 * on its own; nothing here has to be re-edited, and the surface cannot end up
 * understating coverage it has been given. Matching is on the field's presence,
 * not on a host or provider name, because a capability is a property of the
 * response and not of who produced it.
 */
export const SENSORY_FIELD_CANDIDATES = Object.freeze({
  /** Cyclomatic complexity, for the texture/grain channel. */
  complexity: Object.freeze([
    'complexity',
    'cyclomatic',
    'cyclomatic_complexity',
  ] as const),
  /** Churn recency, for the warmth channel. */
  churn: Object.freeze([
    'churn',
    'churn_recency',
    'last_modified',
    'last_modified_at',
    'last_commit_at',
  ] as const),
  /** Symbol- or path-scoped live activity, for the pulse channel. */
  activity: Object.freeze([
    'activity',
    'last_strike_at',
    'activity_path',
  ] as const),
});

function served(coverage: TraceCoverage, candidates: readonly string[]): string | null {
  return candidates.find((name) => coverage.rowFields.includes(name)) ?? null;
}

/**
 * The five sensory channels, each resolved against the payload in hand.
 *
 * The sensory contract is app-wide and fixed — weight is always connectedness,
 * tension is always coupling — but which channels a given field can actually
 * DRIVE depends on what arrived. This returns all five either way, so the
 * surface can show a channel as inert instead of omitting it, and a reader
 * learns the same mapping everywhere even where a measurement is missing.
 */
export function sensoryChannels(model: TraceModel): readonly SensoryChannel[] {
  const c = model.coverage;
  const anyDegree = model.nodes.some((node) => node.degree != null);
  const callSiteTotal = model.channels.reduce((sum, channel) => sum + channel.calls, 0);
  const complexityField = served(c, SENSORY_FIELD_CANDIDATES.complexity);
  const churnField = served(c, SENSORY_FIELD_CANDIDATES.churn);
  const activityField = served(c, SENSORY_FIELD_CANDIDATES.activity);

  return [
    {
      feel: 'weight / inertia',
      measurement: 'connectedness (degree)',
      state: anyDegree ? 'measured' : 'not-on-this-wire',
      staticEquivalent: 'sill width',
      note: anyDegree
        ? 'degree sets each body’s mass, so hover latency, bloom depth and settle time all scale with it'
        : 'no row on this payload carried a degree, so every body is at the mass floor and weight reads nothing',
    },
    {
      feel: 'tension / deformation',
      measurement: 'coupling strength (call sites on one edge)',
      state: callSiteTotal > 0 ? 'measured' : 'not-on-this-wire',
      staticEquivalent: 'channel thickness',
      note:
        callSiteTotal > 0
          ? `each channel is a spring stiffened by its own call-site count (${callSiteTotal} across ${model.channels.length} channels), so dragging deforms the neighbourhood in proportion to coupling`
          : 'no calls rows arrived, so no channel carries a spring',
    },
    {
      feel: 'texture / grain',
      measurement: 'cyclomatic complexity',
      state: complexityField ? 'measured' : 'not-on-this-wire',
      staticEquivalent: 'contour tightness',
      note: complexityField
        ? `driven by the payload’s ${complexityField} field`
        : 'this route’s rows carry no complexity field, so the channel is inert — the symbols are not being claimed to be simple',
    },
    {
      feel: 'warmth',
      measurement: 'churn recency',
      state: churnField ? 'measured' : 'not-on-this-wire',
      staticEquivalent: 'heat tint held at its current value',
      note: churnField
        ? `driven by the payload’s ${churnField} field`
        : 'this route’s rows carry no churn or last-modified field, so nothing is tinted — untinted here means unmeasured, not cold',
    },
    {
      feel: 'pulse',
      measurement: 'live activity',
      state: activityField ? 'measured' : 'coarser-scope',
      staticEquivalent: 'pinned-lit',
      note: activityField
        ? `driven by the payload’s ${activityField} field`
        : 'the live activity stream is project-scoped and carries no path, so no strike can be attributed to a symbol on this field',
    },
  ];
}

/**
 * Everything the field is NOT showing, counted from rows in hand.
 *
 * Every clause is conditional on a measured figure, so the caption shortens
 * when there is genuinely nothing to disclose rather than reciting boilerplate.
 */
export function coverageCaption(model: TraceModel): string {
  const c = model.coverage;
  const parts: string[] = [
    `${c.hopsFetched} ${c.hopsFetched === 1 ? 'hop' : 'hops'} · ${c.drawn} symbols drawn`,
    `${c.namedButNotDrawn} further ${c.namedButNotDrawn === 1 ? 'symbol' : 'symbols'} not drawn`,
  ];
  if (c.unexpandedNeighbors > 0) {
    parts.push(
      `${c.unexpandedNeighbors} ${c.unexpandedNeighbors === 1 ? 'neighbour was' : 'neighbours were'} not expanded, so their own callers and callees are unknown, not zero`,
    );
  }
  if (c.hopsFetched === 1) {
    parts.push('nothing beyond hop 1 was fetched, so nothing beyond it is counted');
  } else {
    parts.push('symbols beyond hop 2 were never named to this view and are not in these counts');
  }
  if (c.capped) {
    parts.push(
      `at least one list returned exactly ${c.cappedAt ?? 'the'} rows, the endpoint limit, so it is a prefix and the true count is unknown`,
    );
  }
  parts.push(
    c.membranesAvailable
      ? `${model.membranes.length} type ${model.membranes.length === 1 ? 'membrane' : 'membranes'} from contains edges`
      : 'the payload carried no contains edges, so no type membranes are drawn — this says nothing about whether these symbols have types',
  );
  return parts.join(' · ');
}

/** The `role="img"` description. Says what is drawn and what is left out. */
export function fieldDescription(model: TraceModel): string {
  const focus = model.nodes.find((node) => node.id === model.focusId);
  const up = model.nodes.filter((node) => node.ring < 0).length;
  const down = model.nodes.filter((node) => node.ring > 0).length;
  const callSites = model.channels.reduce((sum, channel) => sum + channel.calls, 0);
  return (
    `Call topography of ${focus?.name ?? model.focusId}. ` +
    `${up} calling symbols are drawn above it as tributaries and ${down} called symbols below it as a delta, ` +
    `joined by ${model.channels.length} channels carrying ${callSites} call sites in total. ` +
    `${coverageCaption(model)}. ` +
    'The ranked list below carries the same symbols as text.'
  );
}

/**
 * Translate the model into the simulation's vocabulary: mass IS degree,
 * stiffness IS the call-site count, and the anchor IS the layout position. No
 * shaping and no normalisation that would launder the measurement — the
 * simulation's own parameters do the scaling, in one place, where they can be
 * read off a table.
 *
 * A node whose `degree` the payload omitted enters at the parameter floor
 * (`minMass`), because a body with no inertia is a numerical singularity, not
 * an honest zero. Its sill is drawn hollow by the renderer, so absence stays
 * visible.
 */
export function buildSimSpec(model: TraceModel, seed = 20260725): SimSpec {
  return {
    seed,
    nodes: model.nodes.map((node) => ({
      id: node.id,
      mass: node.degree ?? 0,
      x0: node.x0,
      y0: node.y0,
    })),
    springs: model.channels
      .filter((channel) => channel.calls > 0)
      .map((channel) => ({ a: channel.a, b: channel.b, stiffness: channel.calls })),
  };
}
