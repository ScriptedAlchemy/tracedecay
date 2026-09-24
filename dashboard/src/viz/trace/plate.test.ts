/**
 * The Trace anatomy plate against the wire-true neighbors fixture. Every
 * expectation is a value observed on that fixture, so a layout that starts
 * inventing, dropping or pooling a count fails here.
 */
import { describe, expect, it } from 'vitest';

import { resolveFixture } from '../../../stories/fixtures/data.ts';
import {
  DashboardEnvelopeV1Schema,
  GraphNeighborsPayloadV1Schema,
} from '../../contracts/generated.ts';
import {
  TRACE_BUDGET,
  buildTraceModel,
  undrawnNeighbours,
  type NeighborsPayload,
  type TraceModelInput,
} from './model.ts';
import { elbowPath, kindShape, layoutPlate } from './plate.ts';
import type { TraceModel } from './types.ts';
import { channelKey, inspectPath, plateDescription } from './inspect.ts';

function neighbors(id: string, limit = 200): NeighborsPayload {
  return DashboardEnvelopeV1Schema(GraphNeighborsPayloadV1Schema).parse(
    resolveFixture(`/api/plugins/graph/node/${id}/neighbors`, `?limit=${limit}`),
  ).payload;
}

function traceInput(focusId = 'sym-0', limit = 200): TraceModelInput {
  const root = neighbors(focusId, limit);
  const ids = [...new Set([...(root.callers ?? []), ...(root.callees ?? [])].map((row) => row.id))]
    .filter((id) => id !== focusId)
    .slice(0, TRACE_BUDGET.expand);
  return {
    focus: {
      id: focusId,
      kind: 'function',
      name: 'subgraph_payload',
      file_path: 'src/dashboard/graph_service.rs',
      start_line: 40,
      degree: 16,
    },
    root,
    expanded: new Map(ids.map((id) => [id, neighbors(id, limit)] as const)),
  };
}

function built(focusId = 'sym-0', limit = 200): { input: TraceModelInput; model: TraceModel } {
  const input = traceInput(focusId, limit);
  return { input, model: buildTraceModel(input) };
}

describe('undrawnNeighbours', () => {
  it('itemises exactly the symbols the coverage total counts', () => {
    const { input, model } = built();
    const undrawn = undrawnNeighbours(input, model);
    expect(model.coverage.namedButNotDrawn).toBe(9);
    expect(undrawn).toHaveLength(9);
    expect(undrawn.every((entry) => entry.hop === 2 && entry.side === 'up')).toBe(true);
    const drawn = new Set(model.nodes.map((node) => node.id));
    expect(undrawn.some((entry) => drawn.has(entry.id))).toBe(false);
  });
});

describe('inspectPath', () => {
  it('lights a hop-2 symbol back to the focus through the drawn hop-1 route only', () => {
    const { model } = built();
    const path = inspectPath(model, 'sym-33');
    const names = [...path.nodes].map((id) => model.nodes.find((node) => node.id === id)!.name);
    expect(names).toEqual(['validate_grant', 'search_payload', 'subgraph_payload']);
    expect(path.channels.size).toBe(2);
    expect(inspectPath(model, null).nodes.size).toBe(0);
    expect(inspectPath(model, 'not-drawn').nodes.size).toBe(0);
  });

  it('describes the plate without the spring field vocabulary', () => {
    const { model } = built();
    const text = plateDescription(model);
    expect(text).toContain(
      'Call neighbourhood of subgraph_payload as an anatomy plate, callers left and callees right on one call-site scale.',
    );
    expect(text).toContain('16 calling and 13 called symbols, joined by 90 channels');
    expect(text).not.toMatch(/tributar|delta/);
  });
});

describe('layoutPlate', () => {
  it('prints only measured plate fields and says absent for the rest', () => {
    const { input, model } = built();
    const layout = layoutPlate(model, input.root, { signature: null, endLine: 52 }, undrawnNeighbours(input, model), 926);
    const fields = Object.fromEntries(layout.fields.map((field) => [field.label, field.value]));
    expect(fields).toEqual({
      file: 'src/dashboard/graph_service.rs',
      lines: '40–52',
      signature: 'absent',
      degree: '16 edges, all kinds',
      callers: '7 symbols · 45 sites',
      callees: '4 symbols · 21 sites',
      'self calls': '0',
      enclosure: 'impl RetrievalService',
      'edges by kind': 'calls 66 · contains 1 · references 1',
      drawn: '7 of 7 callers · 4 of 4 callees',
    });
    expect(layout.fields.find((field) => field.label === 'signature')!.absent).toBe(true);
  });

  it('holds callers and callees to one call-site scale and counts what it leaves off', () => {
    const { input, model } = built();
    const layout = layoutPlate(model, input.root, { signature: null, endLine: null }, undrawnNeighbours(input, model), 926);
    expect(layout.stacked).toBe(false);
    expect(layout.scale.maxCalls).toBe(22);
    // One px-per-call for every bar on both sides.
    const lengths = layout.rows.map((row) => row.segments.reduce((a, b) => a + b, 0) * layout.scale.pxPerCall);
    expect(Math.max(...lengths)).toBeCloseTo(22 * layout.scale.pxPerCall);
    expect(layout.rows).toHaveLength(29);
    expect(layout.crossLinks).toBe(52);
    // Readouts wrap to their column; the words are all there.
    expect(layout.columns.find((c) => c.side === 'up' && c.hop === 2)!.notes.map((n) => n.text).join(' ')).toBe(
      '+9 named, not drawn',
    );
    const scheduler = layout.rows.find((row) => row.node.name === 'scheduler_tick')!;
    expect(scheduler.segments).toEqual([11, 10, 1]);
  });

  it('stacks callers above and callees below in a narrow column', () => {
    const { input, model } = built();
    const layout = layoutPlate(model, input.root, { signature: null, endLine: null }, undrawnNeighbours(input, model), 289);
    expect(layout.stacked).toBe(true);
    // One corridor down the gutter carries every link.
    expect(new Set(layout.connectors.flatMap((c) => c.corridors))).toEqual(new Set([12]));
    expect(layout.connectors).toHaveLength(model.channels.length);
    expect(layout.rows.every((row) => row.grow === 1)).toBe(true);
    const order = layout.columns.map((column) => `${column.side}${column.hop}`);
    expect(order).toEqual(['up2', 'up1', 'down1', 'down2']);
  });

  it('marks a list that came back at the endpoint limit as a prefix', () => {
    const { input, model } = built('sym-0', 6);
    const layout = layoutPlate(model, input.root, { signature: null, endLine: null }, undrawnNeighbours(input, model), 926);
    expect(layout.fields.find((field) => field.label === 'callers')!.value).toBe(
      '1 symbol · 6 sites · prefix',
    );
    expect(layout.columns.find((c) => c.side === 'up' && c.hop === 1)!.notes.map((n) => n.text).join(' ')).toBe(
      'a list hit the row limit: prefix only',
    );
  });
});

describe('plate connectors', () => {
  it('draws every drawn call link, bars or not, as one connector', () => {
    const { input, model } = built();
    const layout = layoutPlate(model, input.root, { signature: null, endLine: null }, undrawnNeighbours(input, model), 926);
    expect(model.channels).toHaveLength(90);
    expect(layout.connectors.map((c) => c.key).sort()).toEqual(model.channels.map(channelKey).sort());
    expect(layout.connectors.every((c) => c.d.startsWith('M'))).toBe(true);
    // Both plate edges carry a through port on this neighbourhood.
    expect(layout.throughPorts.map((p) => p.x)).toEqual([layout.plate.x, layout.plate.x + layout.plate.width]);
  });

  it('bundles a column into one corridor, so trunks carry counted links', () => {
    const { input, model } = built();
    const layout = layoutPlate(model, input.root, { signature: null, endLine: null }, undrawnNeighbours(input, model), 926);
    const corridor = new Map<number, number>();
    for (const c of layout.connectors) for (const x of c.corridors) corridor.set(x, (corridor.get(x) ?? 0) + 1);
    // Four corridors between five slots, nothing else.
    expect(corridor.size).toBe(4);
    const leftOfPlate = Math.round((layout.columns.find((c) => c.side === 'up' && c.hop === 1)!.x1 + layout.plate.x) / 2);
    const upToFocus = model.channels.filter(
      (c) => c.b === model.focusId && model.nodes.find((n) => n.id === c.a)!.ring === -1,
    );
    expect(upToFocus).toHaveLength(7);
    for (const channel of upToFocus) {
      expect(layout.connectors.find((c) => c.key === channelKey(channel))!.corridors).toEqual([leftOfPlate]);
    }
    expect([...corridor.values()].reduce((a, b) => a + b, 0)).toBeGreaterThan(model.channels.length);
  });

  it('rounds each elbow softly and never draws a zero-length corner', () => {
    expect(elbowPath([[0, 0], [10, 0], [10, 10]])).toBe('M0,0 L5,0 Q10,0 10,5 L10,10');
    expect(elbowPath([[0, 0], [20, 0], [20, 0], [20, 30]])).toBe('M0,0 L14,0 Q20,0 20,6 L20,30');
    expect(elbowPath([[0, 0]])).toBe('');
  });

  it('gives each kind a shape cue so kind never rests on hue alone', () => {
    expect(['function', 'method', 'struct', 'trait', 'module'].map(kindShape)).toEqual([
      'circle',
      'diamond',
      'square',
      'triangle',
      'bar',
    ]);
    expect(kindShape('enum')).toBe(kindShape('enum'));
  });
});
