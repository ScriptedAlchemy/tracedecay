/**
 * The instrument plate must not become a second source of truth.
 *
 * Two failure modes are worth a test each. The first is DRIFT: the legend says
 * "channel width is call sites" and prints a range that the drawn channels do
 * not actually span, because someone typed the range once and the payload
 * moved. The second is SILENT ABSENCE: a measurement the wire never sent gets
 * rendered as a blank cell or a plausible zero, which reads as "none" when the
 * truth is "not asked, not answered".
 *
 * Every test below is one of those two.
 */
import { describe, expect, it } from 'vitest';

import { resolveFixture } from '../../../stories/fixtures/data.ts';
import {
  DashboardEnvelopeV1Schema,
  GraphNeighborsPayloadV1Schema,
} from '../../contracts/generated.ts';
import { TRACE_BUDGET, buildTraceModel, type NeighborsPayload } from './model.ts';
import { legendPanels, readoutCells, type ReadoutValue } from './readout.ts';
import type { TraceModel, TraceNode } from './types.ts';

function neighbors(id: string): NeighborsPayload {
  return DashboardEnvelopeV1Schema(GraphNeighborsPayloadV1Schema).parse(
    resolveFixture(`/api/plugins/graph/node/${id}/neighbors`),
  ).payload;
}

/** The same wire-true model the field is drawn from. */
function fixtureModel(): TraceModel {
  const root = neighbors('sym-0');
  const ids = new Set<string>();
  for (const row of [...(root.callers ?? []), ...(root.callees ?? [])]) {
    if (typeof row.id === 'string') ids.add(row.id);
  }
  const expanded = new Map<string, NeighborsPayload>();
  for (const id of [...ids].slice(0, TRACE_BUDGET.expand)) expanded.set(id, neighbors(id));
  return buildTraceModel({
    focus: { id: 'sym-0', kind: 'function', name: 'resolve_context', degree: 24 },
    root,
    expanded,
  });
}

function node(over: Partial<TraceNode> & { id: string }): TraceNode {
  return {
    name: over.id,
    kind: 'function',
    degree: 4,
    filePath: 'crates/retrieval/src/lib.rs',
    startLine: 1,
    ring: 0,
    x0: 0,
    y0: 0,
    undrawnEdges: 0,
    selfCalls: 0,
    ...over,
  };
}

/** A hand-built model, for states the fixture does not happen to contain. */
function synthetic(over: {
  nodes?: readonly TraceNode[];
  channels?: TraceModel['channels'];
  membranes?: TraceModel['membranes'];
  coverage?: Partial<TraceModel['coverage']>;
}): TraceModel {
  const nodes = over.nodes ?? [node({ id: 'focus' })];
  return {
    focusId: 'focus',
    world: { width: 1200, height: 1040 },
    rows: new Map([[0, 520]]),
    nodes,
    channels: over.channels ?? [],
    membranes: over.membranes ?? [],
    coverage: {
      hopsFetched: 2,
      drawn: nodes.length,
      namedButNotDrawn: 0,
      unexpandedNeighbors: 0,
      cappedAt: null,
      capped: false,
      membranesAvailable: true,
      rowFields: ['degree', 'id', 'kind', 'name'],
      ...over.coverage,
    },
  };
}

function text(value: ReadoutValue): string {
  return value.kind === 'measured' ? `${value.value} ${value.unit ?? ''}`.trim() : value.why;
}

describe('the header readout strip', () => {
  it('prints the seven cells the approved sheet prints, in its order', () => {
    const labels = readoutCells(fixtureModel()).map((cell) => cell.label);
    expect(labels.map((label) => label.replace(/ ≤.*/, ''))).toEqual([
      'Focus',
      'Callers',
      'Callees',
      'Depth limit',
      'Beyond the limit',
      'Types entered',
      'Modules crossed',
    ]);
  });

  it('counts callers and callees from the drawn rows, not from the payload', () => {
    // The drift test. If these were read off the payload's list lengths they
    // would disagree with the picture the moment dedupe or the draw budget
    // dropped a row.
    const model = fixtureModel();
    const cells = readoutCells(model);
    const up = model.nodes.filter((n) => n.ring < 0).length;
    const down = model.nodes.filter((n) => n.ring > 0).length;
    expect(cells[1]!.value).toMatchObject({ value: String(up) });
    expect(cells[2]!.value).toMatchObject({ value: String(down) });

    const upSites = model.channels.filter((c) => c.dir === 'up').reduce((s, c) => s + c.calls, 0);
    expect(cells[1]!.value).toMatchObject({ unit: `${upSites} call sites` });
  });

  it('never renders an empty cell: every reading is measured or says why not', () => {
    for (const cell of readoutCells(fixtureModel())) {
      expect(text(cell.value).length).toBeGreaterThan(0);
      if (cell.value.kind === 'measured') expect(cell.value.value).not.toBe('');
    }
  });

  it('prints "types entered" as absent, with the reason, when no contains edge arrived', () => {
    const cell = readoutCells(synthetic({ coverage: { membranesAvailable: false } }))[5]!;
    expect(cell.value.kind).toBe('absent');
    expect(text(cell.value)).toContain('no contains edges');
    // Absence of the edge kind is not absence of types, and the cell has to
    // carry that distinction or it is simply wrong.
    expect(cell.qualifier).toContain('not a claim about whether these symbols have types');
  });

  it('discloses capping on the counts a capped list bounds', () => {
    const cells = readoutCells(synthetic({ coverage: { capped: true, cappedAt: 200 } }));
    expect(cells[1]!.qualifier).toContain('200');
    expect(cells[1]!.qualifier).toContain('floor, not a total');
    expect(cells[2]!.qualifier).toContain('floor, not a total');
    // A cell no capped list bounds must not claim one.
    expect(cells[0]!.qualifier).toBeNull();
  });

  it('discloses unexpanded neighbours on the depth cell rather than implying a clean edge', () => {
    const cell = readoutCells(synthetic({ coverage: { unexpandedNeighbors: 3 } }))[3]!;
    expect(cell.qualifier).toContain('unknown, not zero');
  });

  it('always says that symbols past the fetched hops are outside the "beyond" count', () => {
    const cell = readoutCells(synthetic({ coverage: { namedButNotDrawn: 12 } }))[4]!;
    expect(cell.value).toMatchObject({ value: '12' });
    expect(cell.qualifier).toContain('nothing was named to this view');
  });

  it('counts modules over symbols and crossings over channels', () => {
    const model = synthetic({
      nodes: [
        node({ id: 'focus', filePath: 'a/one.rs' }),
        node({ id: 'b', ring: -1, filePath: 'b/two.rs' }),
        node({ id: 'c', ring: 1, filePath: 'a/three.rs' }),
      ],
      channels: [
        { a: 'b', b: 'focus', calls: 3, dir: 'up' },
        { a: 'focus', b: 'c', calls: 2, dir: 'down' },
      ],
    });
    const cell = readoutCells(model)[6]!;
    // Two modules (a, b); one of the two channels crosses between them.
    expect(cell.value).toMatchObject({ value: '2', unit: '1 crossing' });
  });

  it('reports symbols with no file path as unattributed instead of bucketing them', () => {
    const model = synthetic({
      nodes: [node({ id: 'focus', filePath: 'a/one.rs' }), node({ id: 'b', filePath: null })],
    });
    expect(readoutCells(model)[6]!.qualifier).toContain('no file path');
  });

  it('prints modules as absent when not one drawn row carried a path', () => {
    const model = synthetic({ nodes: [node({ id: 'focus', filePath: null })] });
    expect(readoutCells(model)[6]!.value.kind).toBe('absent');
  });
});

describe('the legend row', () => {
  it('teaches exactly the six channels this field draws', () => {
    const panels = legendPanels(fixtureModel());
    expect(panels.map((p) => p.sample)).toEqual([
      'channel',
      'sill',
      'rows',
      'hue',
      'membrane',
      'mouth',
    ]);
    // The sheet's sixth panel is module relief dimmed behind the flow. This
    // surface draws none, so claiming it would be the drift this file guards.
    expect(JSON.stringify(panels).toLowerCase()).not.toContain('underlay');
  });

  it('prints the call-site range the drawn channels actually span', () => {
    const model = fixtureModel();
    const calls = model.channels.map((c) => c.calls);
    const low = Math.min(...calls);
    const high = Math.max(...calls);
    const panel = legendPanels(model)[0]!;
    expect(panel.reading).toMatchObject({
      value: low === high ? String(low) : `${low}–${high}`,
      unit: `across ${model.channels.length} channels`,
    });
  });

  it('prints the degree range over only the symbols that carried one', () => {
    const model = synthetic({
      nodes: [
        node({ id: 'focus', degree: 9 }),
        node({ id: 'b', degree: 2 }),
        node({ id: 'c', degree: null }),
      ],
    });
    const panel = legendPanels(model)[1]!;
    expect(panel.reading).toMatchObject({ value: '2–9', unit: 'over 2 symbols' });
    expect(panel.qualifier).toContain('hollow sill');
  });

  it('says the row axis is hop distance and not elevation', () => {
    // Sheet 01 spends height on dependency depth. This one must not borrow
    // that authority, which is why the wording is checked and not just the
    // number.
    expect(legendPanels(fixtureModel())[2]!.teach).toContain('not elevation');
  });

  it('agrees with the strip about the up/down split', () => {
    const model = fixtureModel();
    const cells = readoutCells(model);
    const rows = legendPanels(model)[2]!.reading;
    expect(rows.kind).toBe('measured');
    if (rows.kind !== 'measured') return;
    const [up, down] = rows.value.split(' / ');
    expect(up).toBe(`${(cells[1]!.value as { value: string }).value} ↑`);
    expect(down).toBe(`${(cells[2]!.value as { value: string }).value} ↓`);
  });

  it('reports the membrane panel as absent when the wire carried no contains edges', () => {
    const panel = legendPanels(synthetic({ coverage: { membranesAvailable: false } }))[4]!;
    expect(panel.reading.kind).toBe('absent');
    expect(panel.qualifier).toContain('no enclosure is drawn');
  });

  it('counts dashed mouths and the edges behind them from the drawn nodes', () => {
    const model = synthetic({
      nodes: [
        node({ id: 'focus', undrawnEdges: 5 }),
        node({ id: 'b', undrawnEdges: 0 }),
        node({ id: 'c', undrawnEdges: 2 }),
      ],
    });
    expect(legendPanels(model)[5]!.reading).toMatchObject({ value: '7', unit: 'at 2 symbols' });
  });

  it('distinguishes "no mouth drawn" from a measured zero', () => {
    const panel = legendPanels(synthetic({ nodes: [node({ id: 'focus', undrawnEdges: null })] }))[5]!;
    expect(panel.reading.kind).toBe('absent');
  });

  it('never renders an empty panel', () => {
    for (const panel of legendPanels(fixtureModel())) {
      expect(panel.teach.length).toBeGreaterThan(0);
      expect(text(panel.reading).length).toBeGreaterThan(0);
    }
  });
});
