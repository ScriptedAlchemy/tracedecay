/**
 * Honesty contract for the TRACE model builder.
 *
 * The picture is allowed to leave things out; it is not allowed to leave them
 * out silently, and it is not allowed to draw anything the wire did not carry.
 * These tests hold `buildTraceModel` to both halves of that, against the
 * wire-true neighbors fixture (`stories/fixtures/data.ts`, mirrored from
 * `graph_service.rs::neighbors_payload`).
 */
import { describe, expect, it } from 'vitest';

import { resolveFixture } from '../../../stories/fixtures/data.ts';
import {
  GraphNeighborsPayloadV1Schema,
} from '../../contracts/generated.ts';
import { TRACE_BUDGET, buildSimSpec, buildTraceModel, type NeighborsPayload } from './model.ts';
import { ringLabel } from './render.ts';

function neighbors(id: string): NeighborsPayload {
  return resolveFixture(`/api/plugins/graph/node/${id}/neighbors`) as NeighborsPayload;
}

function hopIds(payload: NeighborsPayload): string[] {
  const ids = new Set<string>();
  for (const row of [...(payload.callers ?? []), ...(payload.callees ?? [])]) {
    if (typeof row.id === 'string') ids.add(row.id);
  }
  return [...ids];
}

function model(focusId = 'sym-0', expandCount: number = TRACE_BUDGET.expand) {
  const root = neighbors(focusId);
  const expanded = new Map<string, NeighborsPayload>();
  for (const id of hopIds(root).slice(0, expandCount)) expanded.set(id, neighbors(id));
  return buildTraceModel({
    focus: { id: focusId, kind: 'function', name: 'resolve_context', degree: 24 },
    root,
    expanded,
  });
}

describe('neighbors payload', () => {
  it('parses against the schema the workspace validates it with', () => {
    expect(() => GraphNeighborsPayloadV1Schema.parse(neighbors('sym-0'))).not.toThrow();
  });

  it('carries one caller/callee ROW PER CALL SITE, which is the only place the wire counts them', () => {
    const payload = GraphNeighborsPayloadV1Schema.parse(neighbors('sym-0'));
    const callers = payload.callers ?? [];
    const distinct = new Set(callers.map((row) => row.id));
    // If rows were already deduped per pair the drill-in would have no
    // call-site measurement at all and every channel would be width 1.
    expect(callers.length).toBeGreaterThan(distinct.size);
    const repeated = callers.filter((row) => row.id === callers[0]!.id);
    expect(new Set(repeated.map((row) => row.edge_line)).size).toBe(repeated.length);
  });
});

describe('buildTraceModel', () => {
  it('places every symbol on the ring it was fetched at, and nowhere else', () => {
    // The row caption is "hop distance from the focus, not elevation", so a
    // node's drawn row and the hop at which the fetch reached it cannot be
    // allowed to disagree — that caption is the whole reason this is not a
    // decorative flow diagram.
    const built = model();
    const root = neighbors('sym-0');
    const hop1 = new Set(hopIds(root));
    expect(built.nodes.find((n) => n.id === built.focusId)!.ring).toBe(0);
    for (const node of built.nodes) {
      if (node.id === built.focusId) continue;
      const ring = Math.abs(node.ring);
      expect(ring === 1 || ring === 2, `${node.id} is on ring ${node.ring}`).toBe(true);
      if (ring === 1) expect(hop1.has(node.id)).toBe(true);
      // A hop-2 symbol must NOT have been reachable at hop 1, or the dedupe
      // that gives "first discovery wins" its meaning has broken.
      if (ring === 2) expect(hop1.has(node.id)).toBe(false);
    }
    // Every drawn ring has a row, and the rows read top-to-bottom by ring.
    const ys = [...built.rows].sort((a, b) => a[0] - b[0]).map(([, y]) => y);
    expect(ys).toEqual([...ys].sort((a, b) => a - b));
    expect(ringLabel(-2)).toBe('2 hops up');
    expect(ringLabel(1)).toBe('1 hop down');
    expect(ringLabel(0)).toBe('focus');
  });

  it('draws only channels whose BOTH ends are drawn, and counts the rest', () => {
    const built = model();
    const drawn = new Set(built.nodes.map((node) => node.id));
    for (const channel of built.channels) {
      expect(drawn.has(channel.a)).toBe(true);
      expect(drawn.has(channel.b)).toBe(true);
      expect(channel.a).not.toBe(channel.b);
      expect(channel.calls).toBeGreaterThan(0);
    }
    // Symbols the fetched lists named but the budget excluded are counted, not
    // discarded — the caption prints this number.
    expect(built.coverage.namedButNotDrawn).toBeGreaterThan(0);
    expect(built.coverage.drawn).toBe(built.nodes.length);
  });

  it('reports recursion as a self-call rather than as a channel', () => {
    // A self-loop spring has no second body, so a naive builder either crashes
    // or drops the row. Neither is acceptable: recursion is measured.
    const built = model();
    const recursive = built.nodes.filter((node) => node.selfCalls > 0);
    expect(recursive.length).toBeGreaterThan(0);
    for (const node of recursive) {
      expect(built.channels.some((c) => c.a === node.id && c.b === node.id)).toBe(false);
    }
    expect(buildSimSpec(built).springs.every((s) => s.a !== s.b)).toBe(true);
  });

  it('derives membranes from contains rows only, and says so when there are none', () => {
    const built = model();
    expect(built.coverage.membranesAvailable).toBe(true);
    const drawn = new Set(built.nodes.map((node) => node.id));
    for (const membrane of built.membranes) {
      // An enclosure a reader can see flow enter and leave needs ≥2 members,
      // and every member must be a symbol actually on the field.
      expect(membrane.of.length).toBeGreaterThanOrEqual(2);
      for (const member of membrane.of) expect(drawn.has(member)).toBe(true);
    }

    // A payload with no `contains` rows must produce NO membranes and must
    // record that the wire did not carry them — never an inferred enclosure
    // from shared file paths.
    const root = neighbors('sym-0');
    const stripped: NeighborsPayload = {
      ...root,
      edges: (root.edges ?? []).filter((edge) => edge.kind !== 'contains'),
      edges_by_kind: (root.edges_by_kind ?? []).filter((entry) => entry.kind !== 'contains'),
    };
    const bare = buildTraceModel({
      focus: { id: 'sym-0', kind: 'function', name: 'resolve_context', degree: 24 },
      root: stripped,
      expanded: new Map(),
    });
    expect(bare.membranes).toEqual([]);
    expect(bare.coverage.membranesAvailable).toBe(false);
  });

  it('counts unexpanded neighbours instead of implying they have none', () => {
    const built = model('sym-0', 2);
    expect(built.coverage.hopsFetched).toBe(2);
    expect(built.coverage.unexpandedNeighbors).toBeGreaterThan(0);

    const hop1Only = model('sym-0', 0);
    expect(hop1Only.coverage.hopsFetched).toBe(1);
    expect(hop1Only.nodes.every((node) => Math.abs(node.ring) <= 1)).toBe(true);
  });

  it('differences degree against drawn call sites to report the edges it omits', () => {
    const built = model();
    for (const node of built.nodes) {
      if (node.degree == null) {
        // An unmeasured degree cannot be differenced, so no figure is invented.
        expect(node.undrawnEdges).toBeNull();
        continue;
      }
      expect(node.undrawnEdges).not.toBeNull();
      expect(node.undrawnEdges!).toBeGreaterThanOrEqual(0);
      expect(node.undrawnEdges!).toBeLessThanOrEqual(node.degree);
    }
    expect(built.nodes.some((node) => (node.undrawnEdges ?? 0) > 0)).toBe(true);
  });

  it('keeps the field inside the drawing budget and inside the world box', () => {
    const built = model();
    expect(built.nodes.length).toBeLessThanOrEqual(
      1 + TRACE_BUDGET.hop1PerSide * 2 + TRACE_BUDGET.hop2PerSide * 2,
    );
    for (const node of built.nodes) {
      expect(node.x0).toBeGreaterThanOrEqual(0);
      expect(node.x0).toBeLessThanOrEqual(built.world.width);
      expect(node.y0).toBeGreaterThanOrEqual(0);
      expect(node.y0).toBeLessThanOrEqual(built.world.height);
    }
    // Node ids are unique: a duplicate would crash the simulation at build.
    expect(new Set(built.nodes.map((n) => n.id)).size).toBe(built.nodes.length);
  });

  it('is deterministic: the same payloads build the identical field', () => {
    expect(JSON.stringify(toComparable(model()))).toBe(JSON.stringify(toComparable(model())));
  });

  it('survives an empty neighbourhood without inventing one', () => {
    const built = buildTraceModel({
      focus: { id: 'lonely', kind: 'function', name: 'lonely', degree: 0 },
      root: { node_id: 'lonely', depth: 1, limit: 50, callers: [], callees: [], edges: [] },
      expanded: new Map(),
    });
    expect(built.nodes.map((n) => n.id)).toEqual(['lonely']);
    expect(built.channels).toEqual([]);
    expect(built.membranes).toEqual([]);
    expect(built.coverage.namedButNotDrawn).toBe(0);
    expect(built.coverage.membranesAvailable).toBe(false);
  });
});

function toComparable(built: ReturnType<typeof model>) {
  return {
    nodes: built.nodes,
    channels: built.channels,
    membranes: built.membranes,
    coverage: built.coverage,
    rows: [...built.rows],
  };
}
