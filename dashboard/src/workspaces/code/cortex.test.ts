import { describe, expect, it } from 'vitest';

import type { GraphNodeV1, StrataMeasurementV1 } from '../../contracts/generated.ts';
import type { DiagnosticsSnapshot } from '../../data/query/codeDiagnostics.ts';
import {
  cortexRegister,
  densityReading,
  diagnosticsForFile,
  groupNeighbors,
  kindLegend,
  locationLabel,
  moduleReading,
  relationLegend,
  relationsBetween,
  strataForPath,
} from './cortex.ts';

function node(over: Partial<GraphNodeV1> & { id: string }): GraphNodeV1 {
  return {
    assertions: null,
    attrs_start_line: null,
    branches: null,
    complexity_analysis: null,
    degree: null,
    doc: null,
    edge_kind: null,
    edge_line: null,
    end_column: null,
    end_line: null,
    file_path: null,
    is_async: null,
    kind: 'function',
    loops: null,
    max_nesting: null,
    name: null,
    parent_id: null,
    qualified_name: null,
    returns: null,
    signature: null,
    span: null,
    start_column: null,
    start_line: null,
    unchecked_calls: null,
    unsafe_blocks: null,
    updated_at: null,
    visibility: null,
    ...over,
  };
}

describe('the register strip', () => {
  it('reads modules as the index’s module kind, and names the absence otherwise', () => {
    expect(
      moduleReading([
        { kind: 'function', count: 40 },
        { kind: 'module', count: 412 },
      ]),
    ).toEqual({ kind: 'measured', value: '412', note: 'symbols of kind module' });
    expect(moduleReading([{ kind: 'function', count: 40 }])).toEqual({
      kind: 'absent',
      reason: 'no module kind in this index',
    });
  });

  it('computes directed density over ordered pairs and refuses it below two symbols', () => {
    expect(densityReading({ nodes: 4, edges: 6, files: 1 })).toMatchObject({
      kind: 'measured',
      value: '50.00',
      unit: '%',
    });
    expect(densityReading({ nodes: 48_612, edges: 162_731, files: 3_217 })).toMatchObject({
      kind: 'measured',
      value: '0.0069',
    });
    expect(densityReading({ nodes: 12_873, edges: 41_206, files: 642 })).toMatchObject({
      value: '0.025',
    });
    expect(densityReading({ nodes: 1, edges: 0, files: 1 })).toEqual({
      kind: 'absent',
      reason: 'undefined below two symbols',
    });
  });

  it('lays out seven cells in the plate’s order and names the renderer’s real rule', () => {
    const cells = cortexRegister({
      totals: { nodes: 12_873, edges: 41_206, files: 642 },
      nodes_by_kind: [{ kind: 'module', count: 393 }],
    });
    expect(cells.map((cell) => cell.label)).toEqual([
      'nodes',
      'edges',
      'files',
      'modules',
      'density',
      'layout',
      'rank',
    ]);
    expect(cells[0]?.reading).toMatchObject({ value: '12,873', note: '12,873 symbols indexed' });
    // The field is force-settled and sized by degree; the register must not
    // promise eigenvector rank the renderer does not compute.
    expect(cells[5]?.reading).toMatchObject({ value: 'force-directed' });
    expect(cells[6]?.reading).toMatchObject({ value: 'degree' });
  });
});

describe('field legends', () => {
  it('counts what is drawn, most numerous first, ties by name', () => {
    expect(
      kindLegend([{ kind: 'struct' }, { kind: 'function' }, { kind: 'struct' }, { kind: 'enum' }]),
    ).toEqual([
      { kind: 'struct', count: 2 },
      { kind: 'enum', count: 1 },
      { kind: 'function', count: 1 },
    ]);
    expect(relationLegend([{ kind: 'calls' }, { kind: 'calls' }, { kind: 'contains' }])).toEqual([
      { kind: 'calls', count: 2 },
      { kind: 'contains', count: 1 },
    ]);
  });
});

describe('neighbour grouping', () => {
  const caller = (id: string, line: number) =>
    node({ id, name: id, edge_kind: 'calls', edge_line: line, file_path: 'src/a.rs' });

  it('collapses one-row-per-call-site into distinct neighbours with site counts', () => {
    const side = groupNeighbors(
      [caller('b', 10), caller('a', 3), caller('b', 14), caller('b', 20), caller('c', 7)],
      200,
    );
    expect(side.distinct).toBe(3);
    expect(side.sites).toBe(5);
    expect(side.capped).toBe(false);
    expect(side.neighbors.map((entry) => [entry.node.id, entry.sites, [...entry.lines]])).toEqual([
      ['b', 3, [10, 14, 20]],
      ['a', 1, [3]],
      ['c', 1, [7]],
    ]);
  });

  it('marks both counts as floors when the wire filled its row budget', () => {
    const rows = Array.from({ length: 4 }, (_, i) => caller(`n${i}`, i));
    expect(groupNeighbors(rows, 4).capped).toBe(true);
    expect(groupNeighbors(rows, 5).capped).toBe(false);
    expect(groupNeighbors([], 0).capped).toBe(false);
  });
});

describe('relation between a preview and the selection', () => {
  const edges = [
    { source: 'x', target: 'pinned', kind: 'calls', line: 12, source_name: null, target_name: null },
    { source: 'pinned', target: 'x', kind: 'references', line: null, source_name: null, target_name: null },
    { source: 'y', target: 'pinned', kind: 'calls', line: 1, source_name: null, target_name: null },
  ];

  it('names each drawn relation with its direction, and none for the selection itself', () => {
    expect(relationsBetween(edges, 'x', 'pinned')).toEqual([
      { kind: 'calls', direction: 'out', line: 12 },
      { kind: 'references', direction: 'in', line: null },
    ]);
    expect(relationsBetween(edges, 'pinned', 'pinned')).toEqual([]);
    expect(relationsBetween(edges, 'z', 'pinned')).toEqual([]);
  });
});

describe('strata for a file', () => {
  const measurement: StrataMeasurementV1 = {
    algorithm: 'longest_path_layering',
    cluster_ordering: 'boundary_edges_desc',
    granularity: 'file',
    graph_generation: 'g1',
    ideal_depth: 5,
    max_depth: 7,
    dependency_edge_kinds: ['imports'],
    clusters: [],
    files: [
      { path: 'src/storage/runtime.rs', depth: 3, scc_size: 3, chain: [] },
      { path: 'src/storage/store.rs', depth: 2, scc_size: 1, chain: [] },
    ],
    scan: {
      budget_ms: 250,
      cache_scope: 'sealed_generation',
      cache_state: 'warm',
      dependency_edges_examined: 100,
      files_examined: 2,
      max_dependency_edges: 50_000,
      max_files: 10_000,
    },
  };

  it('places an exact path at its depth and directory, cycle included', () => {
    expect(strataForPath(measurement, 'src/storage/runtime.rs')).toEqual({
      kind: 'measured',
      depth: 3,
      maxDepth: 7,
      idealDepth: 5,
      directory: 'src/storage',
      sccSize: 3,
      capped: false,
    });
  });

  it('falls back to the directory’s depths, then names the absence', () => {
    expect(strataForPath(measurement, 'src/storage/other.rs')).toEqual({
      kind: 'directory_only',
      directory: 'src/storage',
      depths: [2, 3],
      capped: false,
    });
    expect(strataForPath(measurement, 'dashboard/src/x.tsx')).toEqual({
      kind: 'not_in_scan',
      filesLaidOut: 2,
      capped: false,
    });
    expect(strataForPath(measurement, null)).toEqual({ kind: 'no_path' });
  });

  it('flags a budget-capped scan so a depth reads as a floor', () => {
    const capped = { ...measurement, scan: { ...measurement.scan, files_examined: 10_000 } };
    expect(strataForPath(capped, 'src/storage/store.rs')).toMatchObject({ capped: true });
    expect(strataForPath(capped, 'nowhere.rs')).toMatchObject({ kind: 'not_in_scan', capped: true });
  });
});

describe('diagnostics in a file', () => {
  const snapshot = {
    summary: { total_errors: 1, total_warnings: 1, pending_refreshes: 0, last_refresh_age_seconds: 4 },
    engines: [],
    settings: { idle_backfill: 'idle', languages: {} },
    settings_revision: 'r1',
    diagnostics: [
      { language: 'rust', source: 'rustc', file: '/abs/src/a.rs', line_start: 3, severity: 'error', code: null, message: 'x', enclosing_node: null },
      { language: 'rust', source: 'rustc', file: 'src/a.rs', line_start: 9, severity: 'warning', code: null, message: 'y', enclosing_node: null },
      { language: 'rust', source: 'rustc', file: 'src/b.rs', line_start: 1, severity: 'error', code: null, message: 'z', enclosing_node: null },
    ],
  } as unknown as DiagnosticsSnapshot;

  it('matches the broker’s absolute or relative path to the index path', () => {
    const file = diagnosticsForFile(snapshot, 'src/a.rs');
    expect(file?.rows.length).toBe(2);
    expect(file?.errors).toBe(1);
    expect(file?.warnings).toBe(1);
    expect(diagnosticsForFile(snapshot, null)).toBeNull();
    expect(diagnosticsForFile(snapshot, 'src/c.rs')?.rows).toEqual([]);
  });
});

describe('location label', () => {
  it('prints as much of file:start–end as the row carries', () => {
    expect(locationLabel({ file_path: 'src/a.rs', start_line: 40, end_line: 52 })).toBe('src/a.rs:40–52');
    expect(locationLabel({ file_path: 'src/a.rs', start_line: 40, end_line: 40 })).toBe('src/a.rs:40');
    expect(locationLabel({ file_path: 'src/a.rs', start_line: null })).toBe('src/a.rs');
    expect(locationLabel({ file_path: null })).toBeNull();
  });
});
