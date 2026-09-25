/**
 * PROVENANCE CAMERAS: the fact scene as one camera frame per category, laid
 * out in screen pixels inside the height the Facts column grants it.
 *
 * Three modes, chosen by what fits:
 *
 *   rows       every frame shows its rows (trust descending, at most
 *              `CAMERA_ROW_CAP`), each row a label and a trust rail.
 *   aggregate  the rows do not fit, so each frame prints its exact count and
 *              draws every fact as one tick on its trust rail; a frame is
 *              opened to read its rows.
 *   open       one opened frame spans the field and lays its rows out in as
 *              many columns as the width holds; what the height cannot hold
 *              is counted and left to the ledger.
 *
 * Every rail in every mode is the same scale anchored at 0, so a rail compares
 * across frames without reading the printed value. Relations are routed from
 * row glyph to row glyph through the gutters, never across a label or rail; a
 * relation with an end that is not drawn is counted, not drawn to nothing.
 *
 * Pure: the same scene, box and open frame yield the same layout.
 */
import { disputesOf, type FactScene, type SceneFact, type SceneRelation } from './factScene.ts';

export const CAMERA_ROW_H = 20;
export const CAMERA_HEADER_H = 22;
export const CAMERA_AGGREGATE_H = 54;
/** The aggregate grid and an opened frame fill a budgeted box, so they
 * spend less on vertical margins than the free-standing rows grid. */
const TIGHT_PAD_Y = 6;
/** Space under a frame's last row. */
const FRAME_FOOT = 4;
export const CAMERA_ROW_CAP = 8;
const FRAME_GAP = 18;
const PAD = 12;
const MIN_FRAME_W = 260;
const MIN_AGGREGATE_W = 116;
const VALUE_W = 40;
const GLYPH_X = 12;
export const LABEL_X = 24;

export type CameraMode = 'rows' | 'aggregate' | 'open';

/** A frame's identity: its category, `null` for facts that carry none. */
export type FrameKey = string | null;

export interface CameraRow {
  readonly fact: SceneFact;
  /** Row box, absolute. */
  readonly x: number;
  readonly top: number;
  readonly w: number;
  /** Glyph centre, absolute. */
  readonly gx: number;
  readonly gy: number;
}

export interface CameraTick {
  readonly fact: SceneFact;
  /** Tick position on the frame's rail; `null` when trust is absent. */
  readonly x: number | null;
  /** The fact is an end of a contradiction or supersession. */
  readonly disputed: boolean;
}

export interface CameraFrame {
  readonly key: FrameKey;
  /** The printed category, `category absent` where the facts carry none. */
  readonly title: string;
  readonly count: number;
  readonly x: number;
  readonly y: number;
  readonly w: number;
  readonly h: number;
  readonly rows: readonly CameraRow[];
  /** Facts in this frame not drawn as rows, whose exact rows the ledger holds. */
  readonly overflow: number;
  /** Where the frame prints its overflow count, in rows and open modes. */
  readonly overflowAt: { readonly x: number; readonly y: number } | null;
  /** Aggregate mode: the frame's rail, absolute, and one tick per fact. */
  readonly rail: { readonly x: number; readonly w: number; readonly y: number } | null;
  readonly ticks: readonly CameraTick[];
  readonly trustRange: readonly [number, number] | null;
  readonly trustAbsent: number;
  readonly withheld: number;
  readonly disputes: number;
  /** Most cited entity names in the frame, by count then name. */
  readonly cites: readonly { label: string; count: number }[];
}

export interface CameraRelation {
  readonly relation: SceneRelation;
  readonly d: string;
  readonly midX: number;
  readonly midY: number;
  /** A same-column arc runs vertically in the glyph gutter; its label follows it. */
  readonly vertical: boolean;
}

/** A relation with only one end drawn as a row in this mode. */
export interface CameraStub {
  readonly relation: SceneRelation;
  /** The drawn end. */
  readonly node: string;
  /** The frame the other end belongs to; `null` when the payload did not include it. */
  readonly other: { readonly key: FrameKey } | null;
  readonly d: string;
}

export interface CameraLayout {
  readonly mode: CameraMode;
  readonly width: number;
  /** Content height; greater than the box only when even the aggregate grid overflows it. */
  readonly height: number;
  readonly frames: readonly CameraFrame[];
  /** Row rail geometry, relative to a row's left edge, shared by every row. */
  readonly rail: { readonly x: number; readonly w: number };
  readonly labelW: number;
  readonly relations: readonly CameraRelation[];
  readonly stubs: readonly CameraStub[];
  /** Relations with an end that is not drawn as a row in this mode. */
  readonly relationsOffField: number;
}

export function layoutCameras(
  scene: FactScene,
  box: { width: number; height: number },
  open: { key: FrameKey } | null,
): CameraLayout {
  const groups = new Map<FrameKey, SceneFact[]>();
  for (const fact of scene.facts) {
    const bucket = groups.get(fact.category) ?? [];
    bucket.push(fact);
    groups.set(fact.category, bucket);
  }
  // A frame holding withheld facts first, so a typed absence is never below
  // the fold; then largest first, with the frame of facts with no category
  // last among the rest.
  const withheld = (facts: readonly SceneFact[]) => facts.some((fact) => fact.restricted);
  const ordered = [...groups.entries()]
    .sort(
      (a, b) =>
        Number(withheld(b[1])) - Number(withheld(a[1])) ||
        Number(a[0] === null) - Number(b[0] === null) ||
        b[1].length - a[1].length ||
        (a[0] ?? '').localeCompare(b[0] ?? ''),
    )
    .map(([key, facts]) => ({
      key,
      facts: [...facts].sort((a, b) => (b.trust ?? -1) - (a.trust ?? -1) || a.factId.localeCompare(b.factId)),
    }));
  const usable = Math.max(MIN_AGGREGATE_W, box.width - PAD * 2);
  const columnsFor = (min: number) =>
    Math.max(1, Math.min(ordered.length || 1, Math.floor((usable + FRAME_GAP) / (min + FRAME_GAP))));
  const rowColumns = columnsFor(MIN_FRAME_W);
  const rowFrameW = Math.floor((usable - (rowColumns - 1) * FRAME_GAP) / rowColumns);
  const railW = Math.round(Math.max(48, Math.min(80, rowFrameW * 0.2)));
  const railX = rowFrameW - VALUE_W - 8 - railW;
  const labelW = railX - LABEL_X - 10;
  const context = frameContext(scene);

  const openGroup = open ? ordered.find((group) => group.key === open.key) : undefined;
  if (openGroup) {
    return openLayout(scene, openGroup, box, usable, context);
  }

  const rowsHeight = bandHeights(ordered.map((group) => rowFrameHeight(group.facts.length)), rowColumns);
  if (rowsHeight <= box.height) {
    const frames: CameraFrame[] = [];
    let top = PAD;
    for (let start = 0; start < ordered.length; start += rowColumns) {
      const band = ordered.slice(start, start + rowColumns);
      const bandH = Math.max(...band.map((group) => rowFrameHeight(group.facts.length)));
      band.forEach((group, index) => {
        const x = PAD + index * (rowFrameW + FRAME_GAP);
        const shown = group.facts.slice(0, CAMERA_ROW_CAP);
        const overflowTop = top + CAMERA_HEADER_H + shown.length * CAMERA_ROW_H;
        frames.push({
          ...summary(group.key, group.facts, context),
          x,
          y: top,
          w: rowFrameW,
          h: bandH,
          rows: shown.map((fact, row) => placeRow(fact, x, top + CAMERA_HEADER_H + row * CAMERA_ROW_H, rowFrameW)),
          overflow: group.facts.length - shown.length,
          overflowAt: group.facts.length > shown.length ? { x: x + LABEL_X, y: overflowTop + 14 } : null,
          rail: null,
          ticks: [],
        });
      });
      top += bandH + FRAME_GAP;
    }
    const { relations, stubs, relationsOffField } = routeAll(scene, frames, (a, b) =>
      a.frame.y === b.frame.y
        ? a.frame.y - FRAME_GAP / 2
        : a.frame.y < b.frame.y
          ? a.frame.y + a.frame.h + FRAME_GAP / 2
          : a.frame.y - FRAME_GAP / 2,
    );
    return {
      mode: 'rows',
      width: box.width,
      height: rowsHeight,
      frames,
      rail: { x: railX, w: railW },
      labelW,
      relations,
      stubs,
      relationsOffField,
    };
  }

  const aggColumns = columnsFor(MIN_AGGREGATE_W);
  const aggW = Math.floor((usable - (aggColumns - 1) * FRAME_GAP) / aggColumns);
  const frames: CameraFrame[] = ordered.map((group, index) => {
    const x = PAD + (index % aggColumns) * (aggW + FRAME_GAP);
    const y = TIGHT_PAD_Y + Math.floor(index / aggColumns) * (CAMERA_AGGREGATE_H + FRAME_GAP);
    // The rail starts past a gutter where facts with no trust are marked.
    const rail = { x: x + 18, w: aggW - 28, y: y + 27 };
    return {
      ...summary(group.key, group.facts, context),
      x,
      y,
      w: aggW,
      h: CAMERA_AGGREGATE_H,
      rows: [],
      overflow: group.facts.length,
      overflowAt: null,
      rail,
      ticks: group.facts.map((fact) => ({
        fact,
        x: fact.trust == null ? null : round(rail.x + clamp01(fact.trust) * rail.w),
        disputed: context.disputed.has(fact.nodeId),
      })),
    };
  });
  return {
    mode: 'aggregate',
    width: box.width,
    height: aggregateHeight(ordered.length, aggColumns),
    frames,
    rail: { x: railX, w: railW },
    labelW,
    relations: [],
    stubs: [],
    relationsOffField: scene.relations.length,
  };
}

interface FrameContext {
  entityLabel: ReadonlyMap<string, string>;
  disputed: ReadonlyMap<string, number>;
}

function frameContext(scene: FactScene): FrameContext {
  const disputed = new Map<string, number>();
  for (const relation of disputesOf(scene)) {
    for (const end of new Set([relation.source, relation.target])) disputed.set(end, (disputed.get(end) ?? 0) + 1);
  }
  return { entityLabel: new Map(scene.entities.map((entity) => [entity.nodeId, entity.label])), disputed };
}

/** What a frame prints about its facts whichever mode draws it. */
function summary(key: FrameKey, facts: readonly SceneFact[], context: FrameContext) {
  const measured = facts.flatMap((fact) => (fact.trust == null ? [] : [fact.trust]));
  const cites = new Map<string, number>();
  for (const fact of facts) {
    for (const id of fact.entityIds) {
      const label = context.entityLabel.get(id) ?? id;
      cites.set(label, (cites.get(label) ?? 0) + 1);
    }
  }
  const ids = new Set(facts.map((fact) => fact.nodeId));
  let disputes = 0;
  for (const [node, count] of context.disputed) if (ids.has(node)) disputes += count;
  return {
    key,
    title: key ?? 'category absent',
    count: facts.length,
    trustRange: measured.length ? ([Math.min(...measured), Math.max(...measured)] as const) : null,
    trustAbsent: facts.length - measured.length,
    withheld: facts.filter((fact) => fact.restricted).length,
    disputes,
    cites: [...cites.entries()]
      .map(([label, count]) => ({ label, count }))
      .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label))
      .slice(0, 3),
  };
}

function openLayout(
  scene: FactScene,
  group: { key: FrameKey; facts: readonly SceneFact[] },
  box: { width: number; height: number },
  usable: number,
  context: FrameContext,
): CameraLayout {
  const columns = Math.max(1, Math.floor((usable - 24 + FRAME_GAP) / (MIN_FRAME_W + FRAME_GAP)));
  const columnW = Math.floor((usable - 24 - (columns - 1) * FRAME_GAP) / columns);
  const perColumn = Math.max(1, Math.floor((box.height - TIGHT_PAD_Y * 2 - CAMERA_HEADER_H - FRAME_FOOT) / CAMERA_ROW_H));
  const capacity = columns * perColumn;
  const shown = group.facts.length > capacity ? group.facts.slice(0, capacity - 1) : group.facts;
  const overflow = group.facts.length - shown.length;
  const slots = shown.length + (overflow > 0 ? 1 : 0);
  const frameH = CAMERA_HEADER_H + Math.min(perColumn, Math.max(1, slots)) * CAMERA_ROW_H + FRAME_FOOT;
  const x = PAD;
  const y = TIGHT_PAD_Y;
  const slot = (index: number) => ({
    x: x + 12 + Math.floor(index / perColumn) * (columnW + FRAME_GAP),
    top: y + CAMERA_HEADER_H + (index % perColumn) * CAMERA_ROW_H,
  });
  const frame: CameraFrame = {
    ...summary(group.key, group.facts, context),
    x,
    y,
    w: usable,
    h: frameH,
    rows: shown.map((fact, index) => placeRow(fact, slot(index).x, slot(index).top, columnW)),
    overflow,
    overflowAt: overflow > 0 ? { x: slot(shown.length).x + LABEL_X, y: slot(shown.length).top + 14 } : null,
    rail: null,
    ticks: [],
  };
  const bottom = y + frameH + TIGHT_PAD_Y / 2;
  const { relations, stubs, relationsOffField } = routeAll(scene, [frame], () => bottom);
  const railW = Math.round(Math.max(48, Math.min(80, columnW * 0.2)));
  const railX = columnW - VALUE_W - 8 - railW;
  return {
    mode: 'open',
    width: box.width,
    height: y + frameH + TIGHT_PAD_Y,
    frames: [frame],
    rail: { x: railX, w: railW },
    labelW: railX - LABEL_X - 10,
    relations,
    stubs,
    relationsOffField,
  };
}

function placeRow(fact: SceneFact, x: number, top: number, w: number): CameraRow {
  return { fact, x, top, w, gx: x + GLYPH_X, gy: top + CAMERA_ROW_H / 2 };
}

function rowFrameHeight(count: number): number {
  const rows = Math.min(count, CAMERA_ROW_CAP) + (count > CAMERA_ROW_CAP ? 1 : 0);
  return CAMERA_HEADER_H + rows * CAMERA_ROW_H + FRAME_FOOT;
}

function aggregateHeight(frames: number, columns: number): number {
  const bands = Math.max(1, Math.ceil(frames / columns));
  return TIGHT_PAD_Y * 2 + bands * CAMERA_AGGREGATE_H + (bands - 1) * FRAME_GAP;
}

function bandHeights(heights: readonly number[], columns: number): number {
  let total = PAD * 2;
  for (let start = 0; start < heights.length; start += columns) {
    total += Math.max(...heights.slice(start, start + columns)) + (start > 0 ? FRAME_GAP : 0);
  }
  return total;
}

function routeAll(
  scene: FactScene,
  frames: readonly CameraFrame[],
  routeY: (a: { frame: CameraFrame }, b: { frame: CameraFrame }) => number,
) {
  const at = new Map<string, { row: CameraRow; frame: CameraFrame }>();
  for (const frame of frames) for (const row of frame.rows) at.set(row.fact.nodeId, { row, frame });
  const relations: CameraRelation[] = [];
  const stubs: CameraStub[] = [];
  let relationsOffField = 0;
  for (const relation of scene.relations) {
    const a = at.get(relation.source);
    const b = at.get(relation.target);
    if (!a || !b) {
      relationsOffField += 1;
      // One end drawn: a stub from its glyph to the column edge, so the
      // relation is still visible, and liftable, where its drawn end sits.
      const end = a ?? b;
      const other = scene.byNode.get(a ? relation.target : relation.source);
      if (end) {
        stubs.push({
          relation,
          node: end.row.fact.nodeId,
          other: other ? { key: other.category } : null,
          d: `M ${end.row.gx} ${end.row.gy} H ${end.row.x + 2}`,
        });
      }
      continue;
    }
    relations.push(route(relation, a, b, relations.length, routeY(a, b)));
  }
  return { relations, stubs, relationsOffField };
}

/** A relation leaves its row glyph to the left and re-enters the other the
 * same way, so a line never crosses a label or rail. Within one column it is
 * an arc in the glyph gutter; between columns it runs the gutters
 * orthogonally, with a small lane offset so parallel runs stay apart. */
function route(
  relation: SceneRelation,
  a: { row: CameraRow },
  b: { row: CameraRow },
  index: number,
  routeY: number,
): CameraRelation {
  if (a.row.x === b.row.x) {
    const cx = a.row.gx - GLYPH_X - 4 - Math.min(6, Math.abs(b.row.gy - a.row.gy) * 0.05);
    return {
      relation,
      d: `M ${a.row.gx} ${a.row.gy} C ${round(cx)} ${a.row.gy}, ${round(cx)} ${b.row.gy}, ${b.row.gx} ${b.row.gy}`,
      midX: round(cx),
      midY: round((a.row.gy + b.row.gy) / 2),
      vertical: true,
    };
  }
  const lane = ((index % 3) - 1) * 2.5;
  const xa = a.row.x - FRAME_GAP / 2 + lane;
  const xb = b.row.x - FRAME_GAP / 2 + lane;
  const y = routeY + lane;
  return {
    relation,
    d: `M ${a.row.gx} ${a.row.gy} H ${round(xa)} V ${round(y)} H ${round(xb)} V ${b.row.gy} H ${b.row.gx}`,
    midX: round((xa + xb) / 2),
    midY: round(y),
    vertical: false,
  };
}

function clamp01(value: number): number {
  return Math.max(0, Math.min(1, value));
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}
