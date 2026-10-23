import { formatMoment } from '../../../workspaces/loom/tracks.ts';
import { xToTime } from '../layout.ts';
import type { ScenePath } from '../types.ts';
import { isLifted, isLink, pathEnds, type SceneFrame } from './frame.ts';

interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface LinkTag extends Bounds {
  path: ScenePath;
}

export interface SceneAnnotations {
  links: readonly ScenePath[];
  tally: string;
  tallyInRuler: boolean;
  tags: readonly LinkTag[];
  cursorText: string | null;
  cursorLabel: { x: number; y: number; anchor: 'start' | 'end' } | null;
}

const TAGGED_LINKS_MAX = 16;
const TAG_HEIGHT = 13;
// Mono advance plus the printed letter spacing; reserve a little extra room.
const TAG_CHAR_PX = 7.2;

function intersects(a: Bounds, b: Bounds): boolean {
  return a.x < b.x + b.width + 3 && a.x + a.width + 3 > b.x &&
    a.y < b.y + b.height + 3 && a.y + a.height + 3 > b.y;
}

export function linkKind(path: ScenePath): 'JOINS' | 'FORKS' {
  return path.kind === 'rejoin' || path.kind === 'result' ? 'JOINS' : 'FORKS';
}

/** Labels occupy space, while the evidence is never culled with them. The
 * wrapping readout carries a tally that cannot share the ruler with NOW, and
 * the relation disclosure retains every link whose inline tag cannot fit. */
export function sceneAnnotations(frame: SceneFrame): SceneAnnotations {
  const { model, fieldX0, fieldX1, top, height } = frame;
  const links = model.paths.filter(isLink);
  const counts = new Map<string, number>();
  for (const path of links) {
    const key = `${linkKind(path)} ${path.grade.toUpperCase()}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  const tally = [...counts.entries()].map(([key, count]) => `${count} ${key}`).join(' · ');
  const tailX = frame.density?.tailX ?? null;
  const tailLater = tailX === null && frame.density?.tailTime != null && frame.density.tailTime > model.viewport.window.end;
  const tallyRight = tailX !== null
    ? tailX > fieldX1 - 30 ? fieldX1 - 40 : tailX - 24
    : tailLater ? fieldX1 - 58 : fieldX1 - 4;
  const tallyInRuler = tally.length * 6.7 <= tallyRight - fieldX0 - 8;

  const cursor = model.cursor;
  const cursorVisible = cursor !== null && cursor.x >= fieldX0 && cursor.x <= fieldX1;
  const cursorText = !cursorVisible ? null : cursor.xBasis === 'time'
    ? `CURSOR ${formatMoment(xToTime(model.viewport, cursor.x))}`
    : 'CURSOR · RECORDED ORDER';
  const cursorLane = cursor === null ? undefined : model.lanes.find((lane) => lane.id === cursor.laneId);
  const cursorWidth = (cursorText?.length ?? 0) * TAG_CHAR_PX;
  const cursorY = cursor?.xBasis !== 'sequence' ? top + 11 : cursor.y !== undefined ? cursor.y + 10
    : cursorLane ? cursorLane.y + cursorLane.height / 2 - 4 : top + 11;
  const cursorLabel = !cursorVisible || !cursorText ? null
    : fieldX1 - cursor.x >= cursorWidth + 8
      ? { x: cursor.x + 5, y: cursorY, anchor: 'start' as const }
      : cursor.x - fieldX0 >= cursorWidth + 8
        ? { x: cursor.x - 5, y: cursorY, anchor: 'end' as const }
        : null;

  const occupied: Bounds[] = [
    ...model.nodes.map((node) => ({ x: node.x - 9, y: node.y - 9, width: 18, height: 18 })),
    ...model.labels.filter((label) => label.group !== 'lane').map((label) => {
      const width = label.text.length * 6.2;
      return { x: label.x - (label.anchor === 'middle' ? width / 2 : label.anchor === 'end' ? width : 0), y: label.y - 10, width, height: 13 };
    }),
  ];
  if (cursorLabel) occupied.push({
    x: cursorLabel.x - (cursorLabel.anchor === 'end' ? cursorWidth : 0),
    y: cursorLabel.y - 10,
    width: cursorWidth,
    height: 13,
  });
  const tags: LinkTag[] = [];
  const ordered = [...links].sort((a, b) => Number(isLifted(b.focus)) - Number(isLifted(a.focus)));
  for (const path of ordered) {
    const { x0, y0, x1, y1 } = pathEnds(path.controls);
    if (Math.abs(y1 - y0) < 20) continue;
    if (!isLifted(path.focus) && (path.grade === 'exact' || links.length > TAGGED_LINKS_MAX)) continue;
    const width = path.grade.length * TAG_CHAR_PX;
    const tag = { path, x: (x0 + x1) / 2 - 4 - width, y: (y0 + y1) / 2 - 7, width, height: TAG_HEIGHT };
    if (tag.x < fieldX0 + 4 || tag.x + width > fieldX1 - 4 || tag.y < top + 3 || tag.y + tag.height > height) continue;
    if (occupied.some((bounds) => intersects(tag, bounds))) continue;
    occupied.push(tag);
    tags.push(tag);
  }
  tags.sort((a, b) => links.indexOf(a.path) - links.indexOf(b.path));
  return { links, tally, tallyInRuler, tags, cursorText, cursorLabel };
}
