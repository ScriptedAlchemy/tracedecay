/**
 * The accessible list's order, held to the field it is the equivalent of.
 *
 * The list is admissible only because it carries the same symbols the canvas
 * draws, so the two properties worth asserting are that nothing is lost and
 * that the order is the one the surface prints under the plate: hop, then call
 * sites, then name. Both are checked against a model built from the wire-true
 * fixture rather than a hand-built one, so a change in what the field draws
 * reaches these assertions too.
 */
import { describe, expect, it } from 'vitest';

import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { TRACE_BUDGET, buildTraceModel, type NeighborsPayload } from '../../viz/trace/model.ts';
import { callSiteTotals, orderByHopThenCallSites } from './traceRanking.ts';

function neighbors(id: string): NeighborsPayload {
  return resolveFixture(`/api/plugins/graph/node/${id}/neighbors`) as NeighborsPayload;
}

function model() {
  const root = neighbors('sym-0');
  const ids = new Set<string>();
  for (const row of [...(root.callers ?? []), ...(root.callees ?? [])]) {
    if (typeof row.id === 'string' && row.id !== 'sym-0') ids.add(row.id);
  }
  const expanded = new Map<string, NeighborsPayload>();
  for (const id of [...ids].slice(0, TRACE_BUDGET.expand)) expanded.set(id, neighbors(id));
  return buildTraceModel({
    focus: { id: 'sym-0', kind: 'function', name: 'resolve_context', degree: 24 },
    root,
    expanded,
  });
}

describe('callSiteTotals', () => {
  it('credits both ends of every drawn channel', () => {
    const built = model();
    const totals = callSiteTotals(built.channels);
    const drawn = built.channels.reduce((sum, channel) => sum + channel.calls, 0);
    let credited = 0;
    for (const value of totals.values()) credited += value;
    // A call site is a fact about the edge, and an edge has two ends: the count
    // beside a caller and the count beside its callee are the same measurement
    // read from either side.
    expect(credited).toBe(drawn * 2);
    expect(totals.size).toBeGreaterThan(1);
  });

  it('counts what the field draws, not what the symbol has', () => {
    const built = model();
    const totals = callSiteTotals(built.channels);
    const focus = built.nodes.find((node) => node.id === built.focusId)!;
    // `degree` is the symbol's total over every edge kind; this is the drawn
    // call sites only. Conflating them would print an unmeasured figure under a
    // measured label.
    expect(totals.get(focus.id)).not.toBe(focus.degree);
  });
});

describe('orderByHopThenCallSites', () => {
  it('carries every drawn symbol exactly once', () => {
    const built = model();
    const ordered = orderByHopThenCallSites(built.nodes, callSiteTotals(built.channels));
    // The list is the canvas's accessible equivalent, so a symbol the field
    // draws and the list omits is a picture making a claim no reader can check.
    expect(ordered).toHaveLength(built.nodes.length);
    expect(new Set(ordered.map((node) => node.id))).toEqual(
      new Set(built.nodes.map((node) => node.id)),
    );
  });

  it('orders by hop distance, with the two sides of a hop equal', () => {
    const built = model();
    const ordered = orderByHopThenCallSites(built.nodes, callSiteTotals(built.channels));
    const rings = ordered.map((node) => Math.abs(node.ring));
    expect(rings).toEqual([...rings].sort((a, b) => a - b));
    // Unsigned, because a one-hop caller and a one-hop callee are the same
    // distance from the focus. Sorting the signed ring would put every caller
    // above every callee and read as a claim that upstream matters more.
    expect(new Set(ordered.filter((node) => Math.abs(node.ring) === 1).map((n) => n.ring)).size)
      .toBe(2);
    expect(ordered[0]!.id).toBe(built.focusId);
  });

  it('breaks a hop tie by call sites, then by name', () => {
    const built = model();
    const totals = callSiteTotals(built.channels);
    const ordered = orderByHopThenCallSites(built.nodes, totals);
    for (let i = 1; i < ordered.length; i += 1) {
      const previous = ordered[i - 1]!;
      const current = ordered[i]!;
      if (Math.abs(previous.ring) !== Math.abs(current.ring)) continue;
      const before = totals.get(previous.id) ?? 0;
      const after = totals.get(current.id) ?? 0;
      expect(before).toBeGreaterThanOrEqual(after);
      if (before === after) {
        expect(previous.name.localeCompare(current.name)).toBeLessThanOrEqual(0);
      }
    }
  });

  it('keeps a symbol with no drawn channel on the list rather than dropping it', () => {
    const built = model();
    // An empty totals map is the extreme of that case: every symbol reads as
    // zero call sites, and every one of them is still on the field.
    const ordered = orderByHopThenCallSites(built.nodes, new Map());
    expect(ordered).toHaveLength(built.nodes.length);
    const rings = ordered.map((node) => Math.abs(node.ring));
    expect(rings).toEqual([...rings].sort((a, b) => a - b));
  });

  it('leaves the order the model handed it untouched', () => {
    const built = model();
    const before = built.nodes.map((node) => node.id);
    orderByHopThenCallSites(built.nodes, callSiteTotals(built.channels));
    // The canvas draws from `model.nodes`; sorting it in place would reorder
    // the picture from underneath the list that is supposed to describe it.
    expect(built.nodes.map((node) => node.id)).toEqual(before);
  });
});
