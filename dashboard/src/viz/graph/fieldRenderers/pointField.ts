import { cssColorToRgb, lerpRgbTuple } from '../activation.ts';
import { kindColor } from '../kindColor.ts';
import {
  EasedCamera,
  SYNAPSE_TRAVEL_MS,
  attachCameraGestures,
  bow,
  createFrameLoop,
  createSpriteCache,
  drawColumns,
  drawGraticule,
  drawMassAxis,
  fitCamera,
  focusCamera,
  mountCanvas,
  rgbaString,
  toScreen,
  type CameraState,
  type FieldPalette,
  type FieldRendererFactory,
  type FieldScene,
  type FieldView,
  type SceneBody,
  type Synapse,
} from './scene.ts';

/**
 * Variant B, the point field: one additive point per indexed unit.
 *
 * A project body is a Vogel disc of its own stores (ice, at the core) and
 * artifacts (kind hue), so its area is its holdings and its luminance is the
 * density of what TraceDecay actually holds. Past {@link MAX_POINTS} units a
 * point stands for several, and the legend prints the ratio. Hand-written
 * Canvas2D: no WebGL context, one draw pass, frames only while something real
 * is unresolved.
 */
export const MAX_POINTS = 24_000;

const GOLDEN = Math.PI * (3 - Math.sqrt(5));
const PAD = { top: 44, right: 150, bottom: 28, left: 56 };

export interface PointCloud {
  /** Unit offsets inside a unit disc, interleaved x,y. */
  offsets: Float32Array;
  /** How many leading points are stores. */
  stores: number;
}

/** Units drawn per point for a whole scene, so every body shares one ratio. */
export function unitsPerPoint(scene: FieldScene): number {
  const total = scene.bodies.reduce(
    (sum, body) => sum + (body.units ? body.units.stores + body.units.artifacts : 0),
    0,
  );
  return Math.max(1, Math.ceil(total / MAX_POINTS));
}

/** A deterministic Vogel spiral, stores first, drawn slightly denser toward
 * the core so the stores read as a nucleus. The envelope, not the spread,
 * states the body's area. */
export function pointCloud(units: { stores: number; artifacts: number }, ratio: number): PointCloud {
  const stores = Math.ceil(units.stores / ratio);
  const count = stores + Math.ceil(units.artifacts / ratio);
  const offsets = new Float32Array(count * 2);
  for (let index = 0; index < count; index += 1) {
    const r = ((index + 0.5) / count) ** 0.7;
    const theta = index * GOLDEN;
    offsets[index * 2] = r * Math.cos(theta);
    offsets[index * 2 + 1] = r * Math.sin(theta);
  }
  return { offsets, stores };
}

/** The drawn body under a screen point, bodies before hubs. */
export function pickBody(
  scene: FieldScene,
  camera: CameraState,
  sx: number,
  sy: number,
): SceneBody | null {
  let best: SceneBody | null = null;
  let bestDistance = Infinity;
  for (const body of scene.bodies) {
    const [x, y] = toScreen(camera, body.x, body.y);
    const reach = body.role === 'hub' ? 9 : body.radius * camera.scale + 5;
    const distance = Math.hypot(sx - x, sy - y);
    if (distance <= reach && distance < bestDistance) {
      best = body;
      bestDistance = distance;
    }
  }
  return best;
}

function pointFieldCamera(scene: FieldScene, width: number, height: number): CameraState {
  return fitCamera(scene.extent, width, height, scene.columns ? PAD : { top: 24, right: 24, bottom: 24, left: 24 });
}

export const createPointField: FieldRendererFactory = ({
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
  const synapses: Synapse[] = [];
  const ratio = unitsPerPoint(scene);
  const clouds = new Map<string, PointCloud>();
  for (const body of scene.bodies) if (body.units) clouds.set(body.id, pointCloud(body.units, ratio));
  const kindRgb = new Map<string, [number, number, number]>();
  const hue = (kind: string): [number, number, number] => {
    let value = kindRgb.get(kind);
    if (!value) {
      value = cssColorToRgb(kindColor(kind, false));
      kindRgb.set(kind, value);
    }
    return value;
  };
  const byId = new Map(scene.bodies.map((body) => [body.id, body]));
  const sprites = createSpriteCache();
  const fitState = (): CameraState => pointFieldCamera(scene, size().width, size().height);
  /** The whole field, or the camera focus when one is set. */
  const framed = (): CameraState => {
    const members = view.focus ? scene.bodies.filter((body) => view.focus!.has(body.id)) : [];
    return focusCamera(members, size().width, size().height, 90) ?? fitState();
  };
  const camera = new EasedCamera(fitState());
  let fitScale = camera.current.scale;

  const lit = (id: string): boolean => {
    if (view.focus?.has(id)) return true;
    const anchor = hovered ?? view.inspected;
    if (anchor == null) return true;
    return id === anchor || (scene.neighbors.get(anchor)?.includes(id) ?? false);
  };
  const receded = (id: string): boolean => view.focus != null && !view.focus.has(id);

  /**
   * The resting field is cached in an offscreen layer and redrawn only when
   * the camera, the reader's view, the theme or the set of warm bodies
   * changes. A decay frame blits it and draws the heat alone, so frame cost
   * does not grow with the number of units on the field.
   */
  const layer = document.createElement('canvas');
  const layerContext = layer.getContext('2d');
  if (!layerContext) throw new Error('This browser has no 2D canvas context.');
  let dirty = true;
  let warmKey = '';
  const fitLayer = (): void => {
    layer.width = canvas.width;
    layer.height = canvas.height;
    const scale = canvas.width / Math.max(size().width, 1);
    layerContext.setTransform(scale, 0, 0, scale, 0, 0);
    dirty = true;
  };
  fitLayer();

  const renderStatic = (g: CanvasRenderingContext2D, cam: CameraState, zoom: number): void => {
    const { width, height } = size();
    g.globalCompositeOperation = 'source-over';
    drawGraticule(g, width, height, cam, colors);
    if (scene.columns) {
      drawColumns(g, width, height, cam, scene.columns, colors);
      drawMassAxis(g, cam, scene.extent, height, colors);
    }
    // Relations: hairline luminance, solid for exact, dashed for inferred.
    for (const path of scene.paths) {
      const a = byId.get(path.source);
      const b = byId.get(path.target);
      if (!a || !b) continue;
      const [ax, ay] = toScreen(cam, a.x, a.y);
      const [bx, by] = toScreen(cam, b.x, b.y);
      const dim = (lit(a.id) && lit(b.id) ? 1 : 0.25) * (receded(a.id) && receded(b.id) ? 0.2 : 1);
      g.setLineDash(path.grade === 'EXACT' ? [] : [3, 3]);
      g.lineWidth = 1;
      g.strokeStyle = rgbaString(colors.ink, (scene.columns ? 0.42 : 0.16) * dim);
      const [cx, cy] = bow(ax, ay, bx, by, path.relation);
      g.beginPath();
      g.moveTo(ax, ay);
      g.quadraticCurveTo(cx, cy, bx, by);
      g.stroke();
    }
    g.setLineDash([]);
    // Envelopes: one hairline frame per measured body, stating its area.
    for (const body of scene.bodies) {
      if (body.role !== 'body' || !body.units) continue;
      const [x, y] = toScreen(cam, body.x, body.y);
      g.strokeStyle = rgbaString(colors.edgeStrong, (receded(body.id) ? 0.08 : 0.3) * (lit(body.id) ? 1 : 0.4));
      g.lineWidth = 1;
      g.beginPath();
      g.arc(x, y, body.radius * cam.scale + 3, 0, Math.PI * 2);
      g.stroke();
    }
    // Units, additive, so luminance is density of real holdings.
    g.globalCompositeOperation = 'lighter';
    for (const body of scene.bodies) {
      if (body.role !== 'body') continue;
      const alpha = (0.35 + 0.65 * (body.vitality ?? 0.6)) * (lit(body.id) ? 1 : 0.22) * (receded(body.id) ? 0.14 : 1);
      drawUnits(g, cam, zoom, body, alpha, null);
    }
    g.globalAlpha = 1;
    g.globalCompositeOperation = 'source-over';
    // Hubs: hollow massless junctions.
    for (const body of scene.bodies) {
      if (body.role !== 'hub') continue;
      drawHub(g, cam, body, colors.ink, (lit(body.id) ? 0.85 : 0.3) * (receded(body.id) ? 0.2 : 1));
    }
    drawLabels(g, cam, zoom);
  };

  /** One body's units; `tint` re-draws them in amber for a warm body. */
  const drawUnits = (
    g: CanvasRenderingContext2D,
    cam: CameraState,
    zoom: number,
    body: SceneBody,
    alpha: number,
    tint: [number, number, number] | null,
  ): void => {
    const { width, height } = size();
    const [x, y] = toScreen(cam, body.x, body.y);
    const r = body.radius * cam.scale;
    if (x < -r || x > width + r || y < -r || y > height + r) return;
    const cloud = clouds.get(body.id);
    if (!cloud) {
      // A symbol body: one point sized by connectedness.
      g.globalAlpha = 0.9 * alpha;
      const d = Math.max(3, r * 2.6);
      g.drawImage(sprites.get(tint ?? hue(body.kind)), x - d / 2, y - d / 2, d, d);
      return;
    }
    const count = cloud.offsets.length / 2;
    const spacing = Math.sqrt((Math.PI * r * r) / Math.max(count, 1));
    const dot = Math.max(2.4, Math.min(7, spacing * 1.25 * Math.min(zoom, 2.5) ** 0.3));
    const half = dot / 2;
    let sprite = sprites.get(tint ?? colors.ink);
    g.globalAlpha = Math.min(1, alpha * 1.15);
    for (let index = 0; index < count; index += 1) {
      if (index === cloud.stores) {
        sprite = sprites.get(tint ?? hue(body.kind));
        g.globalAlpha = alpha * 0.8;
      }
      g.drawImage(sprite, x + cloud.offsets[index * 2]! * r - half, y - cloud.offsets[index * 2 + 1]! * r - half, dot, dot);
    }
  };

  const drawHub = (g: CanvasRenderingContext2D, cam: CameraState, body: SceneBody, rgb: [number, number, number], alpha: number): void => {
    const [x, y] = toScreen(cam, body.x, body.y);
    g.fillStyle = rgbaString(colors.substrate, 1);
    g.strokeStyle = rgbaString(rgb, alpha);
    g.lineWidth = 1.5;
    g.beginPath();
    g.arc(x, y, 5, 0, Math.PI * 2);
    g.fill();
    g.stroke();
    g.beginPath();
    g.arc(x, y, 1.5, 0, Math.PI * 2);
    g.fillStyle = g.strokeStyle;
    g.fill();
  };

  const draw = (now: number, deltaMs: number): boolean => {
    const warm = field.tick(now);
    const moving = camera.step(deltaMs);
    const { width, height } = size();
    const cam = camera.current;
    const zoom = cam.scale / fitScale;
    const nextWarmKey = scene.bodies.filter((body) => field.heatOf(body.id) > 0.02).map((body) => body.id).join('\u0000');
    if (moving || nextWarmKey !== warmKey) dirty = true;
    warmKey = nextWarmKey;
    if (dirty) {
      renderStatic(layerContext, cam, zoom);
      dirty = false;
    }
    context.globalCompositeOperation = 'copy';
    context.drawImage(layer, 0, 0, width, height);
    context.globalCompositeOperation = 'source-over';

    // Admitted activity: the exact touched identity blooms amber, and a
    // relation conducts only while both of its ends are warm.
    for (const path of scene.paths) {
      const heat = Math.min(field.heatOf(path.source), field.heatOf(path.target));
      const a = byId.get(path.source);
      const b = byId.get(path.target);
      if (heat <= 0.05 || !a || !b) continue;
      const [ax, ay] = toScreen(cam, a.x, a.y);
      const [bx, by] = toScreen(cam, b.x, b.y);
      const [cx, cy] = bow(ax, ay, bx, by, path.relation);
      context.lineWidth = 1 + heat;
      context.strokeStyle = rgbaString(lerpRgbTuple(colors.ink, colors.alert, Math.min(1, heat * 1.4)), 0.5 + 0.5 * heat);
      context.beginPath();
      context.moveTo(ax, ay);
      context.quadraticCurveTo(cx, cy, bx, by);
      context.stroke();
    }
    context.globalCompositeOperation = 'lighter';
    for (const body of scene.bodies) {
      const heat = field.heatOf(body.id);
      if (heat <= 0) continue;
      const [x, y] = toScreen(cam, body.x, body.y);
      const reach = Math.max(body.radius * cam.scale, 8) * (1.5 + heat);
      const gradient = context.createRadialGradient(x, y, 0, x, y, reach);
      gradient.addColorStop(0, rgbaString(colors.alert, 0.5 * heat));
      gradient.addColorStop(1, rgbaString(colors.alert, 0));
      context.fillStyle = gradient;
      context.beginPath();
      context.arc(x, y, reach, 0, Math.PI * 2);
      context.fill();
      if (body.role === 'body') drawUnits(context, cam, zoom, body, 0.8 * heat, colors.alert);
    }
    context.globalAlpha = 1;
    let travelling = false;
    if (!isReduced()) {
      for (const synapse of synapses) {
        const age = now - synapse.at;
        const a = byId.get(synapse.from);
        const b = synapse.to != null ? byId.get(synapse.to) : undefined;
        if (age < 0 || age > SYNAPSE_TRAVEL_MS || !a || !b) continue;
        travelling = true;
        const t = age / SYNAPSE_TRAVEL_MS;
        const [ax, ay] = toScreen(cam, a.x, a.y);
        const [bx, by] = toScreen(cam, b.x, b.y);
        const [cx, cy] = bow(ax, ay, bx, by, 'checkout');
        const u = 1 - t;
        context.fillStyle = rgbaString(colors.alert, 0.95);
        context.beginPath();
        context.arc(u * u * ax + 2 * u * t * cx + t * t * bx, u * u * ay + 2 * u * t * cy + t * t * by, 3, 0, Math.PI * 2);
        context.fill();
      }
    }
    context.globalCompositeOperation = 'source-over';
    for (const body of scene.bodies) {
      const heat = field.heatOf(body.id);
      if (body.role === 'hub' && heat > 0) drawHub(context, cam, body, lerpRgbTuple(colors.ink, colors.alert, Math.min(1, heat * 1.4)), 1);
    }
    // Inspection: a 2px cyan ring, never a glow.
    const inspected = view.inspected != null ? byId.get(view.inspected) : undefined;
    if (inspected) {
      const [x, y] = toScreen(cam, inspected.x, inspected.y);
      context.strokeStyle = rgbaString(colors.hot, 1);
      context.lineWidth = 2;
      context.beginPath();
      context.arc(x, y, inspected.role === 'hub' ? 9 : inspected.radius * cam.scale + 6, 0, Math.PI * 2);
      context.stroke();
    }
    return warm || moving || travelling;
  };

  const drawLabels = (g: CanvasRenderingContext2D, cam: CameraState, zoom: number): void => {
    const occupied: Array<[number, number, number, number]> = [];
    const free = (x: number, y: number, w: number, h: number): boolean =>
      !occupied.some(([ox, oy, ow, oh]) => x < ox + ow && x + w > ox && y < oy + oh && y + h > oy);
    const anchor = hovered ?? view.inspected;
    const struck = new Set(synapses.filter((synapse) => field.heatOf(synapse.from) > 0.02).map((synapse) => synapse.from));
    const priority = (body: SceneBody): number =>
      (body.id === anchor ? 1e9 : 0) +
      (struck.has(body.id) ? 1e8 : 0) +
      (view.focus?.has(body.id) ? 1e7 : 0) +
      (body.role === 'hub' ? 0.5 : (body.mass ?? 0) + 1);
    const ranked = [...scene.bodies].sort((a, b) => priority(b) - priority(a));
    const detailed = zoom >= 1.6 || view.focus != null;
    const { width: fieldWidth, height: fieldHeight } = size();
    g.textAlign = 'left';
    for (const body of ranked) {
      if (receded(body.id) && body.id !== anchor) continue;
      const [x, y] = toScreen(cam, body.x, body.y);
      if (x < -40 || x > fieldWidth || y < -20 || y > fieldHeight + 20) continue;
      const r = body.role === 'hub' ? 6 : body.radius * cam.scale * 1.12 + 4;
      const lines = [body.role === 'hub' ? `repo:${body.label}` : body.label];
      if (body.role === 'hub') lines.push('hub · massless');
      else if (detailed || body.id === anchor) lines.push(...body.detail.slice(0, 3));
      else if (scene.columns) lines.push(body.detail[2] ?? '');
      const strike = struck.has(body.id) ? [...synapses].reverse().find((synapse) => synapse.from === body.id) : undefined;
      if (strike) lines.push(`${strike.label} · ${strike.time}`);
      // Mono glyphs have one advance, so width needs no measurement per frame.
      const width = Math.max(...lines.map((line) => line.length)) * 6.7;
      const height = 13 * lines.length;
      const lx = body.role === 'hub' ? x - width / 2 : x + r;
      const ly = body.role === 'hub' ? y + 12 : y - 6;
      if (lx + width > fieldWidth - 4 || !free(lx - 2, ly - 2, width + 6, height + 4)) {
        if (body.id !== anchor) continue;
      }
      occupied.push([lx - 2, ly - 2, width + 6, height + 4]);
      const alpha = lit(body.id) ? 1 : 0.4;
      g.fillStyle = rgbaString(colors.substrate, 0.62 * alpha);
      g.fillRect(lx - 3, ly, width + 6, height + 2);
      lines.forEach((line, index) => {
        g.font = index === 0 ? `600 11px ${colors.labelFont}` : `400 10px ${colors.labelFont}`;
        g.fillStyle =
          strike && index === lines.length - 1
            ? rgbaString(colors.alert, 0.95)
            : index === 0
              ? rgbaString(body.id === view.inspected ? colors.hot : colors.ink, 0.94 * alpha)
              : rgbaString(colors.inkMuted, 0.92 * alpha);
        g.fillText(line, lx, ly + 10 + index * 13);
      });
    }
  };

  const loop = createFrameLoop(draw, isReduced);
  const repaint = (): void => {
    draw(performance.now(), 0);
    loop.wake();
  };
  const unsubscribe = field.subscribe(() => loop.wake());
  const releaseGestures = attachCameraGestures(canvas, camera, () => {
    dirty = true;
    loop.wake();
  });
  let downAt: { x: number; y: number } | null = null;
  const pointerDown = (event: PointerEvent): void => {
    downAt = { x: event.clientX, y: event.clientY };
  };
  const pointerMove = (event: PointerEvent): void => {
    if (event.buttons !== 0) return;
    const rect = canvas.getBoundingClientRect();
    const body = pickBody(scene, camera.current, event.clientX - rect.left, event.clientY - rect.top);
    const next = body?.id ?? null;
    canvas.style.cursor = body?.role === 'body' ? 'pointer' : 'default';
    if (next === hovered) return;
    hovered = next;
    dirty = true;
    if (next != null) onHover(next);
    repaint();
  };
  const pointerLeave = (): void => {
    if (hovered == null) return;
    hovered = null;
    dirty = true;
    repaint();
  };
  const click = (event: MouseEvent): void => {
    if (downAt && Math.hypot(event.clientX - downAt.x, event.clientY - downAt.y) >= 4) return;
    const rect = canvas.getBoundingClientRect();
    const body = pickBody(scene, camera.current, event.clientX - rect.left, event.clientY - rect.top);
    if (body?.role === 'body') onSelect(body.id);
  };
  canvas.addEventListener('pointerdown', pointerDown);
  canvas.addEventListener('pointermove', pointerMove);
  canvas.addEventListener('pointerleave', pointerLeave);
  canvas.addEventListener('click', click);
  repaint();

  return {
    setView(next) {
      const focusChanged = next.focus !== view.focus;
      view = next;
      dirty = true;
      if (focusChanged) camera.set(framed(), isReduced());
      repaint();
    },
    synapse(synapse) {
      synapses.push(synapse);
      if (synapses.length > 32) synapses.shift();
      loop.wake();
    },
    resize() {
      fitToContainer();
      fitLayer();
      fitScale = fitState().scale;
      camera.set(framed(), true);
      repaint();
    },
    zoom(factor) {
      const { width, height } = size();
      const t = camera.target;
      const [wx, wy] = [(width / 2 - t.tx) / t.scale, (t.ty - height / 2) / t.scale];
      const scale = t.scale * factor;
      camera.set({ scale, tx: width / 2 - wx * scale, ty: height / 2 + wy * scale }, isReduced());
      dirty = true;
      repaint();
    },
    fit() {
      camera.set(fitState(), isReduced());
      dirty = true;
      repaint();
    },
    retheme(next) {
      colors = next;
      dirty = true;
      repaint();
    },
    destroy() {
      loop.stop();
      unsubscribe();
      releaseGestures();
      canvas.removeEventListener('pointerdown', pointerDown);
      canvas.removeEventListener('pointermove', pointerMove);
      canvas.removeEventListener('pointerleave', pointerLeave);
      canvas.removeEventListener('click', click);
      canvas.remove();
    },
  };
};