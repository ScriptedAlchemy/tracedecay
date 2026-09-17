/**
 * The Three.js runtime for the registry field: one orthographic 2.5D scene
 * that draws the luminous bodies, their hanging filaments, the evidenced
 * checkout paths, and the heat that admitted activity leaves on them.
 *
 * This module draws and decides nothing about the registry. Positions, radii,
 * relations and depth arrive in a `RegistrySceneModel`; heat arrives through
 * the `ActivationField` the page strikes when a real event lands; hover and
 * emphasis arrive as identities. The runtime samples all of it per frame and
 * runs a frame loop only while something real is unresolved — warm heat, an
 * easing focus, a camera in flight — then stops. Under reduced motion no loop
 * runs at all: every state change composes one static frame.
 *
 * Geometry is body-local. Every vertex carries the centre of the body it
 * belongs to, and the vertex shader places it at `centre.x * spread + local`,
 * so the categorical column spacing can follow the aperture's aspect without
 * stretching a single crown and without rebuilding a buffer.
 *
 * Renderer-specific code stays here so the model, the camera arithmetic and
 * the label priority can be exercised without a GPU, and so the React host can
 * substitute this whole module in a DOM test.
 */
import {
  AdditiveBlending,
  BufferGeometry,
  DataTexture,
  Float32BufferAttribute,
  LineBasicMaterial,
  LineLoop,
  LineSegments,
  NearestFilter,
  OrthographicCamera,
  Points,
  RGBAFormat,
  SRGBColorSpace,
  Scene,
  ShaderMaterial,
  UnsignedByteType,
  Vector3,
  WebGLRenderer,
} from 'three';
import {
  approach,
  cssColorToRgb,
  settled,
  type ActivationField,
} from '../graph/activation.ts';
import { kindColor } from '../graph/kindColor.ts';
import { palette, type GraphPalette } from '../graph/palette.ts';
import { buildNeuralBody } from './neuralBody.ts';
import {
  bodiesBounds,
  columnSpread,
  pickBody,
  samplePath,
  spreadExtent,
  type RegistrySceneModel,
  type SceneBody,
} from './registrySceneModel.ts';
import {
  cameraEquals,
  clampZoom,
  fitBounds,
  panBy,
  unproject,
  zoomAbout,
  type CameraState,
  type Viewport,
} from './sceneCamera.ts';
import { bodyState, channelByte, wantsNextFrame } from './sceneState.ts';

export interface SceneView {
  readonly camera: CameraState;
  readonly viewport: Viewport;
  /** Column spread in force; overlays must place centres at `x * spread`. */
  readonly spread: number;
  /** The Fit camera for this viewport, for zoom readouts. */
  readonly fit: CameraState;
}

export interface RegistryRuntimeOptions {
  readonly container: HTMLElement;
  readonly model: RegistrySceneModel;
  /** Admitted heat. The runtime only ever reads it. */
  readonly field: ActivationField;
  readonly isReduced: () => boolean;
  /** The camera moved (fit, zoom, pan, animation frame) or the box changed. */
  readonly onView: (view: SceneView) => void;
}

export interface RegistryRuntime {
  readonly canvas: HTMLCanvasElement;
  resize(viewport: Viewport): void;
  /** Hover/keyboard inspection. Raises one body and dims the rest. Never heat. */
  focus(id: string | null): void;
  /** Camera focus on a set of bodies; everything else recedes to context.
   * `null` returns to the whole field. */
  emphasize(ids: ReadonlySet<string> | null): void;
  fit(): void;
  zoomIn(): void;
  zoomOut(): void;
  /** Pointer-centred zoom; `factor` > 1 closes in. */
  zoomAt(factor: number, anchor: { px: number; py: number }): void;
  panBy(dxPx: number, dyPx: number): void;
  view(): SceneView;
  pick(px: number, py: number): SceneBody | null;
  /** Something real changed off-frame (a strike landed); draw it. */
  wake(): void;
  /** Compose the no-motion frame and stop any loop. */
  settle(): void;
  retheme(): void;
  dispose(): void;
}

const CAMERA_PAD_PX = 34;
const HOP_PULSE_PERIOD_MS = 1100;

const STATE_SAMPLE = /* glsl */ `
  uniform sampler2D uState;
  uniform float uBodies;
  vec4 bodyState(float body) {
    return texture2D(uState, vec2((body + 0.5) / uBodies, 0.5));
  }
`;

const PLACE = /* glsl */ `
  uniform float uSpread;
  vec3 place(vec3 local, vec2 centre) {
    return vec3(centre.x * uSpread + local.x, centre.y + local.y, local.z);
  }
`;

const DUST_VERTEX = /* glsl */ `
  attribute vec2 aCenter;
  attribute float aBody;
  attribute float aAlpha;
  attribute float aSize;
  attribute float aFlare;
  attribute vec3 aColor;
  uniform float uPixelsPerUnit;
  uniform float uMaxPointSize;
  uniform vec3 uHot;
  uniform vec3 uAlert;
  uniform vec3 uDim;
  varying vec4 vColor;
  ${STATE_SAMPLE}
  ${PLACE}
  void main() {
    vec4 s = bodyState(aBody);
    float heat = s.r;
    float dim = s.g;
    float raise = s.b;
    float vitality = s.a;
    vec3 tint = mix(aColor, uAlert, heat * 0.85);
    tint = mix(tint, uHot, raise * 0.3);
    tint = mix(tint, uDim, dim * 0.75);
    float resting = aAlpha
      * (0.32 + 0.68 * vitality)
      * (1.0 - 0.82 * dim)
      * (1.0 + 1.6 * heat)
      * (1.0 + 0.35 * raise);
    // A flare sprite exists only while its body is warm: the impact of an
    // admitted event, widening as the heat decays.
    float flare = aAlpha * heat * (1.0 - 0.82 * dim);
    vColor = vec4(mix(tint, uAlert, aFlare), mix(resting, flare, aFlare));
    gl_Position = projectionMatrix * modelViewMatrix * vec4(place(position, aCenter), 1.0);
    float grow = mix(1.0 + 0.6 * heat + 0.1 * raise, 0.7 + 1.1 * (1.0 - heat), aFlare);
    float px = aSize * uPixelsPerUnit * grow;
    gl_PointSize = clamp(px, 0.75, uMaxPointSize);
  }
`;

const DUST_FRAGMENT = /* glsl */ `
  varying vec4 vColor;
  void main() {
    vec2 d = gl_PointCoord - vec2(0.5);
    float r = length(d) * 2.0;
    if (r > 1.0) discard;
    float a = 1.0 - r;
    a = a * a;
    gl_FragColor = vec4(vColor.rgb, vColor.a * a);
  }
`;

const LINE_VERTEX = /* glsl */ `
  attribute vec2 aCenter;
  attribute float aBody;
  attribute float aAlpha;
  attribute vec3 aColor;
  uniform vec3 uHot;
  uniform vec3 uAlert;
  uniform vec3 uDim;
  varying vec4 vColor;
  ${STATE_SAMPLE}
  ${PLACE}
  void main() {
    vec4 s = bodyState(aBody);
    float heat = s.r;
    float dim = s.g;
    float raise = s.b;
    float vitality = s.a;
    vec3 tint = mix(aColor, uAlert, heat * 0.8);
    tint = mix(tint, uHot, raise * 0.25);
    tint = mix(tint, uDim, dim * 0.75);
    float alpha = aAlpha * (0.38 + 0.62 * vitality) * (1.0 - 0.85 * dim) * (1.0 + 0.7 * heat);
    vColor = vec4(tint, alpha);
    gl_Position = projectionMatrix * modelViewMatrix * vec4(place(position, aCenter), 1.0);
  }
`;

const LINE_FRAGMENT = /* glsl */ `
  varying vec4 vColor;
  void main() {
    gl_FragColor = vColor;
  }
`;

const PATH_VERTEX = /* glsl */ `
  attribute float aFrom;
  attribute float aTo;
  uniform vec3 uEdge;
  uniform vec3 uAlert;
  uniform vec3 uDim;
  varying vec4 vColor;
  ${STATE_SAMPLE}
  void main() {
    vec4 a = bodyState(aFrom);
    vec4 b = bodyState(aTo);
    // A relation conducts only when both of its ends are warm: that is what
    // makes it a synapse and not a wire.
    float conduct = min(a.r, b.r);
    float dim = min(a.g, b.g);
    float vitality = (a.a + b.a) * 0.5;
    vec3 tint = mix(uEdge, uAlert, min(1.0, conduct * 1.6));
    tint = mix(tint, uDim, dim * 0.7);
    float alpha = (0.26 + 0.34 * vitality) * (1.0 - 0.85 * dim) + 0.7 * conduct;
    vColor = vec4(tint, min(1.0, alpha));
    gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
  }
`;

interface StateChannels {
  readonly texture: DataTexture;
  readonly data: Uint8Array;
}

function unit([r, g, b]: [number, number, number]): [number, number, number] {
  return [r / 255, g / 255, b / 255];
}

function createState(count: number): StateChannels {
  const data = new Uint8Array(Math.max(1, count) * 4);
  const texture = new DataTexture(data, Math.max(1, count), 1, RGBAFormat, UnsignedByteType);
  texture.magFilter = NearestFilter;
  texture.minFilter = NearestFilter;
  texture.generateMipmaps = false;
  texture.needsUpdate = true;
  return { texture, data };
}

/** The renderer's own largest point sprite. Glow anchors above it render at
 * the cap rather than failing, so a GPU with a small range loses a little
 * bloom and nothing else. */
function maxPointSize(renderer: WebGLRenderer): number {
  const gl = renderer.getContext();
  const range = gl.getParameter(gl.ALIASED_POINT_SIZE_RANGE) as Float32Array | null;
  const max = range?.[1];
  return typeof max === 'number' && max > 1 ? Math.min(max, 512) : 64;
}

class Batch {
  readonly positions: number[] = [];
  readonly centers: number[] = [];
  readonly colors: number[] = [];
  readonly alphas: number[] = [];
  readonly sizes: number[] = [];
  readonly bodies: number[] = [];
  readonly flares: number[] = [];

  push(
    body: number,
    center: readonly [number, number],
    x: number,
    y: number,
    z: number,
    color: readonly [number, number, number],
    alpha: number,
    size = 0,
    flare = 0,
  ): void {
    this.positions.push(x, y, z);
    this.centers.push(center[0], center[1]);
    this.colors.push(color[0], color[1], color[2]);
    this.alphas.push(alpha);
    this.sizes.push(size);
    this.bodies.push(body);
    this.flares.push(flare);
  }

  geometry(withSize: boolean): BufferGeometry {
    const geometry = new BufferGeometry();
    geometry.setAttribute('position', new Float32BufferAttribute(this.positions, 3));
    geometry.setAttribute('aCenter', new Float32BufferAttribute(this.centers, 2));
    geometry.setAttribute('aColor', new Float32BufferAttribute(this.colors, 3));
    geometry.setAttribute('aAlpha', new Float32BufferAttribute(this.alphas, 1));
    geometry.setAttribute('aBody', new Float32BufferAttribute(this.bodies, 1));
    if (withSize) {
      geometry.setAttribute('aSize', new Float32BufferAttribute(this.sizes, 1));
      geometry.setAttribute('aFlare', new Float32BufferAttribute(this.flares, 1));
    }
    return geometry;
  }
}

export function createRegistryRuntime(options: RegistryRuntimeOptions): RegistryRuntime {
  const { container, model, field, isReduced, onView } = options;
  const canvas = document.createElement('canvas');
  canvas.style.position = 'absolute';
  canvas.style.inset = '0';
  canvas.style.width = '100%';
  canvas.style.height = '100%';
  canvas.style.display = 'block';
  container.appendChild(canvas);

  const renderer = new WebGLRenderer({
    canvas,
    antialias: true,
    alpha: true,
    premultipliedAlpha: true,
    powerPreference: 'low-power',
  });
  const pixelRatio = Math.min(2, window.devicePixelRatio || 1);
  renderer.setPixelRatio(pixelRatio);
  renderer.setClearColor(0x000000, 0);
  const pointCap = maxPointSize(renderer);

  let colors: GraphPalette = palette(container);
  const scene = new Scene();
  const camera3 = new OrthographicCamera(-1, 1, 1, -1, 0.1, 50);
  camera3.position.set(0, 0, 10);
  camera3.lookAt(new Vector3(0, 0, 0));

  const bodies = model.bodies;
  const index = new Map(bodies.map((body, position) => [body.id, position] as const));
  const state = createState(bodies.length);

  const uniforms = {
    uState: { value: state.texture },
    uBodies: { value: Math.max(1, bodies.length) },
    uSpread: { value: 1 },
    uPixelsPerUnit: { value: 1 },
    uMaxPointSize: { value: pointCap },
    uHot: { value: unit(colors.hot) },
    uAlert: { value: unit(colors.alert) },
    uDim: { value: unit(colors.dim) },
    uEdge: { value: unit(colors.edge) },
  };
  const additive = {
    uniforms,
    blending: AdditiveBlending,
    transparent: true,
    depthTest: false,
    depthWrite: false,
  };
  const dustMaterial = new ShaderMaterial({ ...additive, vertexShader: DUST_VERTEX, fragmentShader: DUST_FRAGMENT });
  const lineMaterial = new ShaderMaterial({ ...additive, vertexShader: LINE_VERTEX, fragmentShader: LINE_FRAGMENT });
  const pathMaterial = new ShaderMaterial({ ...additive, vertexShader: PATH_VERTEX, fragmentShader: LINE_FRAGMENT });

  // ---- geometry -----------------------------------------------------------
  const dustBatch = new Batch();
  const glowBatch = new Batch();
  const lineBatch = new Batch();
  /** Per-body colour ranges (vertex indices), so a theme flip re-tints in place. */
  const slices: Array<{ dust: [number, number]; glow: [number, number]; line: [number, number] }> = [];

  const hueOf = (body: SceneBody): [number, number, number] =>
    body.kind === 'repository'
      ? unit(colors.label)
      : unit(cssColorToRgb(kindColor(body.hueKey, colors.light)));

  // Depth is carried by the body geometry (thinner, softer dust one layer
  // back), not by z: the scene is additive with depth testing off, so z would
  // change nothing.
  const z = 0;
  bodies.forEach((body, position) => {
    const hue = hueOf(body);
    const center: readonly [number, number] = [body.x, body.y];
    const dustStart = dustBatch.bodies.length;
    const glowStart = glowBatch.bodies.length;
    const lineStart = lineBatch.bodies.length;
    if (body.kind === 'repository') {
      // A hub is an identity mark, never a holding: a bright core, a soft
      // halo and a thin ring at a fixed categorical size.
      glowBatch.push(position, center, 0, 0, z, hue, 0.95, body.radius * 1.5);
      glowBatch.push(position, center, 0, 0, z, hue, 0.26, body.radius * 7);
      const ring = 40;
      const ringRadius = body.radius * 2.1;
      for (let step = 0; step < ring; step += 1) {
        const a0 = (step / ring) * Math.PI * 2;
        const a1 = ((step + 1) / ring) * Math.PI * 2;
        lineBatch.push(position, center, Math.cos(a0) * ringRadius, Math.sin(a0) * ringRadius, z, hue, 0.5);
        lineBatch.push(position, center, Math.cos(a1) * ringRadius, Math.sin(a1) * ringRadius, z, hue, 0.5);
      }
    } else {
      const geometry = buildNeuralBody({ id: body.id, radius: body.radius, depth: body.depth });
      for (let particle = 0; particle < geometry.count; particle += 1) {
        dustBatch.push(
          position,
          center,
          geometry.positions[particle * 3]!,
          geometry.positions[particle * 3 + 1]!,
          z,
          hue,
          geometry.alphas[particle]!,
          geometry.sizes[particle]!,
        );
      }
      for (let anchor = 0; anchor < geometry.glowCount; anchor += 1) {
        glowBatch.push(
          position,
          center,
          geometry.glows[anchor * 4]!,
          geometry.glows[anchor * 4 + 1]!,
          z,
          hue,
          geometry.glows[anchor * 4 + 3]!,
          geometry.glows[anchor * 4 + 2]!,
        );
      }
      // Two flare sprites, dark at rest: a tight one over the crown and a
      // wide, faint one that reads in peripheral vision.
      glowBatch.push(position, center, 0, body.radius * 0.18, z, hue, 0.9, body.radius * 1.6, 1);
      glowBatch.push(position, center, 0, body.radius * 0.18, z, hue, 0.35, body.radius * 3.6, 1);
      for (let segment = 0; segment < geometry.filamentSegments; segment += 1) {
        lineBatch.push(position, center, geometry.filaments[segment * 4]!, geometry.filaments[segment * 4 + 1]!, z, hue, 0.2);
        lineBatch.push(position, center, geometry.filaments[segment * 4 + 2]!, geometry.filaments[segment * 4 + 3]!, z, hue, 0.2);
      }
    }
    slices.push({
      dust: [dustStart, dustBatch.bodies.length],
      glow: [glowStart, glowBatch.bodies.length],
      line: [lineStart, lineBatch.bodies.length],
    });
  });

  const dustGeometry = dustBatch.geometry(true);
  const glowGeometry = glowBatch.geometry(true);
  const lineGeometry = lineBatch.geometry(false);
  const dust = new Points(dustGeometry, dustMaterial);
  const glows = new Points(glowGeometry, dustMaterial);
  const filaments = new LineSegments(lineGeometry, lineMaterial);
  for (const object of [dust, glows, filaments]) object.frustumCulled = false;

  // Evidenced paths are sampled in world space and rebuilt when the spread
  // changes (a resize), which is rare and cheap: a few curves of 24 segments.
  const pathGeometry = new BufferGeometry();
  const pathFrom: number[] = [];
  const pathTo: number[] = [];
  for (const path of model.paths) {
    const from = index.get(path.from);
    const to = index.get(path.to);
    if (from === undefined || to === undefined) continue;
    for (let step = 0; step < 24; step += 1) {
      pathFrom.push(from, from);
      pathTo.push(to, to);
    }
  }
  pathGeometry.setAttribute('position', new Float32BufferAttribute(new Float32Array(pathFrom.length * 3), 3));
  pathGeometry.setAttribute('aFrom', new Float32BufferAttribute(pathFrom, 1));
  pathGeometry.setAttribute('aTo', new Float32BufferAttribute(pathTo, 1));
  const paths = new LineSegments(pathGeometry, pathMaterial);
  paths.frustumCulled = false;
  let pathPoints: Array<Array<readonly [number, number]>> = [];

  const layoutPaths = (spread: number): void => {
    pathPoints = model.paths.map((path) => samplePath(model, path, spread, 24));
    const attribute = pathGeometry.getAttribute('position');
    let vertex = 0;
    for (const points of pathPoints) {
      for (let step = 0; step < points.length - 1; step += 1) {
        const a = points[step]!;
        const b = points[step + 1]!;
        attribute.setXYZ(vertex, a[0], a[1], 0);
        attribute.setXYZ(vertex + 1, b[0], b[1], 0);
        vertex += 2;
      }
    }
    attribute.needsUpdate = true;
  };

  // One travelling light per path, parked cold until its relation conducts.
  // Pulses are placed in world space directly (centre 0, spread-independent).
  const pulseCount = Math.max(1, model.paths.length);
  const pulseBatch = new Batch();
  for (const path of model.paths) {
    pulseBatch.push(index.get(path.from) ?? 0, [0, 0], 0, 0, 0, unit(colors.alert), 0, 0.05);
  }
  if (model.paths.length === 0) pulseBatch.push(0, [0, 0], 0, 0, 0, unit(colors.alert), 0, 0.05);
  const pulseGeometry = pulseBatch.geometry(true);
  const pulses = new Points(pulseGeometry, dustMaterial);
  pulses.frustumCulled = false;

  // The focus halo: one restrained ring the hover raises around one body.
  const haloMaterial = new LineBasicMaterial({ transparent: true, opacity: 0, depthTest: false });
  const haloGeometry = new BufferGeometry();
  const haloPoints: number[] = [];
  for (let step = 0; step < 64; step += 1) {
    const angle = (step / 64) * Math.PI * 2;
    haloPoints.push(Math.cos(angle), Math.sin(angle), 0);
  }
  haloGeometry.setAttribute('position', new Float32BufferAttribute(haloPoints, 3));
  const halo = new LineLoop(haloGeometry, haloMaterial);
  halo.visible = false;

  scene.add(paths, glows, dust, filaments, pulses, halo);

  // ---- state --------------------------------------------------------------
  let viewport: Viewport = { width: 1, height: 1 };
  let spread = 1;
  let worldExtent = spreadExtent(model.extent, spread);
  let fit: CameraState = fitBounds(worldExtent, viewport, CAMERA_PAD_PX);
  let cam: CameraState = fit;
  let cameraTarget: CameraState | null = null;
  let atFit = true;
  let emphasis: ReadonlySet<string> | null = null;
  const focus = { t: 0, target: 0, shown: null as string | null };
  let focusNeighborhood: Set<string> | null = null;
  let alive = true;
  let raf = 0;
  let lastFrame = 0;
  /** Under reduced motion heat still has to cool. Nothing travels; the static
   * frame is simply recomposed once a second while anything is warm, the same
   * coarse clock the signal panel uses to keep a printed age true. */
  let coolingTimer: ReturnType<typeof setTimeout> | null = null;

  const neighborhoodOf = (id: string): Set<string> => {
    const set = new Set<string>([id]);
    for (const path of model.pathsByBody.get(id) ?? []) {
      set.add(path.from);
      set.add(path.to);
    }
    return set;
  };

  const applyCamera = (): void => {
    const halfW = (viewport.width * cam.scale) / 2;
    const halfH = (viewport.height * cam.scale) / 2;
    camera3.left = -halfW;
    camera3.right = halfW;
    camera3.top = halfH;
    camera3.bottom = -halfH;
    camera3.position.set(cam.cx, cam.cy, 10);
    camera3.updateProjectionMatrix();
    uniforms.uPixelsPerUnit.value = pixelRatio / cam.scale;
    uniforms.uSpread.value = spread;
  };

  const writeState = (): void => {
    const shown = focus.shown;
    bodies.forEach((body, position) => {
      const channels = bodyState({
        heat: field.heatOf(body.id),
        vitality: body.vitality,
        inFocusNeighborhood: shown === null ? null : focusNeighborhood?.has(body.id) === true,
        focusT: focus.t,
        shown: body.id === shown,
        outsideEmphasis: emphasis === null ? null : !emphasis.has(body.id),
      });
      state.data[position * 4] = channelByte(channels.heat);
      state.data[position * 4 + 1] = channelByte(channels.dim);
      state.data[position * 4 + 2] = channelByte(channels.raise);
      state.data[position * 4 + 3] = channelByte(channels.vitality);
    });
    state.texture.needsUpdate = true;
  };

  const writeHalo = (): void => {
    const shown = focus.shown;
    const body = shown === null ? undefined : model.byId.get(shown);
    if (!body || focus.t <= 0.01) {
      halo.visible = false;
      return;
    }
    halo.visible = true;
    const radius = body.kind === 'repository' ? body.radius * 3 : body.radius * 1.12;
    halo.position.set(body.x * spread, body.y - (body.kind === 'repository' ? 0 : body.radius * 0.05), 0);
    halo.scale.set(radius, radius * 0.92, 1);
    haloMaterial.opacity = 0.85 * focus.t;
    const [hr, hg, hb] = unit(colors.hot);
    // The shader uniforms are raw sRGB; say so here too, or three would treat
    // the ring as linear and output-encode it lighter than the dust it rings.
    haloMaterial.color.setRGB(hr, hg, hb, SRGBColorSpace);
  };

  const writePulses = (now: number): void => {
    const reduced = isReduced();
    const phase = (now % HOP_PULSE_PERIOD_MS) / HOP_PULSE_PERIOD_MS;
    const position = pulseGeometry.getAttribute('position');
    const alpha = pulseGeometry.getAttribute('aAlpha');
    const size = pulseGeometry.getAttribute('aSize');
    model.paths.forEach((path, at) => {
      const heatFrom = field.heatOf(path.from);
      const heatTo = field.heatOf(path.to);
      const travel = Math.min(heatFrom, heatTo);
      const points = pathPoints[at];
      if (reduced || travel <= 0.04 || !points || points.length < 2) {
        alpha.setX(at, 0);
        return;
      }
      const forward = heatFrom >= heatTo;
      const spans = points.length - 1;
      const walked = (forward ? phase : 1 - phase) * spans;
      const span = Math.max(0, Math.min(spans - 1, Math.floor(walked)));
      const local = walked - span;
      const a = points[span]!;
      const b = points[span + 1]!;
      position.setXYZ(at, a[0] + (b[0] - a[0]) * local, a[1] + (b[1] - a[1]) * local, 0);
      alpha.setX(at, 0.95 * Math.min(1, travel * 2));
      size.setX(at, 0.05 + 0.07 * travel);
    });
    position.needsUpdate = true;
    alpha.needsUpdate = true;
    size.needsUpdate = true;
  };

  const compose = (now: number): void => {
    if (!alive) return;
    writeState();
    writeHalo();
    writePulses(now);
    applyCamera();
    renderer.render(scene, camera3);
  };

  const currentView = (): SceneView => ({ camera: cam, viewport, spread, fit });
  const publish = (): void => onView(currentView());

  const step = (now: number): void => {
    raf = 0;
    if (!alive) return;
    const delta = lastFrame === 0 ? 16 : now - lastFrame;
    lastFrame = now;
    const warm = field.tick(now);
    focus.t = approach(focus.t, focus.target, delta, 90);
    const focusSettled = settled(focus.t, focus.target);
    if (focusSettled) {
      focus.t = focus.target;
      if (focus.target === 0) {
        focus.shown = null;
        focusNeighborhood = null;
      }
    }
    let cameraMoving = false;
    if (cameraTarget) {
      const next: CameraState = {
        cx: approach(cam.cx, cameraTarget.cx, delta, 70),
        cy: approach(cam.cy, cameraTarget.cy, delta, 70),
        scale: approach(cam.scale, cameraTarget.scale, delta, 70),
      };
      const tolerance = cameraTarget.scale * 0.5;
      const done =
        settled(next.cx, cameraTarget.cx, tolerance)
        && settled(next.cy, cameraTarget.cy, tolerance)
        && settled(next.scale, cameraTarget.scale, cameraTarget.scale * 0.002);
      cam = done ? cameraTarget : next;
      if (done) cameraTarget = null;
      cameraMoving = !done;
      publish();
    }
    compose(now);
    if (alive && wantsNextFrame({ warm, focusSettled, cameraMoving, reduced: isReduced() })) {
      raf = requestAnimationFrame(step);
    } else {
      lastFrame = 0;
    }
  };

  const settle = (): void => {
    if (raf) {
      cancelAnimationFrame(raf);
      raf = 0;
    }
    if (coolingTimer !== null) {
      clearTimeout(coolingTimer);
      coolingTimer = null;
    }
    lastFrame = 0;
    const warm = field.tick(performance.now());
    focus.t = focus.target;
    if (focus.target === 0) {
      focus.shown = null;
      focusNeighborhood = null;
    }
    if (cameraTarget) {
      cam = cameraTarget;
      cameraTarget = null;
      publish();
    }
    compose(performance.now());
    if (warm && alive && isReduced()) coolingTimer = setTimeout(settle, 1000);
  };

  const wake = (): void => {
    if (!alive) return;
    if (isReduced()) {
      settle();
      return;
    }
    if (!raf) {
      lastFrame = 0;
      raf = requestAnimationFrame(step);
    }
  };

  const moveCamera = (next: CameraState, animate: boolean): void => {
    const bounded = clampZoom(next, fit);
    if (cameraEquals(bounded, cam) && cameraTarget === null) return;
    if (animate && !isReduced()) {
      cameraTarget = bounded;
      wake();
      return;
    }
    cameraTarget = null;
    cam = bounded;
    publish();
    wake();
  };

  const emphasisCamera = (ids: ReadonlySet<string>): CameraState | null => {
    const bounds = bodiesBounds(model, ids, spread);
    if (!bounds) return null;
    return clampZoom(fitBounds(bounds, viewport, CAMERA_PAD_PX * 2), fit);
  };

  const unsubscribe = field.subscribe(wake);

  const retint = (): void => {
    const dustColor = dustGeometry.getAttribute('aColor');
    const glowColor = glowGeometry.getAttribute('aColor');
    const lineColor = lineGeometry.getAttribute('aColor');
    slices.forEach((slice, position) => {
      const [cr, cg, cb] = hueOf(bodies[position]!);
      for (let at = slice.dust[0]; at < slice.dust[1]; at += 1) dustColor.setXYZ(at, cr, cg, cb);
      for (let at = slice.glow[0]; at < slice.glow[1]; at += 1) glowColor.setXYZ(at, cr, cg, cb);
      for (let at = slice.line[0]; at < slice.line[1]; at += 1) lineColor.setXYZ(at, cr, cg, cb);
    });
    dustColor.needsUpdate = true;
    glowColor.needsUpdate = true;
    lineColor.needsUpdate = true;
    const [ar, ag, ab] = unit(colors.alert);
    const pulseColor = pulseGeometry.getAttribute('aColor');
    for (let at = 0; at < pulseCount; at += 1) pulseColor.setXYZ(at, ar, ag, ab);
    pulseColor.needsUpdate = true;
    uniforms.uHot.value = unit(colors.hot);
    uniforms.uAlert.value = unit(colors.alert);
    uniforms.uDim.value = unit(colors.dim);
    uniforms.uEdge.value = unit(colors.edge);
  };

  layoutPaths(spread);

  return {
    canvas,
    resize: (next) => {
      if (!alive) return;
      const width = Math.max(1, Math.floor(next.width));
      const height = Math.max(1, Math.floor(next.height));
      viewport = { width, height };
      renderer.setSize(width, height, false);
      const nextSpread = columnSpread(model.extent, viewport);
      if (nextSpread !== spread) {
        spread = nextSpread;
        worldExtent = spreadExtent(model.extent, spread);
        layoutPaths(spread);
      }
      const nextFit = fitBounds(worldExtent, viewport, CAMERA_PAD_PX);
      // A resize while the reader was looking at a fitted view (the whole
      // field, or a focused repository) keeps that view fitted; a camera the
      // reader zoomed or panned holds its world centre.
      fit = nextFit;
      if (atFit) {
        cam = emphasis === null ? nextFit : emphasisCamera(emphasis) ?? nextFit;
        cameraTarget = null;
      } else {
        cam = clampZoom(cam, fit);
      }
      publish();
      compose(performance.now());
    },
    focus: (id) => {
      if (!alive) return;
      const body = id === null ? null : model.byId.get(id) ?? null;
      if (body === null) {
        focus.target = 0;
      } else {
        focus.shown = body.id;
        focusNeighborhood = neighborhoodOf(body.id);
        focus.target = 1;
      }
      wake();
    },
    emphasize: (ids) => {
      if (!alive) return;
      emphasis = ids;
      // Either view starts fitted: the whole field, or the focused members.
      atFit = true;
      if (ids === null) {
        moveCamera(fit, true);
        return;
      }
      const target = emphasisCamera(ids);
      if (target) moveCamera(target, true);
      else wake();
    },
    fit: () => {
      atFit = true;
      moveCamera(emphasis === null ? fit : emphasisCamera(emphasis) ?? fit, true);
    },
    zoomIn: () => {
      atFit = false;
      moveCamera({ ...(cameraTarget ?? cam), scale: (cameraTarget ?? cam).scale / 1.5 }, true);
    },
    zoomOut: () => {
      atFit = false;
      moveCamera({ ...(cameraTarget ?? cam), scale: (cameraTarget ?? cam).scale * 1.5 }, true);
    },
    zoomAt: (factor, anchor) => {
      atFit = false;
      moveCamera(zoomAbout(cameraTarget ?? cam, viewport, factor, anchor), false);
    },
    panBy: (dx, dy) => {
      atFit = false;
      moveCamera(panBy(cameraTarget ?? cam, dx, dy), false);
    },
    view: currentView,
    pick: (px, py) => {
      const { x, y } = unproject(cam, viewport, px, py);
      return pickBody(model, x, y, spread);
    },
    wake,
    settle,
    retheme: () => {
      if (!alive) return;
      colors = palette(container);
      retint();
      compose(performance.now());
    },
    dispose: () => {
      if (!alive) return;
      alive = false;
      unsubscribe();
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
      if (coolingTimer !== null) clearTimeout(coolingTimer);
      coolingTimer = null;
      for (const geometry of [dustGeometry, glowGeometry, lineGeometry, pathGeometry, pulseGeometry, haloGeometry]) {
        geometry.dispose();
      }
      for (const material of [dustMaterial, lineMaterial, pathMaterial, haloMaterial]) material.dispose();
      state.texture.dispose();
      renderer.dispose();
      // `dispose` frees GPU objects but keeps the context alive on a canvas
      // nobody will draw to again; browsers evict the oldest live contexts by
      // firing `webglcontextlost` on them, which would read as a real loss on
      // whichever field is current. Release it deliberately instead.
      renderer.forceContextLoss();
      canvas.remove();
    },
  };
}
