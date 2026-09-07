/**
 * The CORTEX layout model, against a wire-true `StrataMeasurementV1`.
 *
 * What this suite protects is not the picture — it is the four claims that make
 * the picture admissible: a region is placed at a depth one of its own files
 * actually has, area carries the file count, a contour is a real interval of a
 * real quantity, and nothing the drawing cap folds out disappears from the
 * model the table reads.
 */
import { describe, expect, it } from 'vitest';

import type {
  StrataClusterV1,
  StrataFileV1,
  StrataMeasurementV1,
} from '../../contracts/generated.ts';
import {
  CONTOUR_INTERVAL,
  MAX_DRAWN_REGIONS,
  buildCortexModel,
  cortexAbsences,
  cortexDescription,
  cortexLegendPanels,
  directoryOf,
} from './cortexRelief.ts';

function cluster(
  directory: string,
  overrides: Partial<StrataClusterV1> = {},
): StrataClusterV1 {
  const incoming = overrides.incoming_edges ?? 4;
  const outgoing = overrides.outgoing_edges ?? 6;
  return {
    directory,
    order: overrides.order ?? 0,
    file_count: overrides.file_count ?? 4,
    internal_edges: overrides.internal_edges ?? 6,
    incoming_edges: incoming,
    outgoing_edges: outgoing,
    boundary_edges: overrides.boundary_edges ?? incoming + outgoing,
  };
}

function file(path: string, depth: number): StrataFileV1 {
  return { path, depth, scc_size: 1, chain: [path] };
}

function measurement(
  clusters: StrataClusterV1[],
  files: StrataFileV1[],
  overrides: Partial<StrataMeasurementV1> = {},
): StrataMeasurementV1 {
  return {
    algorithm: 'tarjan_scc_then_longest_path',
    cluster_ordering: 'dsm_boundary_edges_desc_then_file_count_desc',
    clusters,
    dependency_edge_kinds: ['calls', 'uses'],
    files,
    granularity: 'file',
    graph_generation: 'g-1',
    ideal_depth: 3,
    max_depth: overrides.max_depth ?? 4,
    scan: {
      budget_ms: 4000,
      cache_scope: 'graph_generation',
      cache_state: 'hit',
      dependency_edges_examined: 900,
      files_examined: 120,
      max_dependency_edges: 40_000,
      max_files: 20_000,
    },
    ...overrides,
  };
}

describe('directoryOf', () => {
  it('matches the producer’s own clustering rule — the exact dirname', () => {
    // `dsm_clusters` keys on `file.rfind('/')`, so membership is exact dirname
    // equality and never a prefix guess.
    expect(directoryOf('src/graph/health.rs')).toBe('src/graph');
    expect(directoryOf('main.rs')).toBe('.');
  });
});

describe('elevation', () => {
  it('places a region at a depth one of its own files actually has', () => {
    const model = buildCortexModel(
      measurement(
        [cluster('src/graph', { order: 0, file_count: 4 })],
        [
          file('src/graph/a.rs', 0),
          file('src/graph/b.rs', 2),
          file('src/graph/c.rs', 2),
          file('src/graph/d.rs', 6),
        ],
      ),
    );
    const region = model.regions[0]!;
    // The mean of 0,2,2,6 is 2.5 — a depth no file in this region is at. The
    // lower median is 2, which is a depth two of them are at.
    expect(region.depth).toBe(2);
    expect(region.depthMin).toBe(0);
    expect(region.depthMax).toBe(6);
    expect(region.depthFiles).toBe(4);
  });

  it('never places a region whose files carried no depth row', () => {
    const model = buildCortexModel(
      measurement(
        [
          cluster('src/graph', { order: 0 }),
          cluster('vendor/blob', { order: 1 }),
        ],
        [file('src/graph/a.rs', 1)],
      ),
    );
    const vendored = model.regions.find((region) => region.directory === 'vendor/blob')!;
    expect(vendored.depth).toBeNull();
    expect(vendored.drawn).toBe(false);
    expect(vendored.x).toBeNull();
    expect(model.unplacedRegions).toBe(1);
    // …and it is still a region in the model, so the table can print it.
    expect(model.regions).toHaveLength(2);
  });

  it('puts bedrock at the bottom of the world and the ridge at the top', () => {
    const model = buildCortexModel(
      measurement(
        [cluster('deep', { order: 0 }), cluster('shallow', { order: 1 })],
        [file('deep/a.rs', 0), file('shallow/a.rs', 4)],
      ),
    );
    const deep = model.regions.find((r) => r.directory === 'deep')!;
    const shallow = model.regions.find((r) => r.directory === 'shallow')!;
    expect(deep.y!).toBeGreaterThan(shallow.y!);
    expect(model.strata.map((band) => band.depth)).toEqual([0, 4]);
    expect(model.strata.every((band) => band.regions === 1)).toBe(true);
  });
});

describe('area', () => {
  it('scales the radius above its floor with the square root of the file count', () => {
    const model = buildCortexModel(
      measurement(
        [
          cluster('big', { order: 0, file_count: 100 }),
          cluster('small', { order: 1, file_count: 25 }),
        ],
        [file('big/a.rs', 1), file('small/a.rs', 1)],
      ),
    );
    const big = model.regions.find((r) => r.directory === 'big')!;
    const small = model.regions.find((r) => r.directory === 'small')!;
    // Four times the files is twice the radius, above the shared floor.
    const FLOOR = 13;
    expect((big.radius! - FLOOR) / (small.radius! - FLOOR)).toBeCloseTo(2, 5);
    expect(model.widestFileCount).toBe(100);
  });
});

describe('contours', () => {
  it('counts whole lines of a real interval of a real quantity', () => {
    const model = buildCortexModel(
      measurement(
        [cluster('dense', { order: 0, file_count: 10, internal_edges: 32 })],
        [file('dense/a.rs', 1)],
      ),
    );
    const region = model.regions[0]!;
    expect(region.density).toBeCloseTo(3.2, 6);
    expect(region.contours).toBe(Math.floor(3.2 / CONTOUR_INTERVAL));
  });

  it('draws a measured zero as no relief rather than as flat ground', () => {
    const model = buildCortexModel(
      measurement(
        [
          cluster('hollow', { order: 0, file_count: 3, internal_edges: 0 }),
          cluster('solid', { order: 1, file_count: 3, internal_edges: 9 }),
        ],
        [file('hollow/a.rs', 1), file('solid/a.rs', 1)],
      ),
    );
    const hollow = model.regions.find((r) => r.directory === 'hollow')!;
    expect(hollow.contours).toBe(0);
    // Still drawn, still at its true position and true area.
    expect(hollow.drawn).toBe(true);
    expect(hollow.radius).toBeGreaterThan(0);
    expect(model.relieflessRegions).toBe(1);
  });
});

describe('the aggregation cap', () => {
  it('draws dozens of regions and keeps every folded one in the model', () => {
    const count = MAX_DRAWN_REGIONS + 9;
    const clusters = Array.from({ length: count }, (_, index) =>
      cluster(`src/mod${index}`, { order: index, file_count: 2 }),
    );
    const files = Array.from({ length: count }, (_, index) =>
      file(`src/mod${index}/a.rs`, index % 5),
    );
    const model = buildCortexModel(measurement(clusters, files));

    expect(model.drawnRegions).toHaveLength(MAX_DRAWN_REGIONS);
    expect(model.regions).toHaveLength(count);
    expect(model.foldedRegions).toBe(9);
    expect(model.foldedFiles).toBe(18);
    expect(model.drawnFiles + model.foldedFiles).toBe(model.totalFiles);
    // The cap takes the measurement's OWN ordering, so the selection rule is
    // the producer's published one and not a preference invented here.
    expect(model.drawnRegions.map((region) => region.order)).toEqual(
      Array.from({ length: MAX_DRAWN_REGIONS }, (_, index) => index),
    );
  });

  it('reports the scan budget as a cap on the terrain, not as a depth', () => {
    const model = buildCortexModel(
      measurement([cluster('a', { order: 0 })], [file('a/x.rs', 1)], {
        scan: {
          budget_ms: 4000,
          cache_scope: 'graph_generation',
          cache_state: 'miss',
          dependency_edges_examined: 40_000,
          files_examined: 20_000,
          max_dependency_edges: 40_000,
          max_files: 20_000,
        },
      }),
    );
    expect(model.capped).toBe(true);
  });
});

describe('what the sheet says about itself', () => {
  const model = buildCortexModel(
    measurement(
      [
        cluster('src/graph', { order: 0, file_count: 9, internal_edges: 18 }),
        cluster('src/store', { order: 1, file_count: 4, internal_edges: 0 }),
      ],
      [file('src/graph/a.rs', 2), file('src/store/a.rs', 0)],
    ),
  );

  it('counts every legend reading from the model, so the key cannot drift', () => {
    const panels = cortexLegendPanels(model);
    const reading = (label: string) =>
      panels.find((panel) => panel.label === label)?.reading ?? '';
    expect(reading('elevation')).toBe(`0 – ${model.maxDepth}`);
    expect(reading('area')).toContain(`${model.widestFileCount}`);
    expect(reading('contours')).toBe('0.50 e / file');
    expect(reading('hue')).toBe(`${model.drawnRegions.length} regions`);
    expect(reading('absence')).toBe(`${model.relieflessRegions} without relief`);
    expect(reading('scale')).toContain(`${model.drawnRegions.length} regions ⟵`);
    // The area panel says files, because the read is file-granular and no
    // per-directory symbol mass is served.
    expect(panels.find((panel) => panel.label === 'area')?.teach).toContain(
      'files and not symbols',
    );
  });

  it('names the channels the read does not back instead of drawing them', () => {
    const labels = cortexAbsences(model).map((panel) => panel.label);
    expect(labels).toEqual(['churn tint', 'cross-region channels', 'weather']);
    const channels = cortexAbsences(model).find(
      (panel) => panel.label === 'cross-region channels',
    )!;
    expect(channels.teach).toContain('not region-pair edge counts');
  });

  it('describes the field as the measurements it is, and points at the table', () => {
    const description = cortexDescription(model);
    expect(description).toContain('Relief terrain of 2 module regions');
    expect(description).toContain('bedrock');
    expect(description).toContain('drawn hollow');
    expect(description).toContain('The table below carries the same regions as text');
  });
});
