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
  CORTEX_WORLD,
  MAX_DRAWN_REGIONS,
  RELIEF_ASPECT,
  buildCortexModel,
  cortexDescription,
  cortexLegendPanels,
  maxRegionsWithoutOverlap,
  reliefBodyRx,
  reliefLabelHalfWidth,
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

function assertBandClear(
  regions: ReadonlyArray<{
    readonly x: number | null;
    readonly y: number | null;
    readonly radius: number | null;
    readonly label: string;
    readonly directory: string;
  }>,
): void {
  expect(new Set(regions.map((region) => region.y)).size).toBe(1);
  const ordered = [...regions].sort((a, b) => a.x! - b.x!);
  for (let index = 1; index < ordered.length; index += 1) {
    const previous = ordered[index - 1]!;
    const current = ordered[index]!;
    const bodyGap = reliefBodyRx(previous.radius!) + reliefBodyRx(current.radius!);
    const labelGap = reliefLabelHalfWidth(previous.label) + reliefLabelHalfWidth(current.label);
    expect(current.x! - previous.x!).toBeGreaterThanOrEqual(Math.max(bodyGap, labelGap) - 1e-6);
  }
}

function assertReadableOnField(region: {
  readonly x: number | null;
  readonly y: number | null;
  readonly radius: number | null;
  readonly label: string;
}): void {
  const rx = reliefBodyRx(region.radius!);
  const ry = region.radius! * RELIEF_ASPECT.y;
  const labelHalf = reliefLabelHalfWidth(region.label);
  expect(region.x! - Math.max(rx, labelHalf)).toBeGreaterThanOrEqual(0);
  expect(region.x! + Math.max(rx, labelHalf)).toBeLessThanOrEqual(CORTEX_WORLD.width);
  expect(region.y! + 30).toBeGreaterThanOrEqual(0);
  expect(region.y! + 30).toBeLessThanOrEqual(CORTEX_WORLD.height);
  expect(region.y! - ry - 6).toBeGreaterThanOrEqual(0);
}

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

  it('keeps every same-depth region on the stratum line when the band is crowded', () => {
    const count = 20;
    const clusters = Array.from({ length: count }, (_, index) =>
      cluster(`src/mod${index}`, { order: index, file_count: 4 }),
    );
    const files = Array.from({ length: count }, (_, index) => file(`src/mod${index}/a.rs`, 1));
    const model = buildCortexModel(measurement(clusters, files, { max_depth: 4 }));
    const band = model.strata.find((stratum) => stratum.depth === 1)!;
    const sameDepth = model.drawnRegions.filter((region) => region.depth === 1);
    expect(sameDepth).toHaveLength(count);
    expect(new Set(sameDepth.map((region) => region.y))).toEqual(new Set([band.y]));
  });

  it('keeps a single crowded stratum on its measured depth line, not spread across the field', () => {
    const count = 20;
    const clusters = Array.from({ length: count }, (_, index) =>
      cluster(`src/mod${index}`, { order: index, file_count: 4 }),
    );
    const files = Array.from({ length: count }, (_, index) => file(`src/mod${index}/a.rs`, 0));
    const model = buildCortexModel(measurement(clusters, files, { max_depth: 1 }));
    const band = model.strata.find((stratum) => stratum.depth === 0)!;
    const sameDepth = model.drawnRegions.filter((region) => region.depth === 0);
    expect(sameDepth).toHaveLength(count);
    expect(new Set(sameDepth.map((region) => region.y))).toEqual(new Set([band.y]));
    const spread = Math.max(...sameDepth.map((region) => region.y!)) -
      Math.min(...sameDepth.map((region) => region.y!));
    expect(spread).toBe(0);
  });

  it('folds crowded same-depth bodies instead of overlapping them', () => {
    const count = 28;
    const clusters = Array.from({ length: count }, (_, index) =>
      cluster(`src/mod${index}`, { order: index, file_count: 4 }),
    );
    const files = Array.from({ length: count }, (_, index) => file(`src/mod${index}/a.rs`, 0));
    const model = buildCortexModel(measurement(clusters, files, { max_depth: 1 }));
    const labels = clusters.map(
      (item) => `${item.directory.slice(item.directory.lastIndexOf('/') + 1)}/`,
    );
    const capacity = maxRegionsWithoutOverlap(model.world.width - 104 - 44, labels);
    expect(model.drawnRegions.length).toBe(capacity);
    expect(model.foldedRegions).toBe(count - capacity);
    expect(model.foldedRegions).toBeGreaterThan(0);
    assertBandClear(model.drawnRegions.filter((region) => region.depth === 0));
  });

  it('keeps multi-depth bedrock bands free of body and label collisions', () => {
    const bedrockCount = 16;
    const ridgeCount = 6;
    const count = bedrockCount + ridgeCount;
    const clusters = Array.from({ length: count }, (_, index) =>
      cluster(`src/mod${index}`, { order: index, file_count: 8 }),
    );
    const files = Array.from({ length: count }, (_, index) =>
      file(`src/mod${index}/a.rs`, index < bedrockCount ? 0 : 1 + (index % 3)),
    );
    const model = buildCortexModel(measurement(clusters, files, { max_depth: 3 }));
    expect(model.maxDepth).toBeGreaterThanOrEqual(3);
    const bedrock = model.drawnRegions.filter((region) => region.depth === 0);
    const labels = clusters
      .slice(0, bedrockCount)
      .map((item) => `${item.directory.slice(item.directory.lastIndexOf('/') + 1)}/`);
    expect(bedrock.length).toBeGreaterThanOrEqual(1);
    expect(bedrock.length).toBeLessThanOrEqual(
      maxRegionsWithoutOverlap(model.world.width - 104 - 44, labels),
    );
    if (bedrockCount > maxRegionsWithoutOverlap(model.world.width - 104 - 44, labels)) {
      expect(model.foldedRegions).toBeGreaterThan(0);
    }
    assertBandClear(bedrock);
  });

  it('lets a sparse two-region band keep a space-driven radius well above the fold floor', () => {
    const model = buildCortexModel(
      measurement(
        [
          cluster('src/alpha', { order: 0, file_count: 100 }),
          cluster('src/beta', { order: 1, file_count: 25 }),
        ],
        [file('src/alpha/a.rs', 0), file('src/beta/a.rs', 0)],
        { max_depth: 1 },
      ),
    );
    const big = model.regions.find((region) => region.directory === 'src/alpha')!;
    const small = model.regions.find((region) => region.directory === 'src/beta')!;
    expect(big.radius).toBeGreaterThan(100);
    expect(small.radius).toBeGreaterThan(40);
    expect((big.radius! - 13) / (small.radius! - 13)).toBeCloseTo(2, 5);
    for (const region of model.drawnRegions) assertReadableOnField(region);
    assertBandClear(model.drawnRegions);
  });

  it('sizes band capacity from the abbreviated canvas label, not the full directory path', () => {
    const count = 8;
    const clusters = Array.from({ length: count }, (_, index) =>
      cluster(
        `crates/tracedecay-code-index-runtime/src/code_index_scheduler/region${index}`,
        { order: index, file_count: 6 },
      ),
    );
    const files = Array.from({ length: count }, (_, index) =>
      file(
        `crates/tracedecay-code-index-runtime/src/code_index_scheduler/region${index}/mod.rs`,
        1,
      ),
    );
    const model = buildCortexModel(measurement(clusters, files, { max_depth: 2 }));
    expect(model.drawnRegions).toHaveLength(count);
    expect(model.readabilityFoldedRegions).toBe(0);
    expect(model.capFoldedRegions).toBe(0);
    expect(model.regions[0]!.directory.length).toBeGreaterThan(60);
    expect(model.regions[0]!.label).toBe('region0/');
    assertBandClear(model.drawnRegions);
    const scale = cortexLegendPanels(model).find((panel) => panel.label === 'scale')!;
    expect(scale.teach).toMatch(/whole clustering is drawn/);
    expect(scale.teach).not.toMatch(/28-region drawing cap folds/);
  });

  it('keeps a representable ridge after folding a crowded bedrock band', () => {
    const bedrockCount = 28;
    const clusters = [
      ...Array.from({ length: bedrockCount }, (_, index) =>
        cluster(`src/bedrock${index}`, { order: index, file_count: 4 }),
      ),
      cluster('src/ridge', { order: bedrockCount, file_count: 8 }),
    ];
    const files = [
      ...Array.from({ length: bedrockCount }, (_, index) => file(`src/bedrock${index}/a.rs`, 0)),
      file('src/ridge/a.rs', 3),
    ];
    const model = buildCortexModel(measurement(clusters, files, { max_depth: 3 }));
    const ridge = model.regions.find((region) => region.directory === 'src/ridge')!;
    expect(ridge.drawn).toBe(true);
    expect(ridge.depth).toBe(3);
    expect(model.drawnRegions.length).toBeLessThanOrEqual(MAX_DRAWN_REGIONS);
    expect(model.foldedRegions).toBe(model.regions.length - model.drawnRegions.length);
    expect(model.readabilityFoldedRegions + model.capFoldedRegions).toBe(model.foldedRegions);
    expect(model.readabilityFoldedRegions).toBeGreaterThan(0);
    const scale = cortexLegendPanels(model).find((panel) => panel.label === 'scale')!;
    expect(scale.teach).toMatch(/readable/);
    expect(cortexDescription(model)).toMatch(/readability/);
    assertBandClear(model.drawnRegions.filter((region) => region.depth === 0));
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
