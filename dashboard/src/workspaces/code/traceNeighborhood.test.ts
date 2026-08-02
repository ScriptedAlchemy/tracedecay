/**
 * The bound on the provisional fan-out.
 *
 * `expansionTargets` IS the cost of drawing two hops over a one-hop endpoint:
 * how many extra reads the field asks for, and which. Until the backend serves
 * a bounded two-hop neighbourhood there is no server-side contract to hold that
 * to, so it is held here — one read per DISTINCT drawn neighbour, never the
 * focus, never more than `TRACE_BUDGET.expand`, in the endpoint's own order.
 *
 * Wire-true fixture throughout (`stories/fixtures/data.ts`, mirrored from
 * `graph_service.rs::neighbors_payload`), parsed through the same generated
 * schema the surface validates responses with, so a row shape this cannot
 * happen in reality cannot be asserted about either.
 */
import { describe, expect, it } from 'vitest';

import { resolveFixture } from '../../../stories/fixtures/data.ts';
import {
  GraphNeighborsPayloadV1Schema,
  type GraphNeighborsPayloadV1,
} from '../../contracts/generated.ts';
import { TRACE_BUDGET } from '../../viz/trace/model.ts';
import { expansionTargets } from './traceNeighborhood.ts';

function neighbors(id: string): GraphNeighborsPayloadV1 {
  return GraphNeighborsPayloadV1Schema.parse(
    resolveFixture(`/api/plugins/graph/node/${id}/neighbors`),
  );
}

/** First appearance across both arms, callers first — the endpoint's order. */
function firstSeen(payload: GraphNeighborsPayloadV1, focusId: string): string[] {
  const ids = new Set<string>();
  for (const row of [...payload.callers, ...payload.callees]) {
    if (row.id !== focusId) ids.add(row.id);
  }
  return [...ids];
}

describe('expansionTargets', () => {
  it('reads each drawn neighbour once, however many call sites it has', () => {
    const payload = neighbors('sym-0');
    const rows = [...payload.callers, ...payload.callees];
    const targets = expansionTargets(payload, 'sym-0');

    // The endpoint returns one row per call site, so a caller with three of
    // them arrives three times. One read per row would be several times the
    // traffic for byte-identical payloads.
    expect(rows.length).toBeGreaterThan(targets.length);
    expect(new Set(targets).size).toBe(targets.length);
  });

  it('never asks the endpoint for the focus whose payload is already in hand', () => {
    const payload = neighbors('sym-0');
    const row = payload.callers[0]!;
    // A recursive symbol is a real `calls` row from the focus to itself, so it
    // does appear in these lists.
    const recursive = { ...payload, callers: [{ ...row, id: 'sym-0' }, ...payload.callers] };
    expect(expansionTargets(recursive, 'sym-0')).not.toContain('sym-0');
  });

  it('stops at the stated budget however many neighbours the payload names', () => {
    const payload = neighbors('sym-0');
    const row = payload.callers[0]!;
    const crowded = {
      ...payload,
      callers: Array.from({ length: TRACE_BUDGET.expand * 3 }, (_, i) => ({
        ...row,
        id: `crowd-${i}`,
      })),
    };
    // The figure that matters is the request count, not the id list: this is
    // one HTTP read per entry, issued in one wave.
    expect(expansionTargets(crowded, 'sym-0')).toHaveLength(TRACE_BUDGET.expand);
  });

  it('expands in the endpoint order, so the same neighbourhood expands the same way twice', () => {
    const payload = neighbors('sym-0');
    const targets = expansionTargets(payload, 'sym-0');

    // `ORDER BY n.qualified_name` on the server, callers arm before callees
    // arm here. Any other order would make which neighbours got expanded
    // depend on iteration luck, and the coverage figures with it.
    expect(targets).toEqual(firstSeen(payload, 'sym-0').slice(0, TRACE_BUDGET.expand));
    expect(expansionTargets(neighbors('sym-0'), 'sym-0')).toEqual(targets);
  });

  it('asks for nothing when the payload named no neighbours', () => {
    const payload = neighbors('sym-0');
    const bare = { ...payload, callers: [], callees: [] };
    expect(expansionTargets(bare, 'sym-0')).toEqual([]);
  });
});
