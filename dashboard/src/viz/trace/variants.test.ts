/**
 * The three candidate Trace layouts against the wire-true neighbors and
 * strata fixtures. Every expectation is a value observed on that fixture, so
 * a layout that starts inventing, dropping or pooling a count fails here.
 */
import { describe, expect, it } from 'vitest';

import { resolveFixture } from '../../../stories/fixtures/data.ts';
import {
  DashboardEnvelopeV1Schema,
  GraphNeighborsPayloadV1Schema,
  StructureReadV12Schema,
} from '../../contracts/generated.ts';
import {
  TRACE_BUDGET,
  buildTraceModel,
  undrawnNeighbours,
  type NeighborsPayload,
  type TraceModelInput,
} from './model.ts';
import { layoutPlate } from './plate.ts';
import { layoutRadial, moduleOf, sectorReading } from './radial.ts';
import { gapReading, layoutTransit } from './transit.ts';
import type { TraceModel } from './types.ts';
import { inspectPath, parseTraceRenderer, variantDescription } from './variants.ts';

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

function strataDepths() {
  const read = StructureReadV12Schema.parse(
    (resolveFixture('/api/plugins/graph/strata') as { payload: unknown }).payload,
  );
  if (read.status !== 'measured') throw new Error('fixture strata is measured');
  return {
    byPath: new Map(read.measurement.files.map((file) => [file.path, file.depth])),
    maxDepth: read.measurement.max_depth,
    floor: false,
  };
}

describe('renderer selection', () => {
  it('keeps the shipped field unless a known candidate is named', () => {
    expect(parseTraceRenderer(null)).toBe('current');
    expect(parseTraceRenderer('plate')).toBe('plate');
    expect(parseTraceRenderer('sankey')).toBe('current');
  });
});

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

  it('describes the arrangement without the spring field vocabulary', () => {
    const { model } = built();
    const text = variantDescription(model, 'an anatomy plate');
    expect(text).toContain('Call neighbourhood of subgraph_payload as an anatomy plate.');
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
    expect(layout.links).toHaveLength(38);
    expect(layout.omittedChannels).toBe(52);
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
    expect(layout.links).toHaveLength(0);
    expect(layout.rows.every((row) => row.grow === 1)).toBe(true);
    const order = layout.columns.map((column) => `${column.side}${column.hop}`);
    expect(order).toEqual(['up2', 'up1', 'down1', 'down2']);
  });

  it('marks a list that came back at the endpoint limit as a prefix', () => {
    const { input, model } = built('sym-0', 6);
    const layout = layoutPlate(model, input.root, { signature: null, endLine: null }, undrawnNeighbours(input, model), 926);
    expect(layout.fields.find((field) => field.label === 'callers')!.value).toBe(
      '1 symbol · 6 sites · prefix at limit',
    );
    expect(layout.columns.find((c) => c.side === 'up' && c.hop === 1)!.notes.map((n) => n.text).join(' ')).toBe(
      'a list hit the row limit: prefix only',
    );
  });
});

describe('layoutTransit', () => {
  it('bands stations by measured file depth and never guesses a missing one', () => {
    const { model } = built();
    const layout = layoutTransit(model, strataDepths(), 926);
    expect(layout.bands.map((band) => `${band.key}:${band.kind}`)).toEqual([
      'd0:empty',
      'd1:empty',
      'd2:empty',
      'd3:empty',
      'd4:station',
      'd5:empty',
      'd6:empty',
      'd7:empty',
      'u:unmeasured',
    ]);
    // Only graph_service.rs is in the fixture's strata files.
    const measured = layout.stations.filter((station) => station.depth !== null);
    expect(measured.every((station) => station.node.filePath === 'src/dashboard/graph_service.rs')).toBe(true);
    expect(measured.map((station) => station.depth)).toEqual([4, 4, 4, 4, 4]);
    expect(layout.lines.filter((line) => line.kind === 'unmeasured')).toHaveLength(87);
    expect(layout.gaps.map(gapReading)).toEqual([
      '26 unmeasured',
      'Δ 0 · 45 unmeasured',
      'Δ 0 · 3 unmeasured',
      'Δ 0 · 13 unmeasured',
    ]);
  });

  it('draws a call into a shallower file as a climb and a deeper one as a descent', () => {
    const { model } = built();
    const focus = model.nodes.find((node) => node.id === model.focusId)!;
    const byPath = new Map(model.nodes.map((node) => [node.filePath ?? '', node.ring === 0 ? 3 : node.ring < 0 ? 1 : 5]));
    byPath.set(focus.filePath!, 3);
    const layout = layoutTransit(model, { byPath, maxDepth: 7, floor: false }, 926);
    const into = layout.lines.find((line) => line.channel.b === model.focusId && line.delta !== null)!;
    expect(into.delta).toBeGreaterThanOrEqual(0);
    expect(layout.lines.some((line) => line.kind === 'climb')).toBe(true);
    for (const line of layout.lines) {
      if (line.delta === null) continue;
      expect(line.kind).toBe(line.delta < 0 ? 'climb' : line.delta === 0 ? 'level' : 'descend');
    }
  });

  it('puts every station in the unmeasured band when strata is not measured', () => {
    const { model } = built();
    const layout = layoutTransit(model, { byPath: null, maxDepth: null, floor: false }, 926);
    expect(layout.bands.map((band) => band.kind)).toEqual(['unmeasured']);
    expect(layout.lines.every((line) => line.kind === 'unmeasured')).toBe(true);
  });
});

describe('layoutRadial', () => {
  it('sectors by module and prints each sector’s omissions apart', () => {
    const { input, model } = built();
    const undrawn = undrawnNeighbours(input, model);
    const layout = layoutRadial(model, undrawn, 926);
    expect(layout.sectors.map((sector) => `${sector.label}[${sectorReading(sector)}]`)).toEqual([
      'dashboard/src/app[+1 sym]',
      'src/storage[+1 sym]',
      'dashboard/src/workspaces/code[+2 sym]',
      'src/dashboard[+4 sym · 7 edges]',
      'src/automation[+1 sym]',
    ]);
    const hidden = layout.sectors.reduce((sum, sector) => sum + sector.hiddenSymbols, 0);
    expect(hidden).toBe(model.coverage.namedButNotDrawn);
    expect(layout.edges).toHaveLength(model.channels.length);
    // Every drawn symbol sits on the ring of its hop.
    for (const entry of layout.nodes) {
      const ring = layout.rings.find((r) => r.hop === Math.abs(entry.node.ring));
      expect(entry.radius).toBe(ring?.r ?? 0);
    }
  });

  it('never guesses a module for a row with no file', () => {
    expect(moduleOf(null)).toBeNull();
    expect(moduleOf('src/a/b.rs')).toBe('src/a');
    expect(moduleOf('lib.rs')).toBe('.');
  });
});
