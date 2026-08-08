/**
 * CORTEX — the macro end of the structure LENS (plan 11b: `:134` "modules as
 * relief terrain — depth-strata placement, area = symbol mass, contour lines =
 * measured connectivity density", `:214` "the right macro underlay: real
 * contour interval", `:229` "Far = CORTEX").
 *
 * This module is the honesty boundary the plan's "Rendering strategy" (`:196`)
 * demands: it turns ONE wire reading — `GET /api/plugins/graph/strata`,
 * `StrataMeasurementV1` — into positions, areas and contour counts, and the
 * renderer beside it draws whatever comes out and decides nothing. Every
 * quantity below is traceable to a field on that measurement.
 *
 * WHAT THE WIRE ACTUALLY CARRIES, and what this therefore does NOT draw.
 * The strata read is FILE-granular by its own `granularity` field. So:
 *
 *   elevation  `files[].depth` — file-level dependency depth (Tarjan SCC then
 *              longest path). A directory spans several strata, so a region is
 *              placed at the MEDIAN depth of its own files and the full range
 *              is printed in the table. Not an average of anything.
 *   area       `clusters[].file_count`. The mockup's channel was symbol mass;
 *              no per-directory symbol count is served to this dashboard, and
 *              inventing one from the top-N `largest_files` sample would
 *              understate every region that is not in that sample. Files are
 *              what was measured, so files are what the area carries, and the
 *              legend says "files" rather than "symbols".
 *   contours   `clusters[].internal_edges ÷ file_count` — internal dependency
 *              edges per file, at a real interval, index contour every fifth.
 *   x          `clusters[].order`, whose rule is the measurement's own
 *              `cluster_ordering` string. Ordinal, and captioned as ordinal.
 *
 * NOT DRAWN, because the read does not carry it (see `cortexAbsences`): churn
 * tint, region-to-region channels, per-region live weather. A channel drawn
 * from a total that is not a pair, or a heat drawn from no churn read, would be
 * exactly the falsified surface this console exists to refuse.
 *
 * PERFORMANCE (plan `:175`, "the cortex aggregates regions — dozens of bodies,
 * not thousands"): the drawing is capped at `MAX_DRAWN_REGIONS`, the cap is a
 * counted figure on the plate, and every region the cap folds out stays in the
 * accessible table. A visual cap is never silent data loss.
 */
import type { StrataClusterV1, StrataMeasurementV1 } from '../../contracts/generated.ts';

/** One contour line per this many internal dependency edges per file. */
export const CONTOUR_INTERVAL = 0.5;
/** Every Nth contour is an index contour: heavier, and labelled with its value. */
export const CONTOUR_INDEX_EVERY = 5;
/** Rings a region can carry before the interior stops being readable. The exact
 * density is printed in the table either way, so this caps ink and not truth. */
export const MAX_DRAWN_CONTOURS = 9;
/** Plan `:175`. Dozens of aggregated bodies, never thousands of symbols. */
export const MAX_DRAWN_REGIONS = 28;

/** World coordinates the renderer maps into the viewport. */
export const CORTEX_WORLD = { width: 1440, height: 900 } as const;
const PAD = { left: 104, right: 44, top: 44, bottom: 64 } as const;
/** Floor radius, so the smallest region is a landform and not a speck. Matches
 * the additive-floor idiom the connectivity spine's `markDiameter` already
 * uses, and the legend states it rather than pretending area is pure. */
const MIN_RADIUS = 13;
/** Landforms are wider than they are tall. Applied uniformly, so relative area
 * between regions is untouched. */
export const RELIEF_ASPECT = { x: 1.14, y: 0.76 } as const;

export interface CortexRegion {
  /** `clusters[].directory` — the exact dirname the producer clustered on. */
  readonly directory: string;
  /** Last path segment with a trailing slash, for the on-field label. */
  readonly label: string;
  /** `clusters[].order` — position in the measurement's own cluster ordering. */
  readonly order: number;
  readonly fileCount: number;
  readonly internalEdges: number;
  readonly incomingEdges: number;
  readonly outgoingEdges: number;
  readonly boundaryEdges: number;
  /** Median depth of this region's own files, or null when no file in it
   * carried a depth row. A region with no measured depth is never placed. */
  readonly depth: number | null;
  readonly depthMin: number | null;
  readonly depthMax: number | null;
  /** Files of this region that carried a depth row. */
  readonly depthFiles: number;
  /** Internal dependency edges per file. */
  readonly density: number;
  /** Whole contour lines at `CONTOUR_INTERVAL`. Zero means measured zero
   * internal edges — drawn hollow and dashed, never drawn as flat ground. */
  readonly contours: number;
  /** Whether the region is on the drawn field at all. */
  readonly drawn: boolean;
  /** World position. Null exactly when `drawn` is false. */
  readonly x: number | null;
  readonly y: number | null;
  readonly radius: number | null;
}

export interface CortexModel {
  readonly regions: readonly CortexRegion[];
  readonly drawnRegions: readonly CortexRegion[];
  readonly world: { readonly width: number; readonly height: number };
  /** Strata actually laid out, bedrock first. */
  readonly strata: readonly { readonly depth: number; readonly y: number; readonly regions: number }[];
  readonly maxDepth: number;
  readonly idealDepth: number;
  readonly totalRegions: number;
  readonly totalFiles: number;
  readonly drawnFiles: number;
  readonly foldedRegions: number;
  readonly foldedFiles: number;
  /** Regions whose files carried no depth row: real, counted, never placed. */
  readonly unplacedRegions: number;
  /** Drawn regions with zero internal dependency edges. */
  readonly relieflessRegions: number;
  readonly widestFileCount: number;
  readonly densestRegion: CortexRegion | null;
  readonly capped: boolean;
  readonly scan: StrataMeasurementV1['scan'];
  readonly algorithm: string;
  readonly clusterOrdering: string;
  readonly granularity: string;
  readonly dependencyEdgeKinds: readonly string[];
  readonly graphGeneration: string;
}

/** The producer clusters on `file.rfind('/')`, so membership is exact dirname
 * equality and nothing has to be guessed from a prefix. */
export function directoryOf(path: string): string {
  const cut = path.lastIndexOf('/');
  return cut < 0 ? '.' : path.slice(0, cut);
}

function labelOf(directory: string): string {
  if (directory === '.' || directory === '') return './';
  const cut = directory.lastIndexOf('/');
  return `${cut < 0 ? directory : directory.slice(cut + 1)}/`;
}

/** Lower median: deterministic, and an actual observed depth rather than a
 * halfway figure no file in the region is at. */
function lowerMedian(sorted: readonly number[]): number | null {
  if (sorted.length === 0) return null;
  return sorted[Math.floor((sorted.length - 1) / 2)] ?? null;
}

function boundaryEdgesOf(cluster: StrataClusterV1): number {
  return cluster.boundary_edges;
}

export function buildCortexModel(measurement: StrataMeasurementV1): CortexModel {
  const depthsByDirectory = new Map<string, number[]>();
  for (const file of measurement.files) {
    const directory = directoryOf(file.path);
    const bucket = depthsByDirectory.get(directory);
    if (bucket) bucket.push(file.depth);
    else depthsByDirectory.set(directory, [file.depth]);
  }
  for (const depths of depthsByDirectory.values()) depths.sort((a, b) => a - b);

  // The measurement's own ordering is the selection rule, so the cap is
  // "the first N in the order the producer already published", not a
  // preference this module invented.
  const ordered = [...measurement.clusters].sort((a, b) => a.order - b.order);

  interface Draft {
    readonly cluster: StrataClusterV1;
    readonly depths: readonly number[];
    readonly depth: number | null;
  }
  const drafts: Draft[] = ordered.map((cluster) => {
    const depths = depthsByDirectory.get(cluster.directory) ?? [];
    return { cluster, depths, depth: lowerMedian(depths) };
  });

  const placeable = drafts.filter((draft) => draft.depth !== null);
  const chosen = placeable.slice(0, MAX_DRAWN_REGIONS);
  const chosenKeys = new Set(chosen.map((draft) => draft.cluster.directory));

  const maxDepth = Math.max(measurement.max_depth, 0);
  const usableWidth = CORTEX_WORLD.width - PAD.left - PAD.right;
  const usableHeight = CORTEX_WORLD.height - PAD.top - PAD.bottom;
  const bandGap = maxDepth > 0 ? usableHeight / maxDepth : usableHeight;

  const byBand = new Map<number, Draft[]>();
  for (const draft of chosen) {
    const depth = draft.depth ?? 0;
    const bucket = byBand.get(depth);
    if (bucket) bucket.push(draft);
    else byBand.set(depth, [draft]);
  }
  const widestBand = [...byBand.values()].reduce((max, band) => Math.max(max, band.length), 1);

  // ONE global scale, so the √-area law holds between every pair of regions
  // on the field rather than being bent per body by a clamp.
  const widestFileCount = chosen.reduce(
    (max, draft) => Math.max(max, draft.cluster.file_count),
    0,
  );
  const slotWidth = usableWidth / widestBand;
  const allowedRadius = Math.max(18, Math.min(slotWidth * 0.42, bandGap * 0.40));
  const areaScale =
    widestFileCount > 0 ? (allowedRadius - MIN_RADIUS) / Math.sqrt(widestFileCount) : 0;

  const bandY = (depth: number): number =>
    maxDepth > 0
      ? PAD.top + ((maxDepth - depth) / maxDepth) * usableHeight
      : PAD.top + usableHeight / 2;

  const placed = new Map<string, { x: number; y: number; radius: number }>();
  for (const [depth, band] of byBand) {
    const inBand = [...band].sort((a, b) => a.cluster.order - b.cluster.order);
    inBand.forEach((draft, index) => {
      placed.set(draft.cluster.directory, {
        x: PAD.left + ((index + 0.5) / inBand.length) * usableWidth,
        y: bandY(depth),
        radius: MIN_RADIUS + areaScale * Math.sqrt(draft.cluster.file_count),
      });
    });
  }

  const regions: CortexRegion[] = drafts.map((draft) => {
    const { cluster } = draft;
    const density = cluster.file_count > 0 ? cluster.internal_edges / cluster.file_count : 0;
    const spot = placed.get(cluster.directory) ?? null;
    const drawn = chosenKeys.has(cluster.directory) && spot !== null;
    return {
      directory: cluster.directory,
      label: labelOf(cluster.directory),
      order: cluster.order,
      fileCount: cluster.file_count,
      internalEdges: cluster.internal_edges,
      incomingEdges: cluster.incoming_edges,
      outgoingEdges: cluster.outgoing_edges,
      boundaryEdges: boundaryEdgesOf(cluster),
      depth: draft.depth,
      depthMin: draft.depths[0] ?? null,
      depthMax: draft.depths[draft.depths.length - 1] ?? null,
      depthFiles: draft.depths.length,
      density,
      contours: Math.floor(density / CONTOUR_INTERVAL),
      drawn,
      x: drawn && spot ? spot.x : null,
      y: drawn && spot ? spot.y : null,
      radius: drawn && spot ? spot.radius : null,
    };
  });

  const drawnRegions = regions.filter((region) => region.drawn);
  const drawnFiles = drawnRegions.reduce((total, region) => total + region.fileCount, 0);
  const totalFiles = regions.reduce((total, region) => total + region.fileCount, 0);
  const densestRegion = drawnRegions.reduce<CortexRegion | null>(
    (best, region) => (best === null || region.density > best.density ? region : best),
    null,
  );

  const strata = [...byBand.keys()]
    .sort((a, b) => a - b)
    .map((depth) => ({
      depth,
      y: bandY(depth),
      regions: byBand.get(depth)?.length ?? 0,
    }));

  return {
    regions,
    drawnRegions,
    world: CORTEX_WORLD,
    strata,
    maxDepth,
    idealDepth: measurement.ideal_depth,
    totalRegions: regions.length,
    totalFiles,
    drawnFiles,
    foldedRegions: regions.length - drawnRegions.length,
    foldedFiles: totalFiles - drawnFiles,
    unplacedRegions: regions.filter((region) => region.depth === null).length,
    relieflessRegions: drawnRegions.filter((region) => region.contours === 0).length,
    widestFileCount,
    densestRegion,
    capped:
      measurement.scan.files_examined >= measurement.scan.max_files ||
      measurement.scan.dependency_edges_examined >= measurement.scan.max_dependency_edges,
    scan: measurement.scan,
    algorithm: measurement.algorithm,
    clusterOrdering: measurement.cluster_ordering,
    granularity: measurement.granularity,
    dependencyEdgeKinds: measurement.dependency_edge_kinds,
    graphGeneration: measurement.graph_generation,
  };
}

/* ---- what the sheet says about itself ----------------------------------- */

export interface CortexPanel {
  readonly label: string;
  readonly reading: string;
  readonly teach: string;
}

/** The key below the field, counted from the model rather than typed by hand,
 * so the legend and the picture cannot drift apart. */
export function cortexLegendPanels(model: CortexModel): readonly CortexPanel[] {
  return [
    {
      label: 'elevation',
      reading: `0 – ${model.maxDepth}`,
      teach: `${model.granularity}-level dependency depth (${model.algorithm}) over ${model.dependencyEdgeKinds.join(', ')} edges. Stratum 0 is bedrock; a region sits at the median depth of its own files, and its full range is in the table.`,
    },
    {
      label: 'area',
      reading:
        model.widestFileCount > 0
          ? `${model.widestFileCount.toLocaleString()} files at the widest`
          : 'no region drawn',
      teach: `radius = a ${MIN_RADIUS} px floor plus √files, so above the floor area carries the count: a region twice as wide holds four times the files. The strata read is ${model.granularity}-granular, so this is files and not symbols.`,
    },
    {
      label: 'contours',
      reading: `${CONTOUR_INTERVAL.toFixed(2)} e / file`,
      teach: `one line per ${CONTOUR_INTERVAL} internal dependency edges per file; every ${CONTOUR_INDEX_EVERY}th is an index contour, drawn heavier.${
        model.densestRegion
          ? ` ${model.densestRegion.label} is densest at ${model.densestRegion.density.toFixed(2)}.`
          : ''
      }`,
    },
    {
      label: 'hue',
      reading: `${model.drawnRegions.length} regions`,
      teach:
        'the kind-hue arc from kindColor.ts, applied to the directory, so a region keeps one hue everywhere in this workspace. Outline wobble is not a measurement; area is.',
    },
    {
      label: 'absence',
      reading:
        model.relieflessRegions === 0
          ? 'every drawn region has relief'
          : `${model.relieflessRegions} without relief`,
      teach:
        'a region whose internal dependency edges measured zero is drawn at its true position and true area with a dashed outline and an empty interior. It is not missing from the sheet, and it is not flat ground.',
    },
    {
      label: 'scale',
      reading: `${model.drawnRegions.length} regions ⟵ ${model.drawnFiles.toLocaleString()} files`,
      teach: `an aggregate surface: ${model.totalFiles.toLocaleString()} files cannot be drawn as bodies, so they are drawn as mass. The cap is ${MAX_DRAWN_REGIONS} regions; every region the cap folds out is in the table below.`,
    },
  ];
}

/** Channels the mockup carries that this read does not back. Stated on the
 * surface as their own panel, because a reader who knows the sheet expects
 * heat and rivers has to be told why there are none. */
export function cortexAbsences(model: CortexModel): readonly CortexPanel[] {
  return [
    {
      label: 'churn tint',
      reading: 'not served',
      teach:
        'no git-churn read is exposed to this dashboard, so the relief carries no heat. A cool sheet here is an unmeasured sheet, not settled ground.',
    },
    {
      label: 'cross-region channels',
      reading: `${model.regions
        .reduce((total, region) => total + region.outgoingEdges, 0)
        .toLocaleString()} boundary edges, unrouted`,
      teach:
        'the strata read serves each region its own boundary totals, not region-pair edge counts, so no channel is drawn between two regions. Each region’s in and out totals are columns in the table.',
    },
    {
      label: 'weather',
      reading: 'project-granular',
      teach:
        'live activity is published for the project and carries no path, so it cannot be attributed to a region and is not drawn on this sheet.',
    },
  ];
}

/** The canvas is one `role="img"`; this is what it says. Every claim here is
 * also printed as text on the surface, and the table is the equivalent. */
export function cortexDescription(model: CortexModel): string {
  const bands = model.strata
    .map((band) => `stratum ${band.depth} holds ${band.regions}`)
    .join(', ');
  return [
    `Relief terrain of ${model.drawnRegions.length} module regions aggregating ${model.drawnFiles.toLocaleString()} files,`,
    `placed by file-level dependency depth from 0 (bedrock) to ${model.maxDepth} (ridge).`,
    bands.length > 0 ? `By stratum: ${bands}.` : '',
    `Area carries the file count; contour rings carry internal dependency edges per file at ${CONTOUR_INTERVAL} per line.`,
    model.relieflessRegions > 0
      ? `${model.relieflessRegions} regions measured zero internal edges and are drawn hollow.`
      : '',
    `The table below carries the same regions as text, including the ${model.foldedRegions} the drawing cap folds out.`,
  ]
    .filter((part) => part.length > 0)
    .join(' ');
}
