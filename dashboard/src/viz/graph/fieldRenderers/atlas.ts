import { lerpRgbTuple } from '../activation.ts';
import {
  createFrameLoop,
  mountCanvas,
  rgbaString,
  engraved,
  type FieldPalette,
  type FieldRendererFactory,
  type FieldScene,
  type FieldView,
  type SceneBody,
  type Synapse,
} from './scene.ts';

/**
 * Variant C, the instrument atlas: every project is an engraved plate on a
 * fixed grid. Its column is its recency bucket and its row is its canonical
 * id order inside that column, so a plate moves only when its recency bucket
 * changes. Every reading is printed, `absent` included; relations are routed
 * hairlines between plates; admitted activity is an amber bar on the plate's
 * edge. Zoom is a focus mode over whole plates, never shrunken text.
 */

export interface PlateTier {
  height: number;
  level: 'full' | 'compact' | 'minimal';
}

const TIERS: readonly PlateTier[] = [
  { height: 70, level: 'full' },
  { height: 46, level: 'compact' },
  { height: 24, level: 'minimal' },
];
const GAP = 6;
const HEADER = 44;
const GUTTER = 18;

export interface Plate {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  body: SceneBody;
  /** Symbol rows when the plate groups a returned graph by kind. */
  rows?: ReadonlyArray<{ id: string; label: string; y: number }>;
  title?: string;
  footer?: string;
}

export interface Wire {
  hub: string;
  points: ReadonlyArray<[number, number]>;
  junction: [number, number];
}

export interface AtlasLayout {
  plates: Plate[];
  wires: Wire[];
  tier: PlateTier;
  height: number;
  columnX: number[];
  columnW: number;
}

/** Plates for the measured registry. Pure, so the grid is testable. */
export function registryAtlas(scene: FieldScene, width: number, height: number): AtlasLayout {
  const columns = scene.columns ?? [];
  const n = Math.max(columns.length, 1);
  const columnW = (width - GUTTER * (n + 1)) / n;
  const columnX = columns.map((_, index) => GUTTER + index * (columnW + GUTTER));
  const byColumn = new Map<number, SceneBody[]>();
  for (const body of scene.bodies) {
    if (body.role !== 'body') continue;
    const index = Math.max(0, Math.min(n - 1, Math.round(body.x)));
    const bucket = byColumn.get(index) ?? [];
    bucket.push(body);
    byColumn.set(index, bucket);
  }
  const tallest = Math.max(1, ...[...byColumn.values()].map((bucket) => bucket.length));
  const room = height - HEADER - 12;
  const tier = TIERS.find((candidate) => tallest * (candidate.height + GAP) <= room) ?? TIERS[TIERS.length - 1]!;
  const plates: Plate[] = [];
  for (const [index, bucket] of byColumn) {
    [...bucket]
      .sort((a, b) => a.id.localeCompare(b.id))
      .forEach((body, row) => {
        plates.push({
          id: body.id,
          x: columnX[index] ?? GUTTER,
          y: HEADER + row * (tier.height + GAP),
          w: columnW,
          h: tier.height,
          body,
        });
      });
  }
  const plateOf = new Map(plates.map((plate) => [plate.id, plate]));
  const wires: Wire[] = [];
  for (const hub of scene.bodies) {
    if (hub.role !== 'hub') continue;
    const members = scene.paths
      .filter((path) => path.source === hub.id || path.target === hub.id)
      .map((path) => plateOf.get(path.source === hub.id ? path.target : path.source))
      .filter((plate): plate is Plate => plate != null)
      .sort((a, b) => a.x - b.x || a.y - b.y);
    const first = members[0];
    if (!first || members.length < 2) continue;
    // A vertical bus in the gutter right of the first member's column.
    const busX = first.x + first.w + GUTTER / 2;
    const ys = members.map((plate) => plate.y + plate.h / 2);
    const junction: [number, number] = [busX, (Math.min(...ys) + Math.max(...ys)) / 2];
    for (const plate of members) {
      const y = plate.y + plate.h / 2;
      const edge = plate.x > busX ? plate.x : plate.x + plate.w;
      wires.push({ hub: hub.id, junction, points: [[edge, y], [busX, y], junction] });
    }
  }
  const bottom = plates.reduce((max, plate) => Math.max(max, plate.y + plate.h), HEADER);
  return { plates, wires, tier, height: bottom + 12, columnX, columnW };
}

/** Plates for a returned symbol graph: one per symbol kind, symbols printed
 * by connectedness. */
export function kindAtlas(scene: FieldScene, width: number): AtlasLayout {
  const byKind = new Map<string, SceneBody[]>();
  for (const body of scene.bodies) {
    const bucket = byKind.get(body.kind) ?? [];
    bucket.push(body);
    byKind.set(body.kind, bucket);
  }
  const kindOf = new Map(scene.bodies.map((body) => [body.id, body.kind]));
  const counts = new Map<string, Map<string, number>>();
  for (const path of scene.paths) {
    const a = kindOf.get(path.source);
    const b = kindOf.get(path.target);
    if (!a || !b || a === b) continue;
    for (const [from, to] of [[a, b], [b, a]] as const) {
      const row = counts.get(from) ?? new Map<string, number>();
      row.set(to, (row.get(to) ?? 0) + 1);
      counts.set(from, row);
    }
  }
  const kinds = [...byKind.entries()].sort((a, b) => b[1].length - a[1].length || a[0].localeCompare(b[0]));
  const cols = Math.max(1, Math.min(4, Math.floor(width / 260)));
  const w = (width - GUTTER * (cols + 1)) / cols;
  const heights = new Array<number>(cols).fill(GUTTER);
  const plates: Plate[] = [];
  for (const [kind, members] of kinds) {
    const ranked = [...members].sort((a, b) => (b.mass ?? -1) - (a.mass ?? -1) || a.label.localeCompare(b.label));
    const shown = ranked.slice(0, 12);
    const relations = [...(counts.get(kind) ?? new Map<string, number>()).entries()]
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .map(([other, count]) => `${other} ${count}`);
    const h = 40 + shown.length * 14 + (ranked.length > shown.length ? 14 : 0) + 20;
    const col = heights.indexOf(Math.min(...heights));
    const y = heights[col]!;
    heights[col] = y + h + GAP * 2;
    plates.push({
      id: `kind:${kind}`,
      x: GUTTER + col * (w + GUTTER),
      y,
      w,
      h,
      body: ranked[0]!,
      title: `${kind} · ${members.length} ${members.length === 1 ? 'symbol' : 'symbols'}`,
      footer: relations.length > 0 ? `relations  ${relations.join(' · ')}` : 'relations  none to other kinds',
      rows: shown.map((body, index) => ({ id: body.id, label: body.label, y: y + 38 + index * 14 })),
    });
  }
  const plateOf = new Map(plates.map((plate) => [plate.id, plate]));
  const wires: Wire[] = [];
  for (const [from, row] of counts) {
    for (const [to] of row) {
      if (from > to) continue;
      const pa = plateOf.get(`kind:${from}`);
      const pb = plateOf.get(`kind:${to}`);
      if (!pa || !pb) continue;
      const a: [number, number] = [pa.x + pa.w / 2, pa.y + pa.h / 2];
      const b: [number, number] = [pb.x + pb.w / 2, pb.y + pb.h / 2];
      wires.push({ hub: `${from}\u0000${to}`, junction: a, points: [a, [a[0], b[1]], b] });
    }
  }
  return { plates, wires, tier: TIERS[0]!, height: Math.max(...heights), columnX: [], columnW: w };
}

/** The plate, or printed symbol row, under a point in layout pixels. */
export function pickPlate(layout: AtlasLayout, x: number, y: number): string | null {
  for (const plate of layout.plates) {
    if (x < plate.x || x > plate.x + plate.w || y < plate.y || y > plate.y + plate.h) continue;
    if (!plate.rows) return plate.id;
    const row = plate.rows.find((candidate) => y >= candidate.y - 11 && y < candidate.y + 3);
    return row?.id ?? null;
  }
  return null;
}

export const createAtlasField: FieldRendererFactory = ({
  container,
  scene,
  field,
  palette,
  isReduced,
  onHover,
  onSelect,
}) => {
  const mounted = mountCanvas(container);
  if (!mounted) throw new Error('This browser has no 2D canvas context.');
  const { canvas, context, size, fitToContainer } = mounted;
  let colors: FieldPalette = palette;
  let view: FieldView = { inspected: null, focus: null };
  let hovered: string | null = null;
  let scroll = 0;
  const synapses: Synapse[] = [];
  const byId = new Map(scene.bodies.map((body) => [body.id, body]));
  const massCeiling = scene.bodies.reduce((max, body) => Math.max(max, body.mass ?? 0), 1);
  const layoutFor = (): AtlasLayout =>
    scene.columns ? registryAtlas(scene, size().width, size().height) : kindAtlas(scene, size().width);
  let layout = layoutFor();

  const anchor = (): string | null => hovered ?? view.inspected;
  const related = (id: string): boolean => {
    const a = anchor();
    if (a == null) return true;
    if (id === a) return true;
    const hubs = scene.neighbors.get(a) ?? [];
    return hubs.some((hub) => (scene.neighbors.get(hub) ?? []).includes(id)) || hubs.includes(id);
  };

  const text = (value: string, x: number, y: number, font: string, rgb: [number, number, number], alpha: number, align: CanvasTextAlign = 'left', legend = false): void => {
    context.font = font;
    context.textAlign = align;
    context.fillStyle = rgbaString(rgb, alpha);
    if (legend) engraved(context, value, x, y);
    else context.fillText(value, x, y);
  };
  const mono = (weight: number, px: number): string => `${weight} ${px}px ${colors.labelFont}`;

  const drawPlate = (plate: Plate, now: number, alpha: number, level: PlateTier['level'] | 'focus'): void => {
    const { x, w, h } = plate;
    const y = plate.y - scroll;
    const body = plate.body;
    const heat = plate.rows ? 0 : field.heatOf(body.id);
    context.fillStyle = rgbaString(colors.face, 0.92 * alpha);
    context.fillRect(x, y, w, h);
    // Engraving: one inset highlight, one hairline frame.
    context.strokeStyle = rgbaString(colors.edgeStrong, 0.55 * alpha);
    context.lineWidth = 1;
    context.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);
    context.strokeStyle = rgbaString(colors.ink, 0.05 * alpha);
    context.beginPath();
    context.moveTo(x + 1.5, y + 1.5);
    context.lineTo(x + w - 1.5, y + 1.5);
    context.stroke();
    if (heat > 0.01) {
      context.fillStyle = rgbaString(colors.alert, Math.min(1, 0.25 + heat));
      context.fillRect(x, y, 4, h);
    }
    if (plate.rows) {
      text((plate.title ?? '').toUpperCase(), x + 10, y + 18, mono(500, 10), colors.ink, 0.9 * alpha, 'left', true);
      context.fillStyle = rgbaString(colors.edgeStrong, 0.4 * alpha);
      context.fillRect(x + 10, y + 25, w - 20, 1);
      for (const row of plate.rows) {
        const symbol = byId.get(row.id);
        const inspected = row.id === view.inspected || row.id === hovered;
        const ry = row.y - scroll;
        if (inspected) {
          context.fillStyle = rgbaString(colors.hot, 0.14);
          context.fillRect(x + 4, ry - 11, w - 8, 14);
          context.fillStyle = rgbaString(colors.hot, 1);
          context.fillRect(x + 4, ry - 11, 2, 14);
        }
        text(fit(row.label, w - 80), x + 10, ry, mono(inspected ? 600 : 400, 11), inspected ? colors.hot : colors.ink, 0.9 * alpha);
        text(symbol?.mass == null ? 'absent' : String(symbol.mass), x + w - 10, ry, mono(400, 10), colors.inkMuted, 0.9 * alpha, 'right');
      }
      const hidden = scene.bodies.filter((candidate) => `kind:${candidate.kind}` === plate.id).length - plate.rows.length;
      if (hidden > 0) text(`${hidden} more in the symbol list`, x + 10, y + h - 26, mono(400, 10), colors.inkMuted, 0.8 * alpha);
      context.fillStyle = rgbaString(colors.edgeStrong, 0.3 * alpha);
      context.fillRect(x + 10, y + h - 20, w - 20, 1);
      text(fit(plate.footer ?? '', w - 20), x + 10, y + h - 7, mono(400, 10), colors.inkMuted, 0.9 * alpha);
      return;
    }
    const strike = synapses.find((synapse) => synapse.from === body.id);
    const warm = heat > 0.02 && strike;
    const [stores, artifacts, mass, seen, branch, repo] = body.detail;
    // A narrow plate keeps its name and drops the age, which the inspector
    // and the exact registry still print.
    const printsWhen = level === 'full' || (level === 'minimal' && w >= 150);
    const labelWidth = w - (printsWhen ? 84 : 34);
    text(fit(body.label, labelWidth), x + 12, y + (level === 'minimal' ? 16 : 17), mono(600, 11), body.id === view.inspected ? colors.hot : colors.ink, 0.95 * alpha);
    const when = warm ? `${strike.label} · ${strike.time}` : (seen ?? 'seen absent').replace(/^seen /, '');
    if (printsWhen) text(when, x + w - 10, y + (level === 'minimal' ? 16 : 17), mono(400, 10), warm ? colors.alert : colors.inkMuted, 0.95 * alpha, 'right');
    const bar = (by: number): void => {
      const track = w - 24;
      context.fillStyle = rgbaString(colors.edgeStrong, 0.35 * alpha);
      context.fillRect(x + 12, by, track, 2);
      context.fillStyle = rgbaString(colors.ink, 0.7 * alpha);
      context.fillRect(x + 12, by, Math.max(2, track * ((body.mass ?? 0) / massCeiling)), 2);
    };
    if (level === 'minimal') {
      bar(y + h - 4);
      return;
    }
    if (level === 'compact') {
      text(fit(`${mass ?? 'mass absent'} · ${when}`, w - 24), x + 12, y + 32, mono(400, 10), warm ? colors.alert : colors.inkMuted, 0.95 * alpha);
      bar(y + 39);
      glyph(x + w - 18, y + 9, body, alpha);
      return;
    }
    text([stores, artifacts, mass].map((value) => value ?? 'absent').join(' · '), x + 12, y + 32, mono(400, 10), colors.inkMuted, 0.95 * alpha);
    bar(y + 38);
    // Repository tile, printed even when absent.
    const tileY = y + 46;
    const shared = (scene.neighbors.get(body.id) ?? []).length > 0;
    context.setLineDash(body.group ? [] : [3, 3]);
    context.strokeStyle = rgbaString(shared ? colors.ink : colors.edgeStrong, (shared ? 0.55 : 0.45) * alpha);
    context.strokeRect(x + 12.5, tileY + 0.5, w - 25, 16);
    context.setLineDash([]);
    const hubHeat = shared ? Math.max(...(scene.neighbors.get(body.id) ?? []).map((hub) => field.heatOf(hub))) : 0;
    if (hubHeat > 0.01) {
      context.fillStyle = rgbaString(colors.alert, 0.18 + 0.5 * hubHeat);
      context.fillRect(x + 13, tileY + 1, 3, 15);
    }
    text(fit(`${(repo ?? 'repository absent')}${shared ? ' · shared' : ''} · ${branch ?? 'branch absent'}`, w - 34), x + 19, tileY + 12, mono(400, 10), colors.inkMuted, 0.95 * alpha);
  };

  const glyph = (gx: number, gy: number, body: SceneBody, alpha: number): void => {
    const shared = (scene.neighbors.get(body.id) ?? []).length > 0;
    context.setLineDash(body.group ? [] : [2, 2]);
    context.strokeStyle = rgbaString(colors.ink, 0.6 * alpha);
    context.strokeRect(gx + 0.5, gy + 0.5, 7, 7);
    context.setLineDash([]);
    if (shared) {
      context.fillStyle = rgbaString(colors.ink, 0.6 * alpha);
      context.fillRect(gx + 2, gy + 2, 4, 4);
    }
  };

  const drawFocus = (now: number): void => {
    const { width, height } = size();
    const members = layout.plates.filter((plate) => view.focus?.has(plate.id));
    if (members.length === 0) return;
    context.fillStyle = rgbaString(colors.substrate, 0.78);
    context.fillRect(0, 0, width, height);
    const w = Math.min(320, (width - 80) / members.length - 40);
    const h = 176;
    const total = members.length * w + (members.length - 1) * 80;
    const top = Math.max(60, (height - h) / 2);
    const junction: [number, number] = [width / 2, top + h + 46];
    members.forEach((plate, index) => {
      const x = (width - total) / 2 + index * (w + 80);
      const focused: Plate = { ...plate, x, y: top + scroll, w, h };
      drawPlate(focused, now, 1, 'full');
      const body = plate.body;
      const fields: Array<[string, string]> = [
        ['id', body.id],
        ['kind', body.kind],
        ...body.detail.slice(3).map((line): [string, string] => {
          const [key, ...rest] = line.split(' ');
          return [key ?? '', rest.join(' ') || 'absent'];
        }),
      ];
      fields.forEach(([key, value], row) => {
        text(key.toUpperCase(), x + 12, top + 82 + row * 15, mono(500, 9), colors.inkMuted, 0.9);
        text(fit(value, w - 90), x + 78, top + 82 + row * 15, mono(400, 10), colors.ink, 0.9);
      });
      context.strokeStyle = rgbaString(colors.ink, 0.5);
      context.beginPath();
      context.moveTo(x + w / 2, top + h);
      context.lineTo(x + w / 2, junction[1]);
      context.lineTo(junction[0], junction[1]);
      context.stroke();
    });
    const hub = scene.bodies.find((body) => body.role === 'hub' && view.focus?.has(body.id));
    context.fillStyle = rgbaString(colors.substrate, 1);
    context.strokeStyle = rgbaString(lerpRgbTuple(colors.ink, colors.alert, hub ? Math.min(1, field.heatOf(hub.id) * 1.5) : 0), 0.9);
    context.lineWidth = 1.5;
    context.fillRect(junction[0] - 5, junction[1] - 5, 10, 10);
    context.strokeRect(junction[0] - 5, junction[1] - 5, 10, 10);
    context.lineWidth = 1;
    if (hub) {
      text(`repo:${hub.label}`, junction[0], junction[1] + 22, mono(600, 11), colors.ink, 0.95, 'center');
      text(fit(hub.detail.join(' · '), width - 80), junction[0], junction[1] + 36, mono(400, 10), colors.inkMuted, 0.9, 'center');
    }
    text('FOCUS · EXACT SHARED GIT DIRECTORY', 16, 22, mono(500, 10), colors.hot, 0.9, 'left', true);
  };

  const draw = (now: number): boolean => {
    const warm = field.tick(now);
    const { width, height } = size();
    context.globalCompositeOperation = 'source-over';
    context.fillStyle = rgbaString(colors.substrate, 1);
    context.fillRect(0, 0, width, height);
    // The plate bed: a 32px graticule the plates sit on.
    context.strokeStyle = rgbaString(colors.grid, 0.5);
    context.beginPath();
    for (let gx = 0.5; gx < width; gx += 32) {
      context.moveTo(gx, 0);
      context.lineTo(gx, height);
    }
    for (let gy = 0.5 - (scroll % 32); gy < height; gy += 32) {
      context.moveTo(0, gy);
      context.lineTo(width, gy);
    }
    context.stroke();
    if (scene.columns) {
      scene.columns.forEach((column, index) => {
        const cx = (layout.columnX[index] ?? 0) + layout.columnW / 2;
        text(column.bound.toUpperCase(), cx, 16, mono(500, 10), colors.ink, 0.86, 'center', true);
        text(`${column.label} · ${column.count}`, cx, 30, mono(400, 10), colors.inkMuted, 0.9, 'center');
        if (column.count === 0) text('no projects', cx, HEADER + 18, mono(400, 10), colors.inkMuted, 0.7, 'center');
      });
    }
    // Wires beneath plates, routed through gutters.
    for (const wire of layout.wires) {
      const heat = byId.has(wire.hub) ? field.heatOf(wire.hub) : 0;
      const members = scene.neighbors.get(wire.hub) ?? [];
      const dim = anchor() == null || members.includes(anchor()!) || wire.hub === anchor() ? 1 : 0.3;
      context.strokeStyle =
        heat > 0.02
          ? rgbaString(lerpRgbTuple(colors.ink, colors.alert, Math.min(1, heat * 1.5)), 0.6 + 0.4 * heat)
          : rgbaString(colors.ink, (scene.columns ? 0.5 : 0.28) * dim);
      context.lineWidth = heat > 0.02 ? 1.5 : 1;
      context.beginPath();
      wire.points.forEach(([px, py], index) => {
        if (index === 0) context.moveTo(px, py - scroll);
        else context.lineTo(px, py - scroll);
      });
      context.stroke();
    }
    for (const wire of layout.wires) {
      if (!scene.columns) break;
      const [jx, jy] = wire.junction;
      const heat = field.heatOf(wire.hub);
      context.fillStyle = rgbaString(colors.substrate, 1);
      context.strokeStyle = rgbaString(lerpRgbTuple(colors.ink, colors.alert, Math.min(1, heat * 1.5)), 0.85);
      context.fillRect(jx - 3.5, jy - scroll - 3.5, 7, 7);
      context.strokeRect(jx - 3.5, jy - scroll - 3.5, 7, 7);
    }
    for (const plate of layout.plates) {
      const alpha = related(plate.id) || plate.rows ? 1 : 0.42;
      drawPlate(plate, now, alpha, layout.tier.level);
    }
    const inspected = layout.plates.find((plate) => plate.id === view.inspected);
    if (inspected && !view.focus) {
      context.strokeStyle = rgbaString(colors.hot, 1);
      context.lineWidth = 2;
      context.strokeRect(inspected.x - 2, inspected.y - scroll - 2, inspected.w + 4, inspected.h + 4);
      context.lineWidth = 1;
    }
    if (view.focus) drawFocus(now);
    return warm;
  };

  const loop = createFrameLoop((now) => draw(now), isReduced);
  const repaint = (): void => {
    draw(performance.now());
    loop.wake();
  };
  const unsubscribe = field.subscribe(() => loop.wake());
  const local = (event: MouseEvent): [number, number] => {
    const rect = canvas.getBoundingClientRect();
    return [event.clientX - rect.left, event.clientY - rect.top + scroll];
  };
  const pointerMove = (event: PointerEvent): void => {
    const id = view.focus ? null : pickPlate(layout, ...local(event));
    canvas.style.cursor = id != null && byId.get(id)?.role === 'body' ? 'pointer' : 'default';
    if (id === hovered) return;
    hovered = id;
    if (id != null) onHover(id);
    repaint();
  };
  const pointerLeave = (): void => {
    hovered = null;
    repaint();
  };
  const click = (event: MouseEvent): void => {
    const id = view.focus ? null : pickPlate(layout, ...local(event));
    if (id != null && byId.get(id)?.role === 'body' && scene.columns) onSelect(id);
  };
  const wheel = (event: WheelEvent): void => {
    const overflow = Math.max(0, layout.height - size().height);
    if (overflow === 0) return;
    event.preventDefault();
    scroll = Math.max(0, Math.min(overflow, scroll + event.deltaY));
    repaint();
  };
  canvas.addEventListener('pointermove', pointerMove);
  canvas.addEventListener('pointerleave', pointerLeave);
  canvas.addEventListener('click', click);
  canvas.addEventListener('wheel', wheel, { passive: false });
  repaint();

  const reveal = (id: string | null): void => {
    const plate = id == null ? undefined : layout.plates.find((candidate) => candidate.id === id || candidate.rows?.some((row) => row.id === id));
    if (!plate) return;
    const { height } = size();
    if (plate.y - scroll < HEADER) scroll = Math.max(0, plate.y - HEADER);
    else if (plate.y + plate.h - scroll > height) scroll = plate.y + plate.h - height + 8;
  };

  return {
    setView(next) {
      view = next;
      reveal(next.inspected);
      repaint();
    },
    synapse(synapse) {
      synapses.push(synapse);
      if (synapses.length > 32) synapses.shift();
      loop.wake();
    },
    resize() {
      fitToContainer();
      layout = layoutFor();
      repaint();
    },
    // Plates never scale their text; zoom steps through the plate tiers by
    // scrolling instead, and focus mode is the close-up.
    zoom(factor) {
      scroll = Math.max(0, Math.min(Math.max(0, layout.height - size().height), scroll + (factor > 1 ? 120 : -120)));
      repaint();
    },
    fit() {
      scroll = 0;
      repaint();
    },
    retheme(next) {
      colors = next;
      repaint();
    },
    destroy() {
      loop.stop();
      unsubscribe();
      canvas.removeEventListener('pointermove', pointerMove);
      canvas.removeEventListener('pointerleave', pointerLeave);
      canvas.removeEventListener('click', click);
      canvas.removeEventListener('wheel', wheel);
      canvas.remove();
    },
  };
};

/** Clip text to a pixel width with an ellipsis, measured in the current font. */
function fit(value: string, width: number): string {
  // Mono at 11px is ~6.6px per glyph; clip by glyph count so no context is
  // needed and the result is deterministic.
  const glyphs = Math.max(4, Math.floor(width / 6.6));
  return value.length <= glyphs ? value : `${value.slice(0, glyphs - 1)}…`;
}
