import Graph from 'graphology';
import Sigma from 'sigma';
import { NodeCircleProgram, type NodeHoverDrawingFunction, type NodeLabelDrawingFunction } from 'sigma/rendering';
import { cssColorToRgb, lerpRgbTuple, restingNodeTint } from '../activation.ts';
import { kindColor } from '../kindColor.ts';
import { hasWebGl } from '../renderer.ts';
import {
  SYNAPSE_TRAVEL_MS,
  bow,
  createFrameLoop,
  drawColumns,
  drawGraticule,
  drawMassAxis,
  mountCanvas,
  rgbaString,
  toScreen,
  type CameraState,
  type FieldPalette,
  type FieldRendererFactory,
  type FieldView,
  type Synapse,
} from './scene.ts';

/**
 * Variant A, Sigma refined: Sigma keeps bodies, picking, labels and camera;
 * a kind-hued core with a thin ice rim replaces the flat disc. Relations,
 * the graticule and admitted heat are drawn on a 2D underlay from Sigma's
 * own viewport transform, so they can curve by relation kind and bloom
 * additively without extra WebGL programs.
 */
class RimProgram extends NodeCircleProgram {
  override getDefinition() {
    return {
      ...super.getDefinition(),
      FRAGMENT_SHADER_SOURCE: `
precision highp float;
varying vec4 v_color;
varying vec2 v_diffVector;
varying float v_radius;
uniform float u_correctionRatio;
const vec4 transparent = vec4(0.0, 0.0, 0.0, 0.0);
const vec3 ice = vec3(0.86, 0.93, 1.0);
void main(void) {
  float border = u_correctionRatio * 2.0;
  float d = length(v_diffVector);
  float dist = d - v_radius + border;
  #ifdef PICKING_MODE
    gl_FragColor = dist > border ? transparent : v_color;
  #else
    float t = dist > border ? 1.0 : (dist > 0.0 ? dist / border : 0.0);
    float rimWidth = max(border * 0.6, v_radius * 0.05);
    float rim = smoothstep(v_radius - rimWidth - border, v_radius - rimWidth, d);
    float shade = 0.5 + 0.5 * sqrt(max(0.0, 1.0 - d / max(v_radius, 0.0001)));
    vec3 rgb = mix(v_color.rgb * shade, ice, rim * 0.62);
    gl_FragColor = mix(vec4(rgb * v_color.a, v_color.a), transparent, t);
  #endif
}`,
    };
  }
}

export const createSigmaField: FieldRendererFactory = ({
  container,
  scene,
  field,
  palette,
  isReduced,
  onHover,
  onSelect,
}) => {
  if (!hasWebGl()) throw new Error('This browser has no WebGL context.');
  const under = mountCanvas(container);
  if (!under) throw new Error('This browser has no 2D canvas context.');
  under.canvas.style.pointerEvents = 'none';
  let colors: FieldPalette = palette;
  let view: FieldView = { inspected: null, focus: null };
  let hovered: string | null = null;
  const synapses: Synapse[] = [];
  const byId = new Map(scene.bodies.map((body) => [body.id, body]));

  const graph = new Graph();
  for (const body of scene.bodies) {
    graph.addNode(body.id, {
      x: body.x,
      y: body.y,
      size: body.radius,
      label: body.role === 'hub' ? `repo:${body.label}` : body.label,
      type: 'rim',
      role: body.role,
    });
  }
  const tint = (id: string): [number, number, number] => {
    const body = byId.get(id);
    if (!body) return colors.dim;
    if (body.role === 'hub') return colors.edgeStrong;
    return restingNodeTint(colors.substrate, cssColorToRgb(kindColor(body.kind, false)), body.vitality ?? 0.6, false);
  };
  const lit = (id: string): boolean => {
    if (view.focus?.has(id)) return true;
    const anchor = hovered ?? view.inspected;
    if (anchor == null) return true;
    return id === anchor || (scene.neighbors.get(anchor)?.includes(id) ?? false);
  };
  const receded = (id: string): boolean => view.focus != null && !view.focus.has(id);

  /** Set once constructed; Sigma draws labels during its own constructor. */
  let live: Sigma | null = null;
  const drawLabel: NodeLabelDrawingFunction = (context, data, settings) => {
    const body = byId.get(data.key);
    if (!body || !data.label) return;
    const zoomed = (live?.getCamera().ratio ?? 1) < 0.62 || view.focus != null;
    const lines = [data.label];
    if (body.role === 'hub') lines.push('hub · massless');
    else lines.push(...(zoomed || body.id === view.inspected ? body.detail.slice(0, 3) : body.detail.slice(2, 3)));
    const strike = synapses.find((synapse) => synapse.from === body.id);
    const heat = field.heatOf(body.id);
    if (strike && heat > 0.02) lines.push(`${strike.label} · ${strike.time}`);
    const x = data.x + data.size + 5;
    const y = data.y - 6;
    context.font = `600 ${settings.labelSize}px ${settings.labelFont}`;
    const width = Math.max(...lines.map((line) => context.measureText(line).width));
    const alpha = lit(body.id) ? 1 : 0.35;
    context.fillStyle = rgbaString(colors.substrate, 0.66 * alpha);
    context.fillRect(x - 3, y, width + 6, lines.length * 13 + 2);
    lines.forEach((line, index) => {
      context.font = index === 0 ? `600 11px ${settings.labelFont}` : `400 10px ${settings.labelFont}`;
      const amber = strike && heat > 0.02 && index === lines.length - 1;
      context.fillStyle = amber
        ? rgbaString(colors.alert, 0.95)
        : rgbaString(index === 0 ? (body.id === view.inspected ? colors.hot : colors.ink) : colors.inkMuted, 0.94 * alpha);
      context.fillText(line, x, y + 10 + index * 13);
    });
  };
  const drawHover: NodeHoverDrawingFunction = (context, data, settings) => {
    context.strokeStyle = rgbaString(colors.hot, 1);
    context.lineWidth = 2;
    context.beginPath();
    context.arc(data.x, data.y, data.size + 4, 0, Math.PI * 2);
    context.stroke();
    drawLabel(context, data, settings);
  };

  const sigma = new Sigma(graph, container, {
    nodeProgramClasses: { rim: RimProgram },
    defaultNodeType: 'rim',
    renderEdgeLabels: false,
    itemSizesReference: 'positions',
    zoomToSizeRatioFunction: (ratio) => ratio,
    labelFont: colors.labelFont,
    labelSize: 11,
    labelWeight: '600',
    labelDensity: 0.9,
    labelGridCellSize: 110,
    labelRenderedSizeThreshold: 0,
    defaultDrawNodeLabel: drawLabel,
    defaultDrawNodeHover: drawHover,
    zIndex: true,
    stagePadding: 48,
    allowInvalidContainer: true,
    minCameraRatio: 0.08,
    maxCameraRatio: 3,
    nodeReducer: (node, data) => {
      const heat = field.heatOf(node);
      let rgb = tint(node);
      if (heat > 0) rgb = lerpRgbTuple(rgb, colors.alert, Math.min(0.55, heat * 0.6));
      const fade = (lit(node) ? 1 : 0.3) * (receded(node) ? 0.16 : 1);
      const forced = node === view.inspected || node === hovered || (heat > 0.02 && synapses.some((s) => s.from === node));
      return {
        ...data,
        color: `rgba(${rgb[0]}, ${rgb[1]}, ${rgb[2]}, ${fade})`,
        forceLabel: forced,
        label: receded(node) && !forced ? '' : data.label,
        zIndex: forced ? 2 : 1,
      };
    },
  });
  live = sigma;
  sigma.setCustomBBox({ x: scene.extent.x, y: scene.extent.y });
  const over = mountCanvas(container);
  if (!over) {
    sigma.kill();
    under.canvas.remove();
    throw new Error('This browser has no 2D canvas context.');
  }
  over.canvas.style.pointerEvents = 'none';

  /** Sigma's current graph-to-viewport map, as the linear camera the 2D
   * layers share. */
  const cameraNow = (): CameraState => {
    const origin = sigma.graphToViewport({ x: 0, y: 0 });
    const unitX = sigma.graphToViewport({ x: 1, y: 0 });
    return { scale: unitX.x - origin.x, tx: origin.x, ty: origin.y };
  };

  const paintUnder = (now: number): boolean => {
    const { width, height } = under.size();
    const context = under.context;
    const cam = cameraNow();
    drawGraticule(context, width, height, cam, colors);
    if (scene.columns) {
      drawColumns(context, width, height, cam, scene.columns, colors);
      drawMassAxis(context, cam, scene.extent, height, colors);
    }
    for (const path of scene.paths) {
      const a = byId.get(path.source);
      const b = byId.get(path.target);
      if (!a || !b) continue;
      const [ax, ay] = toScreen(cam, a.x, a.y);
      const [bx, by] = toScreen(cam, b.x, b.y);
      const heat = Math.min(field.heatOf(a.id), field.heatOf(b.id));
      const fade = (lit(a.id) && lit(b.id) ? 1 : 0.25) * (receded(a.id) && receded(b.id) ? 0.2 : 1);
      const [cx, cy] = bow(ax, ay, bx, by, path.relation);
      context.setLineDash(path.grade === 'EXACT' ? [] : [3, 3]);
      context.lineWidth = heat > 0.05 ? 1 + heat : 0.8;
      context.strokeStyle =
        heat > 0.05
          ? rgbaString(lerpRgbTuple(colors.ink, colors.alert, Math.min(1, heat * 1.4)), (0.5 + 0.5 * heat) * fade)
          : rgbaString(colors.ink, (scene.columns ? 0.4 : 0.14) * fade);
      context.beginPath();
      context.moveTo(ax, ay);
      context.quadraticCurveTo(cx, cy, bx, by);
      context.stroke();
    }
    context.setLineDash([]);
    context.globalCompositeOperation = 'lighter';
    for (const body of scene.bodies) {
      const heat = field.heatOf(body.id);
      if (heat <= 0) continue;
      const [x, y] = toScreen(cam, body.x, body.y);
      const reach = Math.max(body.radius * cam.scale, 8) * (1.7 + heat);
      const gradient = context.createRadialGradient(x, y, 0, x, y, reach);
      gradient.addColorStop(0, rgbaString(colors.alert, 0.55 * heat));
      gradient.addColorStop(1, rgbaString(colors.alert, 0));
      context.fillStyle = gradient;
      context.fillRect(x - reach, y - reach, reach * 2, reach * 2);
    }
    context.globalCompositeOperation = 'source-over';

    const top = over.context;
    top.clearRect(0, 0, width, height);
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
        top.fillStyle = rgbaString(colors.alert, 0.95);
        top.beginPath();
        top.arc(u * u * ax + 2 * u * t * cx + t * t * bx, u * u * ay + 2 * u * t * cy + t * t * by, 3, 0, Math.PI * 2);
        top.fill();
      }
    }
    const inspected = view.inspected != null && view.inspected !== hovered ? byId.get(view.inspected) : undefined;
    if (inspected) {
      const [x, y] = toScreen(cam, inspected.x, inspected.y);
      top.strokeStyle = rgbaString(colors.hot, 1);
      top.lineWidth = 2;
      top.beginPath();
      top.arc(x, y, inspected.radius * cam.scale + 4, 0, Math.PI * 2);
      top.stroke();
    }
    return travelling;
  };

  let travelling = false;
  sigma.on('afterRender', () => {
    travelling = paintUnder(performance.now());
  });
  const loop = createFrameLoop((now) => {
    const warm = field.tick(now);
    sigma.refresh();
    return warm || travelling;
  }, isReduced);
  const unsubscribe = field.subscribe(() => loop.wake());

  sigma.on('enterNode', ({ node }) => {
    hovered = node;
    container.style.cursor = byId.get(node)?.role === 'body' ? 'pointer' : 'default';
    onHover(node);
    sigma.refresh();
  });
  sigma.on('leaveNode', () => {
    hovered = null;
    container.style.cursor = 'default';
    sigma.refresh();
  });
  sigma.on('clickNode', ({ node }) => {
    if (byId.get(node)?.role === 'body') onSelect(node);
  });

  const cameraTo = (state: { x: number; y: number; ratio: number }): void => {
    const camera = sigma.getCamera();
    if (isReduced()) camera.setState(state);
    else void camera.animate(state, { duration: 260 });
  };

  return {
    setView(next) {
      const focusChanged = next.focus !== view.focus;
      view = next;
      if (focusChanged) {
        const members = next.focus ? [...next.focus].flatMap((id) => sigma.getNodeDisplayData(id) ?? []) : [];
        if (members.length > 0) {
          const xs = members.map((member) => member.x);
          const ys = members.map((member) => member.y);
          const span = Math.max(Math.max(...xs) - Math.min(...xs), Math.max(...ys) - Math.min(...ys), 0.05);
          cameraTo({
            x: (Math.max(...xs) + Math.min(...xs)) / 2,
            y: (Math.max(...ys) + Math.min(...ys)) / 2,
            ratio: Math.min(1, span * 2.2),
          });
        } else cameraTo({ x: 0.5, y: 0.5, ratio: 1 });
      }
      sigma.refresh();
    },
    synapse(synapse) {
      synapses.push(synapse);
      if (synapses.length > 32) synapses.shift();
      loop.wake();
    },
    resize() {
      under.fitToContainer();
      over.fitToContainer();
      sigma.resize();
      sigma.refresh();
    },
    zoom(factor) {
      const camera = sigma.getCamera();
      cameraTo({ x: camera.x, y: camera.y, ratio: camera.getBoundedRatio(camera.ratio / factor) });
    },
    fit() {
      cameraTo({ x: 0.5, y: 0.5, ratio: 1 });
    },
    retheme(next) {
      colors = next;
      sigma.refresh();
    },
    destroy() {
      loop.stop();
      unsubscribe();
      sigma.kill();
      under.canvas.remove();
      over.canvas.remove();
    },
  };
};

