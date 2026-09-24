/**
 * Layout for the ANATOMY PLATE candidate (plan 11b Surface 1).
 *
 * The focus is a machined plate carrying only measured fields, `absent`
 * printed where the wire was silent. Callers stand left and callees right as
 * bars on ONE call-site scale, so a 10-site caller and a 10-site callee are the
 * same length. Hop 2 stands in a second column per side, each bar split into
 * one segment per drawn channel into hop 1, and every omission is printed at
 * the foot of the column it happened in.
 *
 * Pure and DOM-free: numbers in, geometry out.
 */
import type { NeighborsPayload, UndrawnNeighbour } from './model.ts';
import type { TraceModel, TraceNode } from './types.ts';
import { channelBetween, channelKey, clip } from './variants.ts';

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
  /** Where hairlines meet this row, on its inner and outer edge. */
  readonly innerPort: { x: number; y: number };
  readonly outerPort: { x: number; y: number };
}

export interface PlateLink {
  readonly key: string;
  readonly from: { x: number; y: number };
  readonly to: { x: number; y: number };
}

export interface PlateLayout {
  readonly width: number;
  readonly height: number;
  readonly stacked: boolean;
  readonly plate: { x: number; y: number; width: number; height: number };
  readonly fields: readonly PlateField[];
  readonly columns: readonly PlateColumn[];
  readonly rows: readonly PlateRow[];
  readonly links: readonly PlateLink[];
  readonly ports: readonly { x: number; y: number; key: string }[];
  readonly scale: { readonly pxPerCall: number; readonly maxCalls: number; readonly y: number };
  /** Drawn channels the plate does not draw a hairline for (lateral, cross-side). */
  readonly omittedChannels: number;
}

/** Below this width the columns stack above and below the plate. */
export const PLATE_STACK_BELOW = 700;
export const PLATE_ROW = 44;
const MARGIN = 12;
const TITLE_H = 30;
export const PLATE_FIELD_H = 29;
export const PLATE_HEAD_H = 48;
const NOTE_H = 14;
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
  const prefix = (sites: number) => (limit !== null && sites >= limit ? ' · prefix at limit' : '');
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
      frames.push({ side, hop, x0: MARGIN, x1: W - MARGIN, nodes, y0: y + TITLE_H });
      y += TITLE_H + nodes.length * PLATE_ROW + noteLines({ side, hop, x0: MARGIN, x1: W - MARGIN }).length * NOTE_H + 12;
    };
    stack('up', 2, up2);
    stack('up', 1, up1);
    plate = { x: MARGIN, y: y + 6, width: W - MARGIN * 2, height: plateH };
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
    const chars = Math.max(8, Math.floor((frame.x1 - frame.x0) / CHAR_PX));
    frame.nodes.forEach((node, i) => {
      const y = frame.y0 + i * PLATE_ROW;
      const { segments, parents } = segmentsOf(node);
      const bits = [node.kind, node.degree === null ? 'degree absent' : `deg ${node.degree}`];
      if (node.undrawnEdges === null) bits.push('undrawn edges absent');
      else if (node.undrawnEdges > 0) bits.push(`+${node.undrawnEdges} edges not drawn`);
      if (node.selfCalls > 0) bits.push(`↻ ${node.selfCalls} self`);
      const cross = crossOn(node.id);
      if (cross > 0) bits.push(`⇄ ${cross} off-plate`);
      if (stacked && Math.abs(node.ring) === 2 && parents.length > 0) {
        bits.push(`via ${parents.map((p) => p.name).join(', ')}`);
      }
      const anchorX = growLeft ? frame.x1 : frame.x0;
      const outerX = growLeft ? frame.x0 : frame.x1;
      rows.push({
        node,
        column,
        y,
        segments,
        name: clip(node.name, chars),
        meta: clip(bits.join(' · '), Math.max(4, chars - segments.join('+').length - 3)),
        anchorX,
        grow: growLeft ? -1 : 1,
        innerPort: { x: anchorX, y: y + 21 },
        outerPort: { x: outerX, y: y + 21 },
      });
    });
  }

  const narrowest = Math.min(...columns.map((c) => c.x1 - c.x0 - 6), 280);
  const pxPerCall = Math.max(2, narrowest / maxCalls);

  /* ---- hairlines -------------------------------------------------------- */
  const links: PlateLink[] = [];
  const ports: Array<{ x: number; y: number; key: string }> = [];
  const rowOf = new Map(rows.map((row) => [row.node.id, row]));
  // Stacked rows carry their route in text (`via …`) and the bars; hairlines
  // would all run down one gutter and say nothing more.
  if (!stacked) {
    for (const [side, list] of [
      ['up', up1],
      ['down', down1],
    ] as const) {
      const edgeX = side === 'up' ? plate.x : plate.x + plate.width;
      const span = plate.height - PLATE_HEAD_H - 12;
      list.forEach((node, i) => {
        const channel = channelBetween(model, node.id, model.focusId);
        if (!channel) return;
        const port = { x: edgeX, y: plate.y + PLATE_HEAD_H + ((i + 0.5) * span) / list.length };
        const key = channelKey(channel);
        ports.push({ ...port, key });
        links.push({ key, from: rowOf.get(node.id)!.innerPort, to: port });
      });
    }
    for (const node of [...up2, ...down2]) {
      const row = rowOf.get(node.id)!;
      for (const parent of segmentsOf(node).parents) {
        const channel = channelBetween(model, node.id, parent.id)!;
        links.push({
          key: channelKey(channel),
          from: row.innerPort,
          to: rowOf.get(parent.id)!.outerPort,
        });
      }
    }
  }

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
    links,
    ports,
    scale: { pxPerCall, maxCalls, y: scaleY },
    omittedChannels: model.channels.length - encoded.size,
  };
}
