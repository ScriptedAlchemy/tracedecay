/**
 * Layout for the TRACE anatomy plate (plan 11b Surface 1).
 *
 * The focus is a machined plate carrying only measured fields, `absent`
 * printed where the wire was silent. Callers stand left and callees right as
 * bars on ONE call-site scale, so a 10-site caller and a 10-site callee are the
 * same length. Hop 2 stands in a second column per side, each bar split into
 * one segment per drawn channel into hop 1, and every omission is printed at
 * the foot of the column it happened in.
 *
 * Every drawn call link is also a connector: from its caller's row port to
 * its callee's, down the corridor beside each column and, across sides,
 * behind the plate. All links in one corridor share one x, so parallel runs
 * are one trunk, and a trunk's brightness is the number of links it carries.
 *
 * Pure and DOM-free: numbers in, geometry out.
 */
import type { NeighborsPayload, UndrawnNeighbour } from './model.ts';
import type { TraceModel, TraceNode } from './types.ts';
import { channelBetween, channelKey, clip } from './inspect.ts';

export interface PlateFocusMeta {
  readonly signature: string | null;
  readonly endLine: number | null;
}

export interface PlateField {
  readonly label: string;
  readonly value: string;
  readonly absent: boolean;
}

export interface PlateColumn {
  readonly side: 'up' | 'down';
  readonly hop: 1 | 2;
  readonly x0: number;
  readonly x1: number;
  readonly title: string;
  readonly titleY: number;
  /** Titles sit on the plate side for hop 1 and the outer edge for hop 2. */
  readonly titleX: number;
  readonly titleAnchor: 'start' | 'end';
  readonly notes: readonly { y: number; text: string }[];
}

export interface PlateRow {
  readonly node: TraceNode;
  readonly column: PlateColumn;
  readonly y: number;
  /** Call sites per drawn channel into the hop inside, strongest first. */
  readonly segments: readonly number[];
  readonly name: string;
  readonly meta: string;
  /** x the bar grows from, and the direction it grows in (+1 right, -1 left). */
  readonly anchorX: number;
  readonly grow: 1 | -1;
}

/** One call link, caller row port to callee row port. */
export interface PlateConnector {
  readonly key: string;
  readonly d: string;
  /** x of each vertical run: the corridors this link shares with others. */
  readonly corridors: readonly number[];
}

export type KindShape = 'circle' | 'diamond' | 'square' | 'triangle' | 'bar';

export interface PlateLayout {
  readonly width: number;
  readonly height: number;
  readonly stacked: boolean;
  readonly plate: { x: number; y: number; width: number; height: number };
  readonly fields: readonly PlateField[];
  readonly columns: readonly PlateColumn[];
  readonly rows: readonly PlateRow[];
  readonly connectors: readonly PlateConnector[];
  /** Where connectors pass behind the plate, one per plate edge in use. */
  readonly throughPorts: readonly { x: number; y: number }[];
  readonly scale: { readonly pxPerCall: number; readonly maxCalls: number; readonly y: number };
  /** Drawn links no bar segment encodes: lateral and cross-side, connector only. */
  readonly crossLinks: number;
}

/** Below this width the columns stack above and below the plate. */
export const PLATE_STACK_BELOW = 700;
export const PLATE_ROW = 44;
const MARGIN = 12;
const TITLE_H = 30;
export const PLATE_FIELD_H = 29;
export const PLATE_HEAD_H = 48;
const NOTE_H = 14;
/** Stacked form: the gutter the one shared corridor runs down. */
const GUTTER = 12;
/** Corner radius of a connector's soft elbow. */
const ELBOW = 6;
/** Monospace advance at 11px, used to clip names to their column. */
const CHAR_PX = 6.7;

function count(n: number, unit: string): string {
  return `${n} ${unit}${n === 1 ? '' : 's'}`;
}

function distinctRows(list: NeighborsPayload['callers']): { distinct: number; sites: number } {
  const rows = (list ?? []).filter((row) => row.id.length > 0);
  return { distinct: new Set(rows.map((row) => row.id)).size, sites: rows.length };
}

export function plateFields(
  model: TraceModel,
  root: NeighborsPayload,
  meta: PlateFocusMeta,
): readonly PlateField[] {
  const focus = model.nodes.find((node) => node.id === model.focusId)!;
  const limit = typeof root.limit === 'number' ? root.limit : null;
  const callers = distinctRows(root.callers);
  const callees = distinctRows(root.callees);
  const prefix = (sites: number) => (limit !== null && sites >= limit ? ' · prefix' : '');
  const enclosure = (root.edges ?? []).find(
    (edge) => edge.kind === 'contains' && edge.target === model.focusId,
  );
  const kinds = (root.edges_by_kind ?? []).map((entry) => `${entry.kind} ${entry.count}`);
  const drawnUp = model.nodes.filter((node) => node.ring === -1).length;
  const drawnDown = model.nodes.filter((node) => node.ring === 1).length;
  const field = (label: string, value: string | null): PlateField => ({
    label,
    value: value ?? 'absent',
    absent: value === null,
  });
  return [
    field('file', focus.filePath),
    field(
      'lines',
      focus.startLine === null
        ? null
        : `${focus.startLine}–${meta.endLine === null ? 'end absent' : meta.endLine}`,
    ),
    field('signature', meta.signature),
    field('degree', focus.degree === null ? null : `${focus.degree} edges, all kinds`),
    field('callers', `${count(callers.distinct, 'symbol')} · ${count(callers.sites, 'site')}${prefix(callers.sites)}`),
    field('callees', `${count(callees.distinct, 'symbol')} · ${count(callees.sites, 'site')}${prefix(callees.sites)}`),
    field('self calls', String(focus.selfCalls)),
    field('enclosure', enclosure ? (enclosure.source_name ?? enclosure.source) : null),
    field('edges by kind', kinds.length ? kinds.join(' · ') : null),
    field('drawn', `${drawnUp} of ${callers.distinct} callers · ${drawnDown} of ${callees.distinct} callees`),
  ];
}

function wrap(text: string, width: number): string[] {
  const lines: string[] = [];
  let line = '';
  for (const word of text.split(' ')) {
    if (line && line.length + 1 + word.length > width) {
      lines.push(line);
      line = word;
    } else line = line ? `${line} ${word}` : word;
  }
  if (line) lines.push(line);
  return lines;
}

const NAMED_SHAPES: Readonly<Record<string, KindShape>> = {
  function: 'circle',
  method: 'diamond',
  struct: 'square',
  class: 'square',
  trait: 'triangle',
  interface: 'triangle',
  module: 'bar',
  file: 'bar',
};
const SHAPES: readonly KindShape[] = ['circle', 'diamond', 'square', 'triangle', 'bar'];

/** A kind's shape cue, so kind never rests on hue alone. Unknown kinds hash
 * onto the same five shapes, stable across reloads. */
export function kindShape(kind: string): KindShape {
  const named = NAMED_SHAPES[kind];
  if (named) return named;
  let hash = 0;
  for (let i = 0; i < kind.length; i += 1) hash = (hash * 31 + kind.charCodeAt(i)) >>> 0;
  return SHAPES[hash % SHAPES.length]!;
}

/** A polyline as a path with soft elbows: each corner rounded by `ELBOW`. */
export function elbowPath(points: readonly (readonly [number, number])[]): string {
  const f = (n: number) => Math.round(n * 10) / 10;
  const pts = points.filter(
    (p, i) => i === 0 || p[0] !== points[i - 1]![0] || p[1] !== points[i - 1]![1],
  );
  if (pts.length < 2) return '';
  let d = `M${f(pts[0]![0])},${f(pts[0]![1])}`;
  for (let i = 1; i < pts.length - 1; i += 1) {
    const [px, py] = pts[i - 1]!;
    const [x, y] = pts[i]!;
    const [nx, ny] = pts[i + 1]!;
    const inLen = Math.hypot(x - px, y - py);
    const outLen = Math.hypot(nx - x, ny - y);
    const r = Math.min(ELBOW, inLen / 2, outLen / 2);
    const ax = x - ((x - px) / (inLen || 1)) * r;
    const ay = y - ((y - py) / (inLen || 1)) * r;
    const bx = x + ((nx - x) / (outLen || 1)) * r;
    const by = y + ((ny - y) / (outLen || 1)) * r;
    d += ` L${f(ax)},${f(ay)} Q${f(x)},${f(y)} ${f(bx)},${f(by)}`;
  }
  const last = pts[pts.length - 1]!;
  return `${d} L${f(last[0])},${f(last[1])}`;
}

function columnTitle(side: 'up' | 'down', hop: 1 | 2): string {
  if (side === 'up') return hop === 1 ? 'callers · 1 hop' : '2 hops · via callers';
  return hop === 1 ? 'callees · 1 hop' : '2 hops · via callees';
}

export function layoutPlate(
  model: TraceModel,
  root: NeighborsPayload,
  meta: PlateFocusMeta,
  undrawn: readonly UndrawnNeighbour[],
  width: number,
): PlateLayout {
  const W = Math.max(240, width);
  const stacked = W < PLATE_STACK_BELOW;
  const fields = plateFields(model, root, meta);
  const byRing = (ring: number) => model.nodes.filter((node) => node.ring === ring);

  // Segments: call sites on each drawn channel into the hop one step inward.
  const segmentsOf = (node: TraceNode): { segments: number[]; parents: TraceNode[] } => {
    const inward = Math.abs(node.ring) - 1;
    const parents = model.nodes.filter(
      (other) =>
        Math.abs(other.ring) === inward &&
        (inward === 0 || Math.sign(other.ring) === Math.sign(node.ring)) &&
        channelBetween(model, node.id, other.id) !== undefined,
    );
    const segments = parents
      .map((parent) => channelBetween(model, node.id, parent.id)!.calls)
      .sort((a, b) => b - a);
    return { segments, parents };
  };
  const total = (node: TraceNode) => segmentsOf(node).segments.reduce((sum, n) => sum + n, 0);

  const ordered = (ring: number, parentOrder: ReadonlyMap<string, number> | null): TraceNode[] =>
    byRing(ring).sort((a, b) => {
      if (parentOrder) {
        const pa = Math.min(...segmentsOf(a).parents.map((p) => parentOrder.get(p.id) ?? 99), 99);
        const pb = Math.min(...segmentsOf(b).parents.map((p) => parentOrder.get(p.id) ?? 99), 99);
        if (pa !== pb) return pa - pb;
      }
      return total(b) - total(a) || a.name.localeCompare(b.name);
    });

  const up1 = ordered(-1, null);
  const down1 = ordered(1, null);
  const up2 = ordered(-2, new Map(up1.map((node, i) => [node.id, i])));
  const down2 = ordered(2, new Map(down1.map((node, i) => [node.id, i])));

  const notesFor = (side: 'up' | 'down', hop: 1 | 2): string[] => {
    const out: string[] = [];
    const hidden = undrawn.filter((entry) => entry.side === side && entry.hop === hop).length;
    if (hidden > 0) out.push(`+${hidden} named, not drawn`);
    if (hop === 2) {
      const seeds = (side === 'up' ? up1 : down1).length;
      if (model.coverage.hopsFetched === 1 && seeds > 0) out.push('hop 2 not fetched');
    }
    if (hop === 1 && model.coverage.capped) out.push('a list hit the row limit: prefix only');
    return out;
  };

  // The widest bar is a row's whole stack of segments, not its largest one.
  // Readouts wrap to their column rather than truncate: an omission count is
  // never the part of a line that gets cut.
  const noteLines = (frame: { side: 'up' | 'down'; hop: 1 | 2; x0: number; x1: number }): string[] =>
    notesFor(frame.side, frame.hop).flatMap((text) => wrap(text, Math.max(12, Math.floor((frame.x1 - frame.x0) / 6.2))));

  const maxCalls = Math.max(1, ...model.nodes.filter((n) => n.ring !== 0).map(total));

  /* ---- column frames ---------------------------------------------------- */
  let plate: PlateLayout['plate'];
  const frames: Array<{ side: 'up' | 'down'; hop: 1 | 2; x0: number; x1: number; nodes: TraceNode[]; y0: number }> = [];
  const plateH = PLATE_HEAD_H + fields.length * PLATE_FIELD_H + 14;

  if (!stacked) {
    const plateW = Math.min(320, Math.max(250, Math.round(W * 0.28)));
    const plateX = Math.round((W - plateW) / 2);
    const sideW = plateX - MARGIN - 34;
    const place = (side: 'up' | 'down', inner: TraceNode[], outer: TraceNode[]) => {
      const hasOuter = outer.length > 0;
      const innerW = hasOuter ? Math.round(sideW * 0.54) : sideW;
      const outerW = hasOuter ? sideW - innerW - 30 : 0;
      if (side === 'up') {
        const innerX1 = plateX - 34;
        frames.push({ side, hop: 1, x0: innerX1 - innerW, x1: innerX1, nodes: inner, y0: TITLE_H });
        if (hasOuter) frames.push({ side, hop: 2, x0: MARGIN, x1: MARGIN + outerW, nodes: outer, y0: TITLE_H });
      } else {
        const innerX0 = plateX + plateW + 34;
        frames.push({ side, hop: 1, x0: innerX0, x1: innerX0 + innerW, nodes: inner, y0: TITLE_H });
        if (hasOuter) frames.push({ side, hop: 2, x0: W - MARGIN - outerW, x1: W - MARGIN, nodes: outer, y0: TITLE_H });
      }
    };
    place('up', up1, up2);
    place('down', down1, down2);
    const hop1Rows = Math.max(up1.length, down1.length, 1);
    const plateY = Math.max(TITLE_H, TITLE_H + (hop1Rows * PLATE_ROW - plateH) / 2);
    plate = { x: plateX, y: Math.round(plateY), width: plateW, height: plateH };
  } else {
    // Stacked: flow reads top to bottom, callers of callers first.
    let y = 0;
    const stack = (side: 'up' | 'down', hop: 1 | 2, nodes: TraceNode[]) => {
      if (nodes.length === 0 && notesFor(side, hop).length === 0) return;
      frames.push({ side, hop, x0: MARGIN + GUTTER, x1: W - MARGIN, nodes, y0: y + TITLE_H });
      y += TITLE_H + nodes.length * PLATE_ROW + noteLines({ side, hop, x0: MARGIN + GUTTER, x1: W - MARGIN }).length * NOTE_H + 12;
    };
    stack('up', 2, up2);
    stack('up', 1, up1);
    plate = { x: MARGIN + GUTTER, y: y + 6, width: W - MARGIN * 2 - GUTTER, height: plateH };
    y += plateH + 18;
    stack('down', 1, down1);
    stack('down', 2, down2);
  }

  // Channels a bar segment encodes; anything else incident on a row is
  // counted on that row, so an omission is attributed rather than pooled.
  const encoded = new Set<string>();
  for (const node of model.nodes) {
    for (const parent of segmentsOf(node).parents) encoded.add(channelKey(channelBetween(model, node.id, parent.id)!));
  }
  const crossOn = (id: string) =>
    model.channels.filter((c) => (c.a === id || c.b === id) && !encoded.has(channelKey(c))).length;

  const columns: PlateColumn[] = [];
  const rows: PlateRow[] = [];
  for (const frame of frames) {
    const notes = noteLines(frame).map((text, i) => ({
      y: frame.y0 + frame.nodes.length * PLATE_ROW + 12 + i * NOTE_H,
      text,
    }));
    const column: PlateColumn = {
      side: frame.side,
      hop: frame.hop,
      x0: frame.x0,
      x1: frame.x1,
      title: columnTitle(frame.side, frame.hop),
      titleY: frame.y0 - 12,
      titleX: stacked || (frame.side === 'up') === (frame.hop === 2) ? frame.x0 : frame.x1,
      titleAnchor: stacked || (frame.side === 'up') === (frame.hop === 2) ? 'start' : 'end',
      notes,
    };
    columns.push(column);
    // Left columns grow their bars leftward from the inner edge; right columns
    // and every stacked column grow rightward, so the scale reads one way.
    const growLeft = !stacked && frame.side === 'up';
    // The kind glyph takes the first 14px on the name line.
    const chars = Math.max(8, Math.floor((frame.x1 - frame.x0 - 14) / CHAR_PX));
    frame.nodes.forEach((node, i) => {
      const y = frame.y0 + i * PLATE_ROW;
      const { segments, parents } = segmentsOf(node);
      const bits = [node.kind, node.degree === null ? 'degree absent' : `deg ${node.degree}`];
      if (node.undrawnEdges === null) bits.push('undrawn edges absent');
      else if (node.undrawnEdges > 0) bits.push(`+${node.undrawnEdges} edges not drawn`);
      if (node.selfCalls > 0) bits.push(`↻ ${node.selfCalls} self`);
      const cross = crossOn(node.id);
      if (cross > 0) bits.push(`⇄ ${cross} cross-link${cross === 1 ? '' : 's'}`);
      if (stacked && Math.abs(node.ring) === 2 && parents.length > 0) {
        bits.push(`via ${parents.map((p) => p.name).join(', ')}`);
      }
      const anchorX = growLeft ? frame.x1 : frame.x0;
      rows.push({
        node,
        column,
        y,
        segments,
        name: clip(node.name, chars),
        meta: clip(bits.join(' · '), Math.max(4, chars - segments.join('+').length - 3)),
        anchorX,
        grow: growLeft ? -1 : 1,
      });
    });
  }

  const narrowest = Math.min(...columns.map((c) => c.x1 - c.x0 - 6), 280);
  const pxPerCall = Math.max(2, narrowest / maxCalls);

  /* ---- connectors ----------------------------------------------------- */
  const rowOf = new Map(rows.map((row) => [row.node.id, row]));
  const throughY = Math.round(plate.y + plate.height / 2);
  const connectors: PlateConnector[] = [];
  const throughUsed = new Set<number>();
  type Pt = readonly [number, number];
  if (stacked) {
    // One corridor down the gutter; the plate's port is its left edge.
    const bus = MARGIN;
    const port = (id: string): Pt =>
      id === model.focusId ? [plate.x, throughY] : [rowOf.get(id)!.column.x0, rowOf.get(id)!.y + 21];
    for (const channel of model.channels) {
      if (!rowOf.has(channel.a) && channel.a !== model.focusId) continue;
      if (!rowOf.has(channel.b) && channel.b !== model.focusId) continue;
      const [ax, ay] = port(channel.a);
      const [bx, by] = port(channel.b);
      if (channel.a === model.focusId || channel.b === model.focusId) throughUsed.add(plate.x);
      connectors.push({
        key: channelKey(channel),
        d: elbowPath([[ax, ay], [bus, ay], [bus, by], [bx, by]]),
        corridors: [bus],
      });
    }
  } else {
    // Slots left to right: the four columns and the plate between them.
    const slots = [
      ...columns.map((column) => ({ x0: column.x0, x1: column.x1, column })),
      { x0: plate.x, x1: plate.x + plate.width, column: null },
    ].sort((p, q) => p.x0 - q.x0);
    const slotOf = (id: string) =>
      id === model.focusId
        ? slots.findIndex((slot) => slot.column === null)
        : slots.findIndex((slot) => slot.column === rowOf.get(id)!.column);
    const plateSlot = slotOf(model.focusId);
    const gapX = (left: number) => Math.round((slots[left]!.x1 + slots[left + 1]!.x0) / 2);
    const portY = (id: string) => (id === model.focusId ? throughY : rowOf.get(id)!.y + 21);
    const edge = (id: string, facing: 'left' | 'right'): number => {
      const slot = slots[slotOf(id)]!;
      return facing === 'left' ? slot.x0 : slot.x1;
    };
    for (const channel of model.channels) {
      const ends = [channel.a, channel.b];
      if (ends.some((id) => id !== model.focusId && !rowOf.has(id))) continue;
      const [l, r] = [...ends].sort((p, q) => slotOf(p) - slotOf(q)) as [string, string];
      const i = slotOf(l);
      const j = slotOf(r);
      let points: Pt[];
      let corridors: number[];
      if (i === j) {
        // Same column: out through the corridor on the plate side and back.
        const toward = i < plateSlot ? i : i - 1;
        const facing = i < plateSlot ? 'right' : 'left';
        const bus = gapX(toward);
        points = [[edge(l, facing), portY(l)], [bus, portY(l)], [bus, portY(r)], [edge(r, facing), portY(r)]];
        corridors = [bus];
      } else if (j === i + 1) {
        const bus = gapX(i);
        points = [[edge(l, 'right'), portY(l)], [bus, portY(l)], [bus, portY(r)], [edge(r, 'left'), portY(r)]];
        corridors = [bus];
      } else {
        // Across the plate: both corridors meet the plate's through line, and
        // the run between them is hidden behind the plate face.
        const first = gapX(i);
        const last = gapX(j - 1);
        points = [
          [edge(l, 'right'), portY(l)],
          [first, portY(l)],
          [first, throughY],
          [last, throughY],
          [last, portY(r)],
          [edge(r, 'left'), portY(r)],
        ];
        corridors = [first, last];
      }
      if (i < plateSlot && j >= plateSlot) throughUsed.add(plate.x);
      if (j > plateSlot && i <= plateSlot) throughUsed.add(plate.x + plate.width);
      connectors.push({ key: channelKey(channel), d: elbowPath(points), corridors });
    }
  }
  const throughPorts = [...throughUsed].sort((a, b) => a - b).map((x) => ({ x, y: throughY }));

  const contentBottom = Math.max(
    plate.y + plate.height,
    ...frames.map(
      (frame) =>
        frame.y0 + frame.nodes.length * PLATE_ROW + noteLines(frame).length * NOTE_H,
    ),
  );
  const scaleY = contentBottom + 20;
  return {
    width: W,
    height: scaleY + 40,
    stacked,
    plate,
    fields,
    columns,
    rows,
    connectors,
    throughPorts,
    scale: { pxPerCall, maxCalls, y: scaleY },
    crossLinks: model.channels.length - encoded.size,
  };
}
