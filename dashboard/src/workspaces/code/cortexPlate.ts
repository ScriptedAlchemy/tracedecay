/**
 * Cortex renderer C, the stratified plate.
 *
 * A machined instrument rather than a picture: horizontal strata by measured
 * dependency depth, one column per module (directory), every symbol a station
 * on shared scales, and relations routed as transit lines through the channel
 * under each stratum. Nothing is placed by a force; every coordinate is a
 * rank or a measurement the plate prints.
 *
 * The depth axis has exactly one basis per drawing, never a mix:
 *
 *   strata  the index's own file depth (`/strata`, longest-path layering),
 *           used when the scan measured at least half of the drawn files.
 *           Symbols whose file it did not lay out stand in a hatched NOT IN
 *           SCAN band, printed, not dropped or guessed.
 *   slice   otherwise, the drawn slice's own layering: longest path over its
 *           dependency relations with cycles condensed. The header says why
 *           the index strata were not used.
 *
 * A relation that climbs against the layering, or runs inside a condensed
 * cycle, is drawn in the error hue, and the header counts them. That is a
 * boundary observation, not a finding of a bug. Containment relations are
 * not dependencies; they are counted, not routed.
 */
import type { StrataMeasurementV1 } from '../../contracts/generated.ts';
import { absenceReason, type StructureResult } from '../../data/query/structure.ts';
import {
  degreeRadius,
  drawHaloLabel,
  drawStateMarks,
  drawSymbolBody,
  modulesOf,
  monoFont,
  placeLabels,
  project,
  type CortexPainter,
  type CortexScene,
  type LabelCandidate,
  type PaintFrame,
} from './cortexScene.ts';

/** Relation kinds the slice basis layers on. `contains` is structure, not use. */
export const DEPENDENCY_KINDS: ReadonlySet<string> = new Set(['calls', 'references', 'imports', 'uses']);

export type StrataInput = StructureResult<StrataMeasurementV1> | 'pending';

export type DepthBasis =
  | { readonly kind: 'strata'; readonly placedFiles: number; readonly drawnFiles: number; readonly generation: string }
  | { readonly kind: 'slice'; readonly why: string; readonly cycles: number; readonly dependencyEdges: number };

export interface Stratification {
  readonly basis: DepthBasis;
  /** Depth per symbol; null only under the strata basis, for an unscanned file. */
  readonly depth: ReadonlyMap<string, number | null>;
  readonly maxDepth: number;
  /** Symbols in each condensed cycle of two or more, keyed by component. */
  readonly component: ReadonlyMap<string, number>;
}

/** Tarjan's strongly connected components over the slice's dependency edges. */
function components(scene: CortexScene): { of: Map<string, number>; sizes: number[]; out: Map<string, string[]> } {
  const out = new Map<string, string[]>(scene.nodes.map((node) => [node.id, []]));
  for (const edge of scene.edges) {
    if (DEPENDENCY_KINDS.has(edge.kind) && edge.source !== edge.target) {
      out.get(edge.source)!.push(edge.target);
    }
  }
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const onStack = new Set<string>();
  const stack: string[] = [];
  const of = new Map<string, number>();
  const sizes: number[] = [];
  let counter = 0;
  const visit = (v: string) => {
    index.set(v, counter);
    low.set(v, counter);
    counter += 1;
    stack.push(v);
    onStack.add(v);
    for (const w of out.get(v)!) {
      if (!index.has(w)) {
        visit(w);
        low.set(v, Math.min(low.get(v)!, low.get(w)!));
      } else if (onStack.has(w)) {
        low.set(v, Math.min(low.get(v)!, index.get(w)!));
      }
    }
    if (low.get(v) === index.get(v)) {
      const id = sizes.length;
      let size = 0;
      let w: string;
      do {
        w = stack.pop()!;
        onStack.delete(w);
        of.set(w, id);
        size += 1;
      } while (w !== v);
      sizes.push(size);
    }
  };
  for (const node of scene.nodes) if (!index.has(node.id)) visit(node.id);
  return { of, sizes, out };
}

/** Longest dependency path below each symbol, cycles condensed. 0 = bedrock. */
export function sliceDepths(scene: CortexScene): {
  depth: Map<string, number>;
  component: Map<string, number>;
  cycles: number;
  dependencyEdges: number;
} {
  const { of, sizes, out } = components(scene);
  const below = new Map<number, Set<number>>();
  let dependencyEdges = 0;
  for (const [v, targets] of out) {
    dependencyEdges += targets.length;
    for (const w of targets) {
      const a = of.get(v)!;
      const b = of.get(w)!;
      if (a === b) continue;
      const set = below.get(a) ?? new Set<number>();
      set.add(b);
      below.set(a, set);
    }
  }
  const memo = new Map<number, number>();
  const depthOf = (c: number): number => {
    const hit = memo.get(c);
    if (hit !== undefined) return hit;
    let d = 0;
    for (const b of below.get(c) ?? []) d = Math.max(d, depthOf(b) + 1);
    memo.set(c, d);
    return d;
  };
  const depth = new Map<string, number>();
  const component = new Map<string, number>();
  for (const node of scene.nodes) {
    const c = of.get(node.id)!;
    depth.set(node.id, depthOf(c));
    if (sizes[c]! > 1) component.set(node.id, c);
  }
  return { depth, component, cycles: sizes.filter((size) => size > 1).length, dependencyEdges };
}

export function stratify(scene: CortexScene, strata: StrataInput | undefined): Stratification {
  const drawnFiles = new Set(scene.nodes.flatMap((node) => (node.filePath ? [node.filePath] : [])));
  if (strata !== undefined && strata !== 'pending' && strata.outcome === 'measured') {
    const byPath = new Map(strata.measurement.files.map((file) => [file.path, file.depth]));
    const placedFiles = [...drawnFiles].filter((path) => byPath.has(path)).length;
    if (placedFiles > 0 && placedFiles * 2 >= drawnFiles.size) {
      const depth = new Map<string, number | null>();
      let maxDepth = 0;
      for (const node of scene.nodes) {
        const d = node.filePath ? (byPath.get(node.filePath) ?? null) : null;
        depth.set(node.id, d);
        if (d !== null) maxDepth = Math.max(maxDepth, d);
      }
      return {
        basis: {
          kind: 'strata',
          placedFiles,
          drawnFiles: drawnFiles.size,
          generation: strata.measurement.graph_generation,
        },
        depth,
        maxDepth,
        component: new Map(),
      };
    }
  }
  const why =
    strata === undefined || strata === 'pending'
      ? 'index strata still reading'
      : strata.outcome === 'measured'
        ? `index strata place ${
            [...drawnFiles].filter((path) =>
              strata.measurement.files.some((file) => file.path === path),
            ).length
          } of ${drawnFiles.size} drawn files, under half`
        : `index strata ${strata.outcome}: ${absenceReason(strata)}`;
  const slice = sliceDepths(scene);
  let maxDepth = 0;
  for (const d of slice.depth.values()) maxDepth = Math.max(maxDepth, d);
  return {
    basis: { kind: 'slice', why, cycles: slice.cycles, dependencyEdges: slice.dependencyEdges },
    depth: slice.depth,
    maxDepth,
    component: slice.component,
  };
}

/* ---- geometry ------------------------------------------------------------ */

/** Tall enough that the field's camera controls clear the column heads. */
const HEADER = 46;
const COLUMN_HEAD = 20;
const LEFT = 88;
const RIGHT = 14;
const BOTTOM = 8;
const CAMERA_CLEARANCE = 190;
const R_MAX = 7;
const PITCH = 2 * R_MAX + 20;
const ROW = 24;

export type BandKey = number | 'unplaced';

export interface PlateBand {
  readonly key: BandKey;
  readonly y0: number;
  readonly y1: number;
  readonly count: number;
}

export interface PlateColumn {
  readonly module: string;
  readonly x0: number;
  readonly x1: number;
  readonly count: number;
}

export interface PlateFold {
  readonly x: number;
  readonly y: number;
  readonly hidden: number;
}

export interface PlateLayout {
  readonly width: number;
  readonly height: number;
  readonly strat: Stratification;
  readonly bands: readonly PlateBand[];
  readonly columns: readonly PlateColumn[];
  readonly positions: ReadonlyMap<string, { x: number; y: number }>;
  readonly bandOf: ReadonlyMap<string, PlateBand>;
  readonly folds: readonly PlateFold[];
  readonly containsEdges: number;
}

export function plateLayout(
  scene: CortexScene,
  box: { width: number; height: number },
  strata: StrataInput | undefined,
): PlateLayout {
  const strat = stratify(scene, strata);
  const keyOf = (id: string): BandKey => strat.depth.get(id) ?? 'unplaced';
  const keys: BandKey[] = [];
  for (let d = strat.maxDepth; d >= 0; d -= 1) keys.push(d);
  if (scene.nodes.some((node) => strat.depth.get(node.id) == null)) keys.push('unplaced');

  // Every band keeps one readable row; the height left over is shared by the
  // square root of population. The axis prints each band's exact count.
  const counts = keys.map((key) => scene.nodes.filter((node) => keyOf(node.id) === key).length);
  const available = Math.max(keys.length * ROW, box.height - HEADER - COLUMN_HEAD - BOTTOM);
  const spare = available - keys.length * ROW;
  const shares = counts.map((count) => Math.sqrt(count));
  const shareTotal = shares.reduce((sum, share) => sum + share, 0) || 1;
  let y = HEADER + COLUMN_HEAD;
  const bands: PlateBand[] = keys.map((key, i) => {
    const height = ROW + (spare * shares[i]!) / shareTotal;
    const band = { key, y0: y, y1: y + height, count: counts[i]! };
    y += height;
    return band;
  });

  const groups = modulesOf(scene);
  const widest = groups.map((group) => {
    const perBand = new Map<BandKey, number>();
    for (const member of group.members) perBand.set(keyOf(member.id), (perBand.get(keyOf(member.id)) ?? 0) + 1);
    return Math.max(1, ...perBand.values());
  });
  const weights = widest.map((n) => Math.pow(n, 0.8));
  const total = weights.reduce((sum, w) => sum + w, 0);
  const span = Math.max(1, box.width - LEFT - RIGHT);
  let cursor = LEFT;
  const columns: PlateColumn[] = groups.map((group, i) => {
    const width = (weights[i]! / total) * span;
    const column = { module: group.module, x0: cursor, x1: cursor + width, count: group.members.length };
    cursor += width;
    return column;
  });

  const positions = new Map<string, { x: number; y: number }>();
  const bandOf = new Map<string, PlateBand>();
  const folds: PlateFold[] = [];
  groups.forEach((group, g) => {
    const column = columns[g]!;
    const perRow = Math.max(1, Math.floor((column.x1 - column.x0 - 8) / PITCH));
    for (const band of bands) {
      const members = group.members.filter((member) => keyOf(member.id) === band.key);
      if (members.length === 0) continue;
      const rows = Math.max(1, Math.floor((band.y1 - band.y0 - 6) / ROW));
      const capacity = perRow * rows;
      const shown = members.length > capacity ? capacity - 1 : members.length;
      const rowCount = Math.ceil(Math.max(1, members.length > capacity ? capacity : shown) / perRow);
      const top = band.y0 + (band.y1 - band.y0 - rowCount * ROW) / 2 + ROW * 0.36;
      const slot = (index: number) => {
        const row = Math.floor(index / perRow);
        const inRow = Math.min(perRow, (members.length > capacity ? capacity : shown) - row * perRow);
        const col = index % perRow;
        const cx = (column.x0 + column.x1) / 2;
        return { x: cx + (col - (inRow - 1) / 2) * PITCH, y: top + row * ROW };
      };
      members.slice(0, shown).forEach((member, index) => {
        positions.set(member.id, slot(index));
        bandOf.set(member.id, band);
      });
      if (members.length > shown) folds.push({ ...slot(shown), hidden: members.length - shown });
      for (const member of members.slice(shown)) bandOf.set(member.id, band);
    }
  });
  return {
    width: box.width,
    height: Math.max(box.height, bands.at(-1)!.y1 + BOTTOM),
    strat,
    bands,
    columns,
    positions,
    bandOf,
    folds,
    containsEdges: scene.edges.filter((edge) => !DEPENDENCY_KINDS.has(edge.kind)).length,
  };
}

/** Which way a routed relation runs against the plate's layering. */
export function edgeCourse(
  layout: Pick<PlateLayout, 'strat'>,
  source: string,
  target: string,
): 'descends' | 'level' | 'cycle' | 'climbs' | 'unplaced' {
  const ds = layout.strat.depth.get(source);
  const dt = layout.strat.depth.get(target);
  if (ds == null || dt == null) return 'unplaced';
  const cs = layout.strat.component.get(source);
  if (cs !== undefined && cs === layout.strat.component.get(target)) return 'cycle';
  if (ds > dt) return 'descends';
  if (ds < dt) return 'climbs';
  return 'level';
}

/* ---- paint --------------------------------------------------------------- */

function hash(text: string): number {
  let h = 0;
  for (let i = 0; i < text.length; i += 1) h = (h * 31 + text.charCodeAt(i)) >>> 0;
  return h;
}

function stationRadius(scene: CortexScene, id: string): number {
  return degreeRadius(scene.byId.get(id)?.degree ?? null, scene.maxDegree, { min: 2.6, max: R_MAX });
}

function engrave(ctx: CanvasRenderingContext2D, text: string, x: number, y: number, color: string, align: CanvasTextAlign = 'left') {
  ctx.save();
  ctx.font = monoFont(10, 500);
  ctx.letterSpacing = '0.14em';
  ctx.fillStyle = color;
  ctx.textAlign = align;
  ctx.textBaseline = 'middle';
  ctx.fillText(text, x, y);
  ctx.restore();
}

/** Fit a label to a width: a path keeps its tail, a name keeps its head. */
function fitText(
  ctx: CanvasRenderingContext2D,
  text: string,
  width: number,
  keep: 'head' | 'tail' = 'head',
): string {
  if (ctx.measureText(text).width <= width) return text;
  let cut = text;
  const shown = () => (keep === 'tail' ? `…${cut}` : `${cut}…`);
  while (cut.length > 1 && ctx.measureText(shown()).width > width) {
    cut = keep === 'tail' ? cut.slice(1) : cut.slice(0, -1);
  }
  return shown();
}

function drawFrame(layout: PlateLayout, scene: CortexScene, frame: PaintFrame): void {
  const { ctx, camera, palette } = frame;
  const { strat } = layout;
  const x0 = project(camera, LEFT, 0).x;
  const x1 = project(camera, layout.width - RIGHT, 0).x;

  layout.bands.forEach((band, i) => {
    const top = project(camera, 0, band.y0).y;
    const bottom = project(camera, 0, band.y1).y;
    ctx.save();
    if (band.key === 'unplaced') {
      ctx.beginPath();
      ctx.rect(x0, top, x1 - x0, bottom - top);
      ctx.clip();
      ctx.strokeStyle = palette.unknown;
      ctx.globalAlpha = 0.18;
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (let x = x0 - (bottom - top); x < x1; x += 7) {
        ctx.moveTo(x, bottom);
        ctx.lineTo(x + (bottom - top), top);
      }
      ctx.stroke();
    } else if (i % 2 === 0) {
      ctx.globalAlpha = 0.22;
      ctx.fillStyle = palette.dim;
      ctx.fillRect(x0, top, x1 - x0, bottom - top);
    }
    ctx.restore();
    ctx.save();
    ctx.strokeStyle = palette.dim;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(x0 - 8, Math.round(bottom) + 0.5);
    ctx.lineTo(x1, Math.round(bottom) + 0.5);
    ctx.stroke();
    ctx.restore();
    const mid = (top + bottom) / 2;
    const label = band.key === 'unplaced' ? 'NOT IN SCAN' : `D${band.key}`;
    ctx.save();
    ctx.font = monoFont(band.key === 'unplaced' ? 9 : 12, 600);
    ctx.fillStyle = band.key === 'unplaced' ? palette.unknown : palette.text;
    ctx.textAlign = 'right';
    ctx.textBaseline = 'middle';
    ctx.fillText(label, LEFT - 14, mid - 6);
    ctx.font = monoFont(10);
    ctx.fillStyle = palette.muted;
    ctx.fillText(`${band.count} sym`, LEFT - 14, mid + 8);
    ctx.restore();
  });
  if (layout.bands.length > 0) {
    const axisTop = project(camera, 0, layout.bands[0]!.y0).y;
    const axisBottom = project(camera, 0, layout.bands.at(-1)!.y1).y;
    ctx.save();
    ctx.translate(12, (axisTop + axisBottom) / 2);
    ctx.rotate(-Math.PI / 2);
    engrave(
      ctx,
      strat.basis.kind === 'strata' ? 'FILE DEPTH' : 'CALL DEPTH',
      0,
      0,
      palette.muted,
      'center',
    );
    ctx.restore();
  }

  const headTop = project(camera, 0, HEADER).y;
  const headBottom = project(camera, 0, HEADER + COLUMN_HEAD).y;
  ctx.font = monoFont(10);
  for (const [i, column] of layout.columns.entries()) {
    const cx0 = project(camera, column.x0, 0).x;
    const cx1 = project(camera, column.x1, 0).x;
    if (i > 0) {
      ctx.save();
      ctx.strokeStyle = palette.dim;
      ctx.setLineDash([2, 4]);
      ctx.beginPath();
      ctx.moveTo(Math.round(cx0) + 0.5, headTop + 4);
      ctx.lineTo(Math.round(cx0) + 0.5, project(camera, 0, layout.bands.at(-1)!.y1).y);
      ctx.stroke();
      ctx.restore();
    }
    ctx.save();
    ctx.font = monoFont(10, 500);
    ctx.fillStyle = palette.text;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    const width = cx1 - cx0 - 10;
    const count = ` · ${column.count}`;
    const name = fitText(ctx, column.module, width - ctx.measureText(count).width, 'tail');
    ctx.fillText(`${name}${count}`, (cx0 + cx1) / 2, headTop + 10);
    ctx.restore();
  }
  ctx.save();
  ctx.strokeStyle = palette.edge;
  ctx.globalAlpha = 0.6;
  ctx.beginPath();
  ctx.moveTo(x0 - 8, Math.round(headBottom) + 0.5);
  ctx.lineTo(x1, Math.round(headBottom) + 0.5);
  ctx.stroke();
  ctx.restore();

  const basis = strat.basis;
  const climbs = scene.edges.filter((edge) => {
    const course = edgeCourse(layout, edge.source, edge.target);
    return DEPENDENCY_KINDS.has(edge.kind) && (course === 'climbs' || course === 'cycle');
  }).length;
  const title =
    basis.kind === 'strata'
      ? `STRATA · INDEX FILE DEPTH · ${basis.placedFiles} OF ${basis.drawnFiles} DRAWN FILES IN SCAN`
      : 'STRATA · CALL DEPTH WITHIN THE DRAWN SLICE';
  engrave(ctx, title, 10, 12, palette.text);
  const detail =
    basis.kind === 'strata'
      ? `longest-path layering, generation ${basis.generation}`
      : `${basis.why} · longest path over ${basis.dependencyEdges} dependency relations, ${basis.cycles} ${basis.cycles === 1 ? 'cycle' : 'cycles'} condensed`;
  ctx.save();
  ctx.font = monoFont(10);
  ctx.fillStyle = palette.muted;
  ctx.textBaseline = 'middle';
  const right = `${climbs} ${basis.kind === 'strata' ? 'climbing or cyclic' : 'cyclic'} · ${layout.containsEdges} containment not routed`;
  // The field's camera controls sit over the header's right end.
  const clear = layout.width - CAMERA_CLEARANCE;
  const rightWidth = ctx.measureText(right).width;
  ctx.fillText(fitText(ctx, detail, clear - 10, 'head'), 10, 27);
  ctx.fillStyle = climbs > 0 ? palette.danger : palette.muted;
  ctx.fillText(right, Math.max(10 + 360, clear - rightWidth), 12);
  ctx.restore();
}

function drawRoutes(layout: PlateLayout, scene: CortexScene, frame: PaintFrame): void {
  const { ctx, camera, palette, emphasis } = frame;
  ctx.save();
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
  for (const edge of scene.edges) {
    if (!DEPENDENCY_KINDS.has(edge.kind)) continue;
    const a = layout.positions.get(edge.source);
    const b = layout.positions.get(edge.target);
    if (!a || !b) continue;
    const course = edgeCourse(layout, edge.source, edge.target);
    const lit = emphasis !== null && emphasis.has(edge.source) && emphasis.has(edge.target);
    const dimmed = emphasis !== null && !lit;
    const alarm = course === 'climbs' || course === 'cycle';
    ctx.strokeStyle = alarm ? palette.danger : lit ? palette.text : palette.edge;
    ctx.globalAlpha = dimmed ? 0.07 : lit ? 0.95 : alarm ? 0.8 : 0.42;
    ctx.lineWidth = lit ? 1.6 : 1;
    ctx.setLineDash(edge.kind === 'references' ? [5, 3] : []);
    const pa = project(camera, a.x, a.y);
    const pb = project(camera, b.x, b.y);
    const lane = ((hash(`${edge.source}>${edge.target}`) % 5) - 2) * 2.2;
    ctx.beginPath();
    ctx.moveTo(pa.x, pa.y);
    if (course === 'level' || course === 'cycle' || Math.abs(pa.y - pb.y) < 1) {
      const lift = 12 + Math.min(18, Math.abs(pa.x - pb.x) * 0.08);
      ctx.bezierCurveTo(pa.x, pa.y - lift, pb.x, pb.y - lift, pb.x, pb.y);
    } else {
      const band = layout.bandOf.get(edge.source)!;
      const down = pb.y > pa.y;
      const channel = project(camera, 0, down ? band.y1 : band.y0).y + (down ? -5 : 5) + lane;
      const radius = 5;
      ctx.arcTo(pa.x, channel, pb.x, channel, radius);
      ctx.arcTo(pb.x, channel, pb.x, pb.y, radius);
      ctx.lineTo(pb.x, pb.y);
    }
    ctx.stroke();
  }
  ctx.restore();
}

function drawStations(layout: PlateLayout, scene: CortexScene, frame: PaintFrame): void {
  const { ctx, camera, palette, emphasis } = frame;
  const obstacles: LabelCandidate[] = [];
  const candidates: { box: LabelCandidate; priority: number; dimmed: boolean; text: string }[] = [];
  ctx.font = monoFont(10);
  for (const node of scene.nodes) {
    const world = layout.positions.get(node.id);
    if (!world) continue;
    const p = project(camera, world.x, world.y);
    const r = stationRadius(scene, node.id);
    const dimmed = emphasis !== null && !emphasis.has(node.id);
    // A station is a ringed stop: substrate knockout under the kind body.
    ctx.save();
    ctx.globalAlpha = dimmed ? 0.3 : 1;
    ctx.fillStyle = palette.substrate;
    ctx.beginPath();
    ctx.arc(p.x, p.y, r + 2, 0, Math.PI * 2);
    ctx.fill();
    ctx.restore();
    drawSymbolBody(ctx, node, p.x, p.y, r, { alpha: dimmed ? 0.25 : 1, palette });
    drawStateMarks(
      ctx,
      p.x,
      p.y,
      r,
      {
        selected: frame.selected === node.id,
        hovered: frame.hovered === node.id,
        cursor: frame.cursor === node.id,
        heat: frame.heat(node.id),
      },
      palette,
    );
    obstacles.push({ id: node.id, x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r });
    const marked = frame.selected === node.id || frame.hovered === node.id || frame.cursor === node.id;
    const maxWidth = PITCH * Math.sqrt(camera.k / frame.fitK) * 1.6;
    const text = marked ? node.label : fitText(ctx, node.label, maxWidth);
    const width = ctx.measureText(text).width;
    candidates.push({
      box: { id: node.id, x: p.x - width / 2, y: p.y + r + 3, width, height: 12 },
      priority: (marked ? 1e6 : 0) + (emphasis?.has(node.id) ? 1e4 : 0) + (node.degree ?? 0),
      dimmed,
      text,
    });
  }
  for (const fold of layout.folds) {
    const p = project(camera, fold.x, fold.y);
    const text = `+${fold.hidden}`;
    ctx.save();
    ctx.font = monoFont(10, 600);
    const width = ctx.measureText(text).width + 8;
    ctx.fillStyle = palette.substrate;
    ctx.fillRect(p.x - width / 2, p.y - 8, width, 16);
    ctx.strokeStyle = palette.edge;
    ctx.strokeRect(p.x - width / 2 + 0.5, p.y - 7.5, width - 1, 15);
    ctx.fillStyle = palette.text;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(text, p.x, p.y);
    ctx.restore();
  }
  candidates.sort((a, b) => b.priority - a.priority);
  const kept = new Set(placeLabels(candidates.map((c) => [c.box]), obstacles, 1).map((box) => box.id));
  for (const { box, dimmed, text } of candidates) {
    if (!kept.has(box.id)) continue;
    const strong = frame.selected === box.id || frame.hovered === box.id || frame.cursor === box.id;
    drawHaloLabel(ctx, text, box.x, box.y + 6, {
      font: monoFont(10, strong ? 600 : 400),
      color: palette.text,
      halo: palette.substrate,
      alpha: dimmed ? 0.3 : strong ? 1 : 0.8,
    });
  }
}

export function platePainter(strata: StrataInput | undefined): CortexPainter<PlateLayout> {
  return {
    name: 'stratified plate',
    relayoutOnResize: true,
    fitPad: 0,
    layout: (scene, box) => plateLayout(scene, box, strata),
    bounds: (layout) => ({ x0: 0, y0: 0, x1: layout.width, y1: layout.height }),
    position: (layout, id) => layout.positions.get(id) ?? null,
    hitRadius: (_layout, scene, id) => stationRadius(scene, id),
    draw(layout, scene, frame) {
      drawFrame(layout, scene, frame);
      drawRoutes(layout, scene, frame);
      drawStations(layout, scene, frame);
    },
  };
}
