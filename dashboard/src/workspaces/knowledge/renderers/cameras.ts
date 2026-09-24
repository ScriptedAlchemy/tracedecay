/**
 * PROVENANCE CAMERAS: the fact scene as bounded rows inside one camera frame
 * per category, each row carrying its trust rail.
 *
 * Every rail in every frame is the same length and anchored at 0, so a rail
 * compares across frames without reading the printed value. Rows inside a
 * frame are trust descending; a frame holds at most `ROW_CAP` rows and names
 * how many more it has, whose exact rows the ledger carries. Relations run
 * between the row glyphs, and a relation that reaches a row past a frame's cap
 * is counted, not drawn to nothing.
 *
 * Layout is in screen pixels for the measured width. Pure.
 */
import type { FactScene, SceneFact, SceneRelation } from './factScene.ts';

export const CAMERA_ROW_H = 16;
export const CAMERA_HEADER_H = 22;
const FRAME_GAP = 18;
const PAD = 12;
export const CAMERA_ROW_CAP = 8;
const MIN_FRAME_W = 236;
const VALUE_W = 38;
const GLYPH_X = 12;
const LABEL_X = 24;

export interface CameraRow {
  readonly fact: SceneFact;
  /** Row top, absolute. */
  readonly top: number;
  /** Glyph centre, absolute. */
  readonly gx: number;
  readonly gy: number;
}

export interface CameraFrame {
  readonly category: string | null;
  /** The printed category, `category absent` where the fact carries none. */
  readonly title: string;
  readonly count: number;
  readonly x: number;
  readonly y: number;
  readonly w: number;
  readonly h: number;
  readonly rows: readonly CameraRow[];
  readonly overflow: number;
  /** Most cited entity names in the frame, by count then name. */
  readonly cites: readonly { label: string; count: number }[];
}

export interface CameraRelation {
  readonly relation: SceneRelation;
  readonly d: string;
  readonly midX: number;
  readonly midY: number;
  /** A same-frame arc runs vertically in the glyph gutter; its label follows it. */
  readonly vertical: boolean;
}

export interface CameraLayout {
  readonly width: number;
  readonly height: number;
  readonly frames: readonly CameraFrame[];
  /** Rail geometry, relative to a frame's left edge, shared by every frame. */
  readonly rail: { readonly x: number; readonly w: number };
  readonly labelW: number;
  readonly relations: readonly CameraRelation[];
  /** Relations with an end on a row past a frame's cap. */
  readonly relationsPastCap: number;
}

export function layoutCameras(scene: FactScene, width: number): CameraLayout {
  const groups = new Map<string | null, SceneFact[]>();
  for (const fact of scene.facts) {
    const bucket = groups.get(fact.category) ?? [];
    bucket.push(fact);
    groups.set(fact.category, bucket);
  }
  // Largest frame first; the frame of facts with no category always last.
  const ordered = [...groups.entries()].sort(
    (a, b) =>
      Number(a[0] === null) - Number(b[0] === null) ||
      b[1].length - a[1].length ||
      (a[0] ?? '').localeCompare(b[0] ?? ''),
  );
  const usable = Math.max(MIN_FRAME_W, width - PAD * 2);
  const columns = Math.max(
    1,
    Math.min(ordered.length || 1, Math.floor((usable + FRAME_GAP) / (MIN_FRAME_W + FRAME_GAP))),
  );
  const frameW = Math.floor((usable - (columns - 1) * FRAME_GAP) / columns);
  const railW = Math.round(Math.max(48, Math.min(80, frameW * 0.2)));
  const railX = frameW - VALUE_W - 8 - railW;
  const labelW = railX - LABEL_X - 10;
  const entityLabel = new Map(scene.entities.map((entity) => [entity.nodeId, entity.label]));

  const frames: CameraFrame[] = [];
  let top = PAD;
  for (let start = 0; start < ordered.length; start += columns) {
    const band = ordered.slice(start, start + columns);
    const heights = band.map(([, facts]) => frameHeight(facts.length));
    const bandH = Math.max(...heights);
    band.forEach(([key, facts], index) => {
      const x = PAD + index * (frameW + FRAME_GAP);
      const sorted = [...facts].sort(
        (a, b) => (b.trust ?? -1) - (a.trust ?? -1) || a.factId.localeCompare(b.factId),
      );
      const shown = sorted.slice(0, CAMERA_ROW_CAP);
      const cites = new Map<string, number>();
      for (const fact of facts) {
        for (const id of fact.entityIds) {
          const label = entityLabel.get(id) ?? id;
          cites.set(label, (cites.get(label) ?? 0) + 1);
        }
      }
      frames.push({
        category: key,
        title: key ?? 'category absent',
        count: facts.length,
        x,
        y: top,
        w: frameW,
        h: bandH,
        rows: shown.map((fact, row) => {
          const rowTop = top + CAMERA_HEADER_H + row * CAMERA_ROW_H;
          return { fact, top: rowTop, gx: x + GLYPH_X, gy: rowTop + CAMERA_ROW_H / 2 };
        }),
        overflow: facts.length - shown.length,
        cites: [...cites.entries()]
          .map(([label, count]) => ({ label, count }))
          .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label))
          .slice(0, 3),
      });
    });
    top += bandH + FRAME_GAP;
  }

  const glyph = new Map<string, { row: CameraRow; frame: CameraFrame }>();
  for (const frame of frames) for (const row of frame.rows) glyph.set(row.fact.nodeId, { row, frame });
  const relations: CameraRelation[] = [];
  let relationsPastCap = 0;
  for (const relation of scene.relations) {
    const a = glyph.get(relation.source);
    const b = glyph.get(relation.target);
    if (!a || !b) {
      relationsPastCap += 1;
      continue;
    }
    relations.push(route(relation, a, b, relations.length));
  }

  return {
    width,
    height: Math.max(top - FRAME_GAP + PAD, PAD * 2),
    frames,
    rail: { x: railX, w: railW },
    labelW,
    relations,
    relationsPastCap,
  };
}

function frameHeight(count: number): number {
  const rows = Math.min(count, CAMERA_ROW_CAP) + (count > CAMERA_ROW_CAP ? 1 : 0);
  return CAMERA_HEADER_H + rows * CAMERA_ROW_H + 6;
}

/** A relation leaves its row glyph to the left and re-enters the other the
 * same way, so a line never crosses a row's printed label or rail. Within one
 * frame it is an arc in the glyph gutter; between frames it runs the gutters
 * between frames, orthogonally, with a small lane offset so parallel runs stay
 * apart. The label sits on the horizontal gutter run. */
function route(
  relation: SceneRelation,
  a: { row: CameraRow; frame: CameraFrame },
  b: { row: CameraRow; frame: CameraFrame },
  index: number,
): CameraRelation {
  if (a.frame === b.frame) {
    const cx = a.row.gx - GLYPH_X - 4 - Math.min(6, Math.abs(b.row.gy - a.row.gy) * 0.05);
    return {
      relation,
      d: `M ${a.row.gx} ${a.row.gy} C ${cx} ${a.row.gy}, ${cx} ${b.row.gy}, ${b.row.gx} ${b.row.gy}`,
      midX: round(cx),
      midY: round((a.row.gy + b.row.gy) / 2),
      vertical: true,
    };
  }
  const lane = ((index % 3) - 1) * 2.5;
  const xa = a.frame.x - FRAME_GAP / 2 + lane;
  const xb = b.frame.x - FRAME_GAP / 2 + lane;
  const yRoute =
    a.frame.y === b.frame.y
      ? a.frame.y - FRAME_GAP / 2 + lane
      : a.frame.y < b.frame.y
        ? a.frame.y + a.frame.h + FRAME_GAP / 2 + lane
        : a.frame.y - FRAME_GAP / 2 + lane;
  return {
    relation,
    d: `M ${a.row.gx} ${a.row.gy} H ${round(xa)} V ${round(yRoute)} H ${round(xb)} V ${b.row.gy} H ${b.row.gx}`,
    midX: round((xa + xb) / 2),
    midY: round(yRoute),
    vertical: false,
  };
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}
