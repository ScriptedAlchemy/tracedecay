/**
 * Measurement → model for the TRACE surface.
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
 *   length of the channel's bar on the plate's one call-site scale.
 * - `degree` is the node's total (in + out) edge count over ALL edge kinds.
 *   Subtracting the call sites this frame draws gives the edges it does not.
 * - `edges` carries every edge kind incident on the focus, including
 *   `contains`. Membranes are derived from those rows and from nothing else,
 *   no shared-file-path guessing. When the payload carries no `contains` rows,
 *   `coverage.membranesAvailable` is false and the readout says the wire did
 *   not carry them.
 * - Both lists are truncated at `limit` (max 200). A list that comes back
 *   exactly at `limit` is a prefix, and `coverage.capped` records it.
 *
 * Depth: the caller fetches hop 1 for the focus and then hop 1 for as many of
 * the drawn hop-1 neighbours as the budget allows. There is no server-side
 * depth-2 query, so hop 2 is assembled here, bounded, deduped, and counted.
 */
import type {
  GraphEdgeV1,
  GraphNeighborsPayloadV1,
  GraphNodeV1,
} from '../../contracts/generated.ts';
import type {
  TraceChannel,
  TraceChannelDirection,
  TraceCoverage,
  TraceMembrane,
  TraceModel,
  TraceNode,
} from './types.ts';

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

/** Neighbour symbol row as the generated neighbors contract serves it. */
export type NeighborRow = GraphNodeV1;

/** Neighbourhood edge row as the generated neighbors contract serves it. */
export type NeighborEdgeRow = GraphEdgeV1;

/** The neighbors payload is the generated contract, not a hand-written mirror. */
export type NeighborsPayload = GraphNeighborsPayloadV1;

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

function rows(list: readonly NeighborRow[] | null | undefined): NeighborRow[] {
  return (list ?? []).filter((row) => row.id.length > 0);
}

/**
 * Distinct ids in a row list, with the number of rows each appeared in, which
 * is that pair's call-site count, because the endpoint emits one row per edge.
 */
function callSites(list: NeighborRow[]): Map<string, { row: NeighborRow; calls: number }> {
  const out = new Map<string, { row: NeighborRow; calls: number }>();
  for (const row of list) {
    const id = row.id;
    const seen = out.get(id);
    if (seen) seen.calls += 1;
    else out.set(id, { row, calls: 1 });
  }
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
 * beyond the fetched hops are never counted, they were never named to us.
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
  // whichever arm of that neighbour named it, the row is hop DISTANCE, and
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
      // Recursion. A real `calls` row that couples no two symbols, so it is
      // counted on the node and printed there rather than drawn as a channel
      // or quietly discarded.
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
    // A one-member enclosure is a true `contains` edge but encloses nothing
    // else on this frame, so it is not counted as a type the calls enter.
    .filter(([, entry]) => entry.of.length >= 2)
    .map(([id, entry]) => ({ id, label: entry.label, of: entry.of }))
    .sort((a, b) => a.id.localeCompare(b.id));

  const membraneOf = new Map<string, string>();
  for (const membrane of membranes) {
    for (const member of membrane.of) membraneOf.set(member, membrane.id);
  }

  /* ---- nodes: nearest hop first, then call sites, then id ------------- */
  const nodes: TraceNode[] = [...drafts.values()]
    .sort(
      (a, b) =>
        Math.abs(a.ring) - Math.abs(b.ring) ||
        a.ring - b.ring ||
        (callSitesOn.get(b.id) ?? 0) - (callSitesOn.get(a.id) ?? 0) ||
        a.id.localeCompare(b.id),
    )
    .map((draft) => ({
      id: draft.id,
      name: draft.name,
      kind: draft.kind,
      degree: draft.degree,
      filePath: draft.filePath,
      startLine: draft.startLine,
      ring: draft.ring,
      undrawnEdges:
        draft.degree == null ? null : Math.max(0, draft.degree - (callSitesOn.get(draft.id) ?? 0)),
      selfCalls: selfCallsOn.get(draft.id) ?? 0,
    }));

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
  };

  return {
    focusId: focus.id,
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
      ? `${model.membranes.length} type ${model.membranes.length === 1 ? 'enclosure' : 'enclosures'} from contains edges`
      : 'the payload carried no contains edges, which says nothing about whether these symbols have types',
  );
  return parts.join(' · ');
}

/** A symbol a fetched list named that the field does not draw. */
export interface UndrawnNeighbour {
  readonly id: string;
  readonly filePath: string | null;
  /** 1 when the focus's own list named it, 2 when an expanded neighbour's did. */
  readonly hop: 1 | 2;
  /** Side of the drawn symbol whose list named it first. */
  readonly side: 'up' | 'down';
}

/**
 * The symbols behind `coverage.namedButNotDrawn`, with the file each row
 * carried, so a renderer can print the omission where it happens instead of
 * only as one total. Read from the same payloads `buildTraceModel` absorbed.
 */
export function undrawnNeighbours(
  input: TraceModelInput,
  model: TraceModel,
): readonly UndrawnNeighbour[] {
  const drawn = new Map(model.nodes.map((node) => [node.id, node.ring]));
  const out = new Map<string, UndrawnNeighbour>();
  const visit = (payload: NeighborsPayload, hop: 1 | 2, ownerRing: number) => {
    for (const side of ['callers', 'callees'] as const) {
      for (const row of rows(payload[side])) {
        if (drawn.has(row.id) || out.has(row.id)) continue;
        const up = hop === 1 ? side === 'callers' : ownerRing < 0;
        out.set(row.id, {
          id: row.id,
          filePath: row.file_path ?? null,
          hop,
          side: up ? 'up' : 'down',
        });
      }
    }
  };
  visit(input.root, 1, 0);
  for (const [seed, payload] of input.expanded) {
    const ring = drawn.get(seed);
    if (ring === undefined || Math.abs(ring) !== 1) continue;
    visit(payload, 2, ring);
  }
  return [...out.values()];
}
