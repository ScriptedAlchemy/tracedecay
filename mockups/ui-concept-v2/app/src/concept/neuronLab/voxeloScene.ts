import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";
import { EffectComposer } from "three/addons/postprocessing/EffectComposer.js";
import { OutputPass } from "three/addons/postprocessing/OutputPass.js";
import { RenderPass } from "three/addons/postprocessing/RenderPass.js";
import { UnrealBloomPass } from "three/addons/postprocessing/UnrealBloomPass.js";
import { mergeGeometries } from "three/addons/utils/BufferGeometryUtils.js";
import { PROJECTS, SYNAPSE_EVENT } from "../../data/fixtures";
import { CHECKOUT_COLORS } from "../../brain/repoLayout";
import { buildScopedGraph } from "../../brain/scopedGraph";
import { interiorIdentities, projectById, sessionTufts, type SessionTuft } from "./interior";
import {
  axonPts,
  disposeObject,
  growDendrites,
  hash32,
  HUB_ID,
  layoutAroundHub,
  layoutLabBodies,
  mulberry32,
  attributionPipes,
  projectIdentities,
  type BrainState,
  type LabBody,
  type LabHooks,
  type LabIdentity,
  type TubeRec,
} from "./labUtil";

const BG = 0x05070e;

/** Idle pale ivory/slate on the mesh; HDR prism packet is added on the same surface. */
const TUBE_VERT = /* glsl */ `
varying vec2 vUv;
varying vec3 vPos;
varying vec3 vWorldN;
varying vec3 vViewN;
varying vec3 vViewPos;
void main() {
  vUv = uv;
  vPos = position;
  vWorldN = normalize((modelMatrix * vec4(normal, 0.0)).xyz);
  vViewN = normalize(normalMatrix * normal);
  vec4 mv = modelViewMatrix * vec4(position, 1.0);
  vViewPos = mv.xyz;
  gl_Position = projectionMatrix * mv;
}
`;

const TUBE_FRAG = /* glsl */ `
varying vec2 vUv;
varying vec3 vPos;
varying vec3 vWorldN;
varying vec3 vViewN;
varying vec3 vViewPos;
uniform float uHead;
uniform float uAmp;
uniform vec3 uSoma;
uniform vec3 uIvory;
uniform float uAlongHue;
uniform vec3 uTint;
uniform float uUseTint;
vec3 prism(float t) {
  float x = fract(t);
  vec3 mag = vec3(1.70, 0.10, 1.22);
  vec3 org = vec3(1.55, 0.58, 0.08);
  vec3 yel = vec3(1.38, 1.58, 0.14);
  vec3 grn = vec3(0.20, 1.52, 0.36);
  vec3 cyn = vec3(0.08, 1.28, 1.70);
  if (x < 0.2) return mix(mag, org, x / 0.2);
  if (x < 0.4) return mix(org, yel, (x - 0.2) / 0.2);
  if (x < 0.6) return mix(yel, grn, (x - 0.4) / 0.2);
  if (x < 0.8) return mix(grn, cyn, (x - 0.6) / 0.2);
  return mix(cyn, mag, (x - 0.8) / 0.2);
}
void main() {
  vec3 N = normalize(vWorldN);
  vec3 Nv = normalize(vViewN);
  vec3 V = normalize(-vViewPos);
  float key = max(dot(N, normalize(vec3(-0.45, 0.78, 0.48))), 0.0);
  float fill = max(dot(N, normalize(vec3(0.55, -0.22, -0.38))), 0.0);
  float hemi = N.y * 0.5 + 0.5;
  float wrap = max(dot(N, normalize(vec3(-0.2, 0.4, 0.35))) * 0.55 + 0.45, 0.0);
  float lit = 0.14 + key * 0.30 + fill * 0.09 + hemi * 0.06 + wrap * 0.09;
  float rim = pow(1.0 - clamp(dot(Nv, V), 0.0, 1.0), 2.6) * 0.06;
  vec3 ivory = uIvory * lit + vec3(0.42, 0.50, 0.56) * rim;

  float along = vUv.x;
  float d = fract(along - uHead + 1.0);
  float head = exp(-d * d * 78.0);
  float trail = exp(-d * d * 12.5) * 0.42;
  float packet = (head + trail) * uAmp;
  packet *= 1.0 - smoothstep(0.74, 0.98, along);

  float hue;
  if (uAlongHue > 0.5) {
    hue = along * 0.92 + uHead * 0.38;
  } else {
    float ang = atan(vPos.z - uSoma.z, vPos.x - uSoma.x);
    float dist = length(vPos - uSoma);
    hue = ang / 6.2831853 + dist * 0.045 + uHead * 0.18;
  }
  // identity hue: the organism keeps its registry color; prism only as fallback
  vec3 rain = uUseTint > 0.5 ? uTint * (1.15 + 0.45 * sin(hue * 6.2831853)) : prism(hue);
  vec3 tint = mix(ivory, ivory * 0.22 + rain * 0.48, clamp(packet * 1.15, 0.0, 1.0));
  vec3 emit = rain * packet * 2.15;
  gl_FragColor = vec4(tint + emit, 1.0);
}
`;

function tubeMat(
  amp: number,
  soma: THREE.Vector3,
  ivoryHex: number,
  alongHue: boolean,
  tintHex: number | null = null,
) {
  const c = new THREE.Color(ivoryHex);
  const tc = new THREE.Color(tintHex ?? 0xffffff);
  return new THREE.ShaderMaterial({
    uniforms: {
      uHead: { value: 0 },
      uAmp: { value: amp },
      uSoma: { value: soma.clone() },
      uIvory: { value: new THREE.Vector3(c.r, c.g, c.b) },
      uAlongHue: { value: alongHue ? 1 : 0 },
      uTint: { value: new THREE.Vector3(tc.r, tc.g, tc.b) },
      uUseTint: { value: tintHex != null ? 1 : 0 },
    },
    vertexShader: TUBE_VERT,
    fragmentShader: TUBE_FRAG,
    transparent: false,
    depthWrite: true,
    depthTest: true,
    toneMapped: false,
    fog: false,
    dithering: true,
  });
}

function tubeGeo(pts: THREE.Vector3[], radius: number, radial = 10) {
  if (pts.length < 2) return null;
  const curve = new THREE.CatmullRomCurve3(pts, false, "catmullrom", 0.32);
  const len = curve.getLength();
  if (len < 0.035) return null;
  const segs = Math.max(12, Math.min(88, Math.round(len * 11)));
  return new THREE.TubeGeometry(curve, segs, radius, radial, false);
}

function mergeTubes(recs: TubeRec[], radiusScale: number, radial: number) {
  const geos: THREE.BufferGeometry[] = [];
  for (const r of recs) {
    const g = tubeGeo(r.pts, r.radius * radiusScale, radial);
    if (g) geos.push(g);
  }
  if (!geos.length) return null;
  const merged = mergeGeometries(geos, false);
  for (const g of geos) g.dispose();
  return merged;
}

/** Recency / mass / row fan, then stretched so knots read as separate neurons. */
function layoutVoxeloField(list?: readonly LabIdentity[]): LabBody[] {
  const bodies = layoutLabBodies(list);
  const spread = list && list.length <= 2 ? 1.55 : 3.15;
  const ySpread = list && list.length <= 2 ? 1.4 : 2.85;
  for (const b of bodies) {
    b.pos.x *= spread;
    b.pos.y *= ySpread;
    b.pos.z *= spread;
  }
  return bodies;
}

function ivoryMat() {
  return new THREE.MeshPhysicalMaterial({
    color: 0xb7c6d0,
    roughness: 0.52,
    metalness: 0.04,
    clearcoat: 0.22,
    clearcoatRoughness: 0.45,
    emissive: 0x0c1c28,
    emissiveIntensity: 0.12,
    envMapIntensity: 0.22,
  });
}

function somaKnot(
  group: THREE.Group,
  origin: THREE.Vector3,
  radius: number,
  rng: () => number,
  projectId: string,
  shell: THREE.Material,
) {
  const knot = new THREE.Group();
  knot.position.copy(origin);
  const n = 5 + Math.floor(rng() * 2);
  for (let i = 0; i < n; i++) {
    const y = rng() * 2 - 1;
    const phi = rng() * Math.PI * 2;
    const rr = Math.sqrt(Math.max(0, 1 - y * y));
    const off = new THREE.Vector3(rr * Math.cos(phi), y, rr * Math.sin(phi)).multiplyScalar(radius * (0.12 + rng() * 0.2));
    const s = radius * (0.36 + rng() * 0.32);
    const mesh = new THREE.Mesh(new THREE.SphereGeometry(s, 22, 18), shell);
    mesh.position.copy(off);
    mesh.userData.projectId = projectId;
    knot.add(mesh);
  }
  const core = new THREE.Mesh(
    new THREE.SphereGeometry(radius * 0.58, 28, 22),
    new THREE.MeshPhysicalMaterial({
      color: 0xd2e4ee,
      roughness: 0.28,
      metalness: 0.08,
      emissive: 0x2a6a88,
      emissiveIntensity: 0.28,
      clearcoat: 0.45,
      clearcoatRoughness: 0.22,
      envMapIntensity: 0.28,
    }),
  );
  core.userData.projectId = projectId;
  knot.add(core);
  const ring = new THREE.Mesh(
    new THREE.TorusGeometry(radius * 0.3, radius * 0.028, 8, 24),
    new THREE.MeshBasicMaterial({
      color: 0x3a7a92,
      transparent: true,
      opacity: 0.7,
      depthWrite: false,
    }),
  );
  ring.rotation.x = Math.PI * 0.5;
  ring.userData.projectId = projectId;
  knot.add(ring);
  group.add(knot);
  return { knot, core };
}

const TRACEDECAY_ID = SYNAPSE_EVENT.projectId;

export type VoxeloHandle = {
  dispose: () => void;
  setFocus: (id: string | null) => void;
  setBrainState: (state: BrainState) => void;
  zoom: (delta: number | "fit") => void;
};

export function mountVoxelo(host: HTMLElement, hooks: LabHooks = {}): VoxeloHandle {
  const driven = Boolean(hooks.brainState);
  let brainState: BrainState =
    hooks.brainState ?? (hooks.scope ? (hooks.grain === "worktree" ? "repo-zoom" : "scoped") : "overview");
  let scope = hooks.scope ?? null;
  const grain = hooks.grain ?? "session";
  let focusId: string | null =
    hooks.focusId ?? (brainState === "hover" || brainState === "synapse" ? TRACEDECAY_ID : null);
  let zoomMul = 1;

  const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false });
  renderer.setClearColor(BG, 1);
  renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 0.82;
  host.appendChild(renderer.domElement);

  const labelLayer = document.createElement("div");
  labelLayer.className = "nl-labels";
  host.appendChild(labelLayer);

  const inspect = document.createElement("div");
  inspect.className = "nl-inspect";
  host.appendChild(inspect);

  const scene = new THREE.Scene();
  scene.background = new THREE.Color(BG);
  scene.fog = new THREE.FogExp2(BG, 0.0074);

  const pmrem = new THREE.PMREMGenerator(renderer);
  const envTex = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environment = envTex;
  scene.environmentIntensity = 0.2;
  pmrem.dispose();

  const camera = new THREE.PerspectiveCamera(32, 1, 0.2, 140);
  scene.add(new THREE.AmbientLight(0x243040, 0.42));
  scene.add(new THREE.HemisphereLight(0x7a9bb0, 0x05070e, 0.32));
  const key = new THREE.DirectionalLight(0xc5d8e6, 0.48);
  key.position.set(-5.2, 7.2, 6.4);
  scene.add(key);
  const fill = new THREE.DirectionalLight(0x1a3040, 0.22);
  fill.position.set(4.5, -2.2, -3.5);
  scene.add(fill);

  const world = new THREE.Group();
  scene.add(world);
  const dustGroup = new THREE.Group();
  scene.add(dustGroup);

  const ivory = ivoryMat();

  let bodies: LabBody[] = [];
  let hubPos: THREE.Vector3 | null = null;
  let somaHits: THREE.Object3D[] = [];
  let labels: { el: HTMLDivElement; pos: THREE.Vector3; radius: number; id: string; place: "right" | "under" }[] = [];
  let neuronCharge: THREE.ShaderMaterial[] = [];
  let neuronAmp: number[] = [];
  let neuronIds: string[] = [];
  let axonCharge: { mat: THREE.ShaderMaterial; speed: number; phase: number }[] = [];
  let cores: { mesh: THREE.Mesh; rest: number; id: string }[] = [];
  const nameOf = new Map<string, string>();
  let worldSize = new THREE.Vector3(1, 1, 1);
  let worldCenter = new THREE.Vector3();
  let fitSphere = new THREE.Sphere(new THREE.Vector3(), 1);
  let hoverId: string | null = null;

  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches ?? false;

  function isNeighborhood() {
    if (driven) return brainState === "repo-zoom";
    return Boolean(scope);
  }

  function isScopedField() {
    return driven && brainState === "scoped";
  }

  function showTufts() {
    if (driven) return false;
    if (scope) return grain === "session";
    return false;
  }

  function enterOnClick() {
    if (driven) return brainState === "overview";
    return !scope;
  }

  function leaveOnEmpty() {
    if (driven) return false;
    return Boolean(scope);
  }

  function idleInspect() {
    if (isScopedField()) return "hover a node · inspect only";
    if (isNeighborhood()) return "hover an orb · inspect only";
    return enterOnClick() ? "hover a soma · click to enter" : "hover a soma · inspect only · esc / empty air returns";
  }

  function topologyKey() {
    if (isScopedField()) return `sc:${scope ?? ""}`;
    if (!isNeighborhood()) return "registry";
    return `n:${scope ?? ""}:${showTufts() ? "t" : "w"}`;
  }

  function addLabel(
    text: string,
    pos: THREE.Vector3,
    radius: number,
    id: string,
    place: "right" | "under" = "right",
    small = false,
  ) {
    const el = document.createElement("div");
    el.className = "nl-label";
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = text;
    name.style.color = small ? "#a9bcc9" : "#eaf4fb";
    name.style.background = small ? "rgba(6,10,18,0.55)" : "rgba(6,10,18,0.72)";
    name.style.padding = small ? "1px 5px" : "2px 8px";
    name.style.border = small ? "1px solid rgba(150,190,210,0.16)" : "1px solid rgba(150,190,210,0.32)";
    name.style.borderRadius = "2px";
    name.style.fontSize = small ? "10px" : "12px";
    name.style.letterSpacing = "0.02em";
    name.style.textShadow = "0 1px 2px #05070e, 0 0 10px #05070e";
    el.appendChild(name);
    labelLayer.appendChild(el);
    labels.push({ el, pos: pos.clone(), radius, id, place });
  }

  function dendriteScale(b: LabBody) {
    return 0.78 + b.somaR * 1.05;
  }

  function orbTint(warm: number) {
    return new THREE.Color().lerpColors(new THREE.Color(0x2f7a96), new THREE.Color(0xc49a3c), warm);
  }

  /** Registry project color / real checkout color — 2D and 3D stay one organism. */
  function identityColor(id: string): THREE.Color | null {
    const co = CHECKOUT_COLORS[id];
    if (co) return new THREE.Color(co);
    const p = PROJECTS.find((pp) => pp.id === id);
    return p ? new THREE.Color(p.color) : null;
  }

  function addPinprickCanopy(origin: THREE.Vector3, radius: number, rng: () => number, color: THREE.Color, projectId: string) {
    const n = 108;
    const pos = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) {
      const y = rng() * 2 - 1;
      const phi = rng() * Math.PI * 2;
      const rr = Math.sqrt(Math.max(0, 1 - y * y));
      const r = radius * (0.38 + rng() * 0.6);
      pos[i * 3] = origin.x + rr * Math.cos(phi) * r;
      pos[i * 3 + 1] = origin.y + y * r;
      pos[i * 3 + 2] = origin.z + rr * Math.sin(phi) * r;
    }
    const geo = new THREE.BufferGeometry();
    geo.setAttribute("position", new THREE.BufferAttribute(pos, 3));
    const pts = new THREE.Points(
      geo,
      new THREE.PointsMaterial({
        color,
        size: 0.05,
        transparent: true,
        opacity: 0.76,
        depthWrite: false,
        sizeAttenuation: true,
      }),
    );
    pts.userData.projectId = projectId;
    pts.renderOrder = 2;
    world.add(pts);
  }

  /** Spherical dendritic nebula — plate 03 geodesic orb, not a tuft at a pipe end. */
  function addCheckoutOrb(b: LabBody) {
    const rng = mulberry32(hash32(b.project.id) ^ 0xa11ce);
    const R = b.somaR;
    const tint = identityColor(b.project.id) ?? orbTint(b.warm);
    const g = new THREE.Group();
    g.position.copy(b.pos);
    g.userData.projectId = b.project.id;

    const glass = new THREE.Mesh(
      new THREE.SphereGeometry(R, 36, 28),
      new THREE.MeshPhysicalMaterial({
        color: 0x0c1824,
        roughness: 0.5,
        metalness: 0.06,
        transparent: true,
        opacity: 0.15,
        emissive: tint,
        emissiveIntensity: 0.16,
        depthWrite: false,
        side: THREE.DoubleSide,
        envMapIntensity: 0.16,
      }),
    );
    glass.userData.projectId = b.project.id;
    g.add(glass);

    const wire = new THREE.LineSegments(
      new THREE.WireframeGeometry(new THREE.IcosahedronGeometry(R * 0.985, 1)),
      new THREE.LineBasicMaterial({
        color: tint,
        transparent: true,
        opacity: 0.24,
        depthWrite: false,
      }),
    );
    wire.userData.projectId = b.project.id;
    g.add(wire);
    world.add(g);
    somaHits.push(g);

    const recs = growDendrites(b.pos, R, rng, 1, true);
    const shellGeo = mergeTubes(recs, 1.0, 6);
    const amp = 0.52 + b.warm * 0.28;
    const dmat = tubeMat(amp, b.pos, 0xa8bac6, false, tint.getHex());
    neuronCharge.push(dmat);
    neuronAmp.push(amp);
    neuronIds.push(b.project.id);
    if (shellGeo) {
      const mesh = new THREE.Mesh(shellGeo, dmat);
      mesh.renderOrder = 1;
      mesh.userData.projectId = b.project.id;
      world.add(mesh);
    }

    const { knot, core } = somaKnot(world, b.pos, 0.26, rng, b.project.id, ivory);
    somaHits.push(knot);
    cores.push({
      mesh: core,
      rest: (core.material as THREE.MeshPhysicalMaterial).emissiveIntensity,
      id: b.project.id,
    });
    addPinprickCanopy(b.pos, R, rng, tint, b.project.id);
    nameOf.set(b.project.id, b.project.name);
    addLabel(b.project.name, b.pos, R, b.project.id);
  }

  function addNeurons(list: LabBody[]) {
    for (const b of list) {
      const rng = mulberry32(hash32(b.project.id) ^ 0xa11ce);
      const scale = dendriteScale(b);
      const recs = growDendrites(b.pos, b.somaR, rng, scale);
      const shellGeo = mergeTubes(recs, 2.05, 8);
      const amp = 0.72 + b.warm * 0.32;
      const tint = identityColor(b.project.id);
      const dmat = tubeMat(amp, b.pos, 0xa8bac6, false, tint ? tint.getHex() : null);
      neuronCharge.push(dmat);
      neuronAmp.push(amp);
      neuronIds.push(b.project.id);
      if (shellGeo) {
        const mesh = new THREE.Mesh(shellGeo, dmat);
        mesh.renderOrder = 1;
        world.add(mesh);
      }
      const { knot, core } = somaKnot(world, b.pos, b.somaR, rng, b.project.id, ivory);
      somaHits.push(knot);
      cores.push({
        mesh: core,
        rest: (core.material as THREE.MeshPhysicalMaterial).emissiveIntensity,
        id: b.project.id,
      });
      nameOf.set(b.project.id, b.project.name);
      addLabel(b.project.name, b.pos, b.somaR, b.project.id);
    }
  }

  function addPipes(pipes: ReturnType<typeof attributionPipes>) {
    for (const pipe of pipes) {
      const rng = mulberry32(hash32(pipe.a.project.id + pipe.b.project.id) ^ 0x90b);
      const a = pipe.a.pos.clone();
      const b = pipe.b.pos.clone();
      const dir = b.clone().sub(a).normalize();
      a.addScaledVector(dir, pipe.a.somaR * 0.85);
      b.addScaledVector(dir, -pipe.b.somaR * 0.85);
      const pts = axonPts(a, b, rng);
      const recs: TubeRec[] = [{ pts, radius: 0.255 + pipe.warm * 0.08 }];
      const shellGeo = mergeTubes(recs, 1.0, 12);
      const amp = (0.78 + pipe.warm * 0.28) * pipe.energy;
      const src = a.clone();
      const amat = tubeMat(amp, src, 0xb8c6d0, true, 0x5ee7ff);
      axonCharge.push({
        mat: amat,
        speed: 0.042 + pipe.warm * 0.05,
        phase: rng(),
      });
      if (shellGeo) {
        const mesh = new THREE.Mesh(shellGeo, amat);
        mesh.renderOrder = 1;
        world.add(mesh);
      }
    }
  }

  const HUB_R = 0.34;
  const HUB_HALO = 0.5;

  function addHubGlyph(origin: THREE.Vector3) {
    const g = new THREE.Group();
    g.position.copy(origin);
    const core = new THREE.Mesh(
      new THREE.SphereGeometry(HUB_R, 32, 24),
      new THREE.MeshPhysicalMaterial({
        color: 0x8e99a6,
        roughness: 0.44,
        metalness: 0.16,
        emissive: 0x1a242c,
        emissiveIntensity: 0.18,
      }),
    );
    core.userData.projectId = HUB_ID;
    core.renderOrder = 4;
    g.add(core);
    const halo = new THREE.Mesh(
      new THREE.SphereGeometry(HUB_HALO, 20, 16),
      new THREE.MeshBasicMaterial({ color: 0x6a7884, transparent: true, opacity: 0.14, depthWrite: false }),
    );
    halo.userData.projectId = HUB_ID;
    halo.renderOrder = 3;
    g.add(halo);
    const ring = new THREE.Mesh(
      new THREE.TorusGeometry(0.42, 0.016, 8, 32),
      new THREE.MeshBasicMaterial({ color: 0x4a5a66, transparent: true, opacity: 0.55, depthWrite: false }),
    );
    ring.rotation.x = Math.PI * 0.5;
    ring.userData.projectId = HUB_ID;
    ring.renderOrder = 4;
    g.add(ring);
    g.renderOrder = 4;
    world.add(g);
    somaHits.push(g);
    nameOf.set(HUB_ID, "repo:git_common_dir · hub — massless");
    addLabel("repo:git_common_dir", origin, HUB_HALO, HUB_ID, "under");
  }

  /** Hub → orb hairline only. Thin 1/3-energy; never a checkout–checkout triangle. */
  function addHairline(hub: THREE.Vector3, body: LabBody) {
    const rng = mulberry32(hash32(`${HUB_ID}|${body.project.id}`) ^ 0x5b0ce);
    const a = hub.clone();
    const b = body.pos.clone();
    const dir = b.clone().sub(a);
    const len = dir.length() || 1;
    dir.multiplyScalar(1 / len);
    a.addScaledVector(dir, HUB_HALO);
    b.addScaledVector(dir, -body.somaR);
    const mid = a.clone().lerp(b, 0.5);
    const n = new THREE.Vector3().crossVectors(dir, new THREE.Vector3(0, 1, 0));
    if (n.lengthSq() < 1e-8) n.set(1, 0, 0);
    n.normalize();
    mid.addScaledVector(n, (rng() - 0.5) * len * 0.028);
    const recs: TubeRec[] = [{ pts: [a, mid, b], radius: 0.028 }];
    const shellGeo = mergeTubes(recs, 1.0, 5);
    const energy = 1 / 3;
    const amp = (0.62 + body.warm * 0.2) * energy;
    const amat = tubeMat(amp, a.clone(), 0x8aa0ae, true, 0x5ee7ff);
    axonCharge.push({
      mat: amat,
      speed: 0.038 + body.warm * 0.04,
      phase: rng(),
    });
    if (shellGeo) {
      const mesh = new THREE.Mesh(shellGeo, amat);
      mesh.renderOrder = 1;
      world.add(mesh);
    }
  }

  let synapseTubeMat: THREE.ShaderMaterial | null = null;

  /**
   * Registry hub glyph below the field with idle hairlines (the overview never
   * fires). tracedecay's hairline is the admitted conduction tube: energy
   * travels along it only in the synapse state — never a soma tint.
   */
  function addRegistryHub() {
    if (!bodies.length) return;
    let minY = Infinity;
    for (const b of bodies) minY = Math.min(minY, b.pos.y - b.somaR);
    const hub = new THREE.Vector3(0.6, minY - 3.2, 0.4);
    hubPos = hub;
    addHubGlyph(hub);
    for (const b of bodies) {
      const conduction = b.project.id === TRACEDECAY_ID;
      const rng = mulberry32(hash32(`${HUB_ID}|${b.project.id}`) ^ 0x5b0ce);
      const a = hub.clone();
      const e = b.pos.clone();
      const dir = e.clone().sub(a);
      const len = dir.length() || 1;
      dir.multiplyScalar(1 / len);
      a.addScaledVector(dir, HUB_HALO);
      e.addScaledVector(dir, -b.somaR * 0.9);
      const mid = a.clone().lerp(e, 0.5);
      const n = new THREE.Vector3().crossVectors(dir, new THREE.Vector3(0, 1, 0));
      if (n.lengthSq() < 1e-8) n.set(1, 0, 0);
      n.normalize();
      mid.addScaledVector(n, (rng() - 0.5) * len * 0.05);
      const recs: TubeRec[] = [{ pts: [e, mid, a], radius: conduction ? 0.05 : 0.02 }];
      const shellGeo = mergeTubes(recs, 1.0, conduction ? 8 : 5);
      const amat = tubeMat(0, e.clone(), conduction ? 0x9fb6c2 : 0x74858f, true, 0x5ee7ff);
      if (conduction) {
        synapseTubeMat = amat;
        axonCharge.push({ mat: amat, speed: 0.075, phase: rng() });
      }
      if (shellGeo) {
        const mesh = new THREE.Mesh(shellGeo, amat);
        mesh.renderOrder = 1;
        world.add(mesh);
      }
    }
  }

  // scoped-field registries for hover isolation (graph-local, never projects)
  let scopedNodeObjs = new Map<string, { obj: THREE.Mesh; base: number }[]>();
  let scopedEdgeObjs: { line: THREE.Line; a: string; b: string; base: number }[] = [];
  let scopedAdj = new Map<string, Set<string>>();

  /** Scoped 3D: the same honest constellation as 2D, spread as a field. */
  function addScopedField(project: NonNullable<ReturnType<typeof projectById>>) {
    const graph = buildScopedGraph(project);
    const centers = new Map<string, THREE.Vector3>();
    graph.clusters.forEach((c, i) => {
      const rng = mulberry32(hash32(c) ^ 0x5c0);
      const th = (i / Math.max(1, graph.clusters.length)) * Math.PI * 2 + 0.4;
      const R = graph.clusters.length > 1 ? 8.6 : 0;
      centers.set(
        c,
        new THREE.Vector3(
          Math.cos(th) * R * (0.95 + rng() * 0.35) * 1.25,
          (rng() - 0.5) * 4.6,
          Math.sin(th) * R * (0.6 + rng() * 0.3),
        ),
      );
    });
    const posOf = new Map<string, THREE.Vector3>();
    for (const c of graph.clusters) {
      const members = graph.nodes.filter((n) => n.cluster === c);
      const sats = members.filter((n) => !n.isHub);
      const center = centers.get(c)!;
      for (const n of members) {
        const p = center.clone();
        if (!n.isHub) {
          const rng = mulberry32(hash32(n.id));
          const si = sats.indexOf(n);
          const th = (si / Math.max(1, sats.length)) * Math.PI * 2 + rng() * 0.6;
          const phi = (rng() - 0.5) * 1.5;
          const rad = 1.9 + rng() * 1.6 + Math.min(1.7, sats.length * 0.11);
          p.add(
            new THREE.Vector3(
              Math.cos(th) * Math.cos(phi) * rad,
              Math.sin(phi) * rad * 0.75,
              Math.sin(th) * Math.cos(phi) * rad,
            ),
          );
        }
        posOf.set(n.id, p);
        const color = new THREE.Color(n.color);
        const r = n.isHub ? 0.3 + Math.min(0.35, n.degree * 0.035) : 0.11;
        const mat = n.dim
          ? new THREE.MeshBasicMaterial({
              color: 0x64748b,
              wireframe: true,
              transparent: true,
              opacity: 1,
            })
          : new THREE.MeshBasicMaterial({
              color: color.clone().lerp(new THREE.Color(1, 1, 1), n.isHub ? 0.35 : 0.15),
              transparent: true,
              opacity: 1,
            });
        const mesh = new THREE.Mesh(new THREE.SphereGeometry(r, 18, 14), mat);
        mesh.position.copy(p);
        mesh.userData.projectId = n.id;
        world.add(mesh);
        somaHits.push(mesh);
        const objs: { obj: THREE.Mesh; base: number }[] = [{ obj: mesh, base: 1 }];
        nameOf.set(n.id, n.label);
        addLabel(n.label, p, r, n.id, "right", !n.isHub && !n.dim);
        if (n.isHub && !n.dim) {
          const halo = new THREE.Mesh(
            new THREE.SphereGeometry(r * 2.1, 16, 12),
            new THREE.MeshBasicMaterial({ color, transparent: true, opacity: 0.16, depthWrite: false }),
          );
          halo.position.copy(p);
          halo.userData.projectId = n.id;
          world.add(halo);
          objs.push({ obj: halo, base: 0.16 });
        }
        scopedNodeObjs.set(n.id, objs);
      }
    }
    const nodeOf = new Map(graph.nodes.map((n) => [n.id, n]));
    const link = (a: string, b: string) => {
      if (!scopedAdj.has(a)) scopedAdj.set(a, new Set());
      scopedAdj.get(a)!.add(b);
    };
    for (const e of graph.edges) {
      const a = posOf.get(e.a);
      const b = posOf.get(e.b);
      const na = nodeOf.get(e.a);
      if (!a || !b || !na) continue;
      link(e.a, e.b);
      link(e.b, e.a);
      const base = na.dim ? 0.22 : 0.3;
      const geo = new THREE.BufferGeometry().setFromPoints([a, b]);
      const line = new THREE.Line(
        geo,
        new THREE.LineBasicMaterial({
          color: new THREE.Color(na.dim ? "#64748b" : na.color),
          transparent: true,
          opacity: base,
          depthWrite: false,
        }),
      );
      world.add(line);
      scopedEdgeObjs.push({ line, a: e.a, b: e.b, base });
    }
  }

  function addLabeledTuft(t: SessionTuft, host: LabBody) {
    const rng = mulberry32(hash32(t.sessionId) ^ 0x51e55);
    const dir = hubPos
      ? host.pos.clone().sub(hubPos)
      : new THREE.Vector3(rng() - 0.5, 0.35 + rng() * 0.5, rng() - 0.5);
    dir.y += 0.22;
    dir.normalize();
    const start = host.pos.clone().addScaledVector(dir, host.somaR * 0.78);
    const end = host.pos.clone().addScaledVector(dir, host.somaR * 1.07);
    const recs: TubeRec[] = [{ pts: axonPts(start, end, rng), radius: 0.032 }];
    const geo = mergeTubes(recs, 1, 8);
    const amp = 0.55;
    const mat = tubeMat(amp, host.pos, 0xb8c6d0, true, 0x9aecff);
    neuronCharge.push(mat);
    neuronAmp.push(amp);
    neuronIds.push(t.sessionId);
    if (geo) {
      const mesh = new THREE.Mesh(geo, mat);
      mesh.renderOrder = 1;
      world.add(mesh);
    }
    const packet = new THREE.Mesh(
      new THREE.SphereGeometry(0.09, 16, 12),
      new THREE.MeshPhysicalMaterial({
        color: 0x9aecff,
        roughness: 0.22,
        metalness: 0.08,
        emissive: 0x2a8aaa,
        emissiveIntensity: 0.7,
        transparent: true,
        opacity: 0.95,
      }),
    );
    packet.position.copy(end);
    packet.userData.projectId = t.sessionId;
    world.add(packet);
    somaHits.push(packet);
    cores.push({
      mesh: packet,
      rest: (packet.material as THREE.MeshPhysicalMaterial).emissiveIntensity,
      id: t.sessionId,
    });
    nameOf.set(t.sessionId, t.label ?? t.sessionId);
    addLabel(t.label ?? shortId(t.sessionId), end, 0.09, t.sessionId);
  }

  function shortId(id: string) {
    if (id.startsWith("agent-")) return id.length > 14 ? id.slice(0, 14) : id;
    return id.slice(0, 8);
  }

  function addUnlabeledGlow(host: LabBody, count: number) {
    const shell = new THREE.Mesh(
      new THREE.SphereGeometry(host.somaR * 1.045, 20, 16),
      new THREE.MeshBasicMaterial({
        color: 0x5ee7ff,
        transparent: true,
        opacity: Math.min(0.16, 0.05 + count * 0.03),
        depthWrite: false,
      }),
    );
    shell.position.copy(host.pos);
    shell.userData.projectId = host.project.id;
    world.add(shell);
    const rng = mulberry32(hash32(host.project.id) ^ 0x6105);
    const n = Math.min(count, 4);
    for (let i = 0; i < n; i++) {
      const y = rng() * 2 - 1;
      const phi = rng() * Math.PI * 2;
      const rr = Math.sqrt(Math.max(0, 1 - y * y));
      const dir = new THREE.Vector3(rr * Math.cos(phi), y, rr * Math.sin(phi)).normalize();
      const mote = new THREE.Mesh(
        new THREE.SphereGeometry(0.04, 10, 8),
        new THREE.MeshBasicMaterial({
          color: 0x9aecff,
          transparent: true,
          opacity: 0.48,
          depthWrite: false,
        }),
      );
      mote.position.copy(host.pos).addScaledVector(dir, host.somaR * 0.98);
      mote.userData.projectId = host.project.id;
      world.add(mote);
    }
  }

  function addTufts(list: SessionTuft[], hosts: LabBody[]) {
    const byId = new Map(hosts.map((b) => [b.project.id, b]));
    const unlabeled = new Map<string, number>();
    for (const t of list) {
      const host = byId.get(t.checkoutId);
      if (!host) continue;
      if (t.labeled) addLabeledTuft(t, host);
      else unlabeled.set(t.checkoutId, (unlabeled.get(t.checkoutId) ?? 0) + 1);
    }
    for (const [id, n] of unlabeled) {
      const host = byId.get(id);
      if (host) addUnlabeledGlow(host, n);
    }
  }

  function addDust() {
    if (dustGroup.children.length) return;
    const dustGeo = new THREE.BufferGeometry();
    const dustN = 160;
    const dust = new Float32Array(dustN * 3);
    const drng = mulberry32(0x11ee);
    for (let i = 0; i < dustN; i++) {
      dust[i * 3] = (drng() - 0.5) * 48;
      dust[i * 3 + 1] = (drng() - 0.5) * 28;
      dust[i * 3 + 2] = (drng() - 0.5) * 48;
    }
    dustGeo.setAttribute("position", new THREE.BufferAttribute(dust, 3));
    dustGroup.add(
      new THREE.Points(
        dustGeo,
        new THREE.PointsMaterial({
          color: 0x3a5060,
          size: 0.04,
          transparent: true,
          opacity: 0.28,
          depthWrite: false,
        }),
      ),
    );
  }

  function captureFitBox() {
    const box = new THREE.Box3();
    if (isNeighborhood()) {
      const center = hubPos ? hubPos.clone() : new THREE.Vector3();
      box.expandByPoint(new THREE.Vector3(center.x + HUB_HALO, center.y + HUB_HALO, center.z + HUB_HALO));
      box.expandByPoint(new THREE.Vector3(center.x - HUB_HALO, center.y - HUB_HALO, center.z - HUB_HALO));
      let r = HUB_HALO;
      for (const b of bodies) {
        const reach = b.somaR;
        box.expandByPoint(new THREE.Vector3(b.pos.x + reach, b.pos.y + reach, b.pos.z + reach));
        box.expandByPoint(new THREE.Vector3(b.pos.x - reach, b.pos.y - reach, b.pos.z - reach));
        r = Math.max(r, b.pos.distanceTo(center) + reach);
      }
      worldSize = box.getSize(new THREE.Vector3());
      worldCenter = center.clone();
      fitSphere.center.copy(center);
      fitSphere.radius = r;
      return;
    }
    box.setFromObject(world);
    // scoped field: fit the constellation itself, not the ambient dust volume
    if (!isScopedField() && dustGroup.children.length) {
      box.union(new THREE.Box3().setFromObject(dustGroup));
    }
    if (!box.isEmpty()) {
      worldSize = box.getSize(new THREE.Vector3());
      worldCenter = box.getCenter(new THREE.Vector3());
      box.getBoundingSphere(fitSphere);
    }
  }

  function clearWorld() {
    while (world.children.length) {
      const ch = world.children[0];
      world.remove(ch);
      disposeObject(ch);
    }
    labelLayer.replaceChildren();
    bodies = [];
    hubPos = null;
    somaHits = [];
    labels = [];
    neuronCharge = [];
    neuronAmp = [];
    neuronIds = [];
    axonCharge = [];
    cores = [];
    nameOf.clear();
    scopedNodeObjs = new Map();
    scopedEdgeObjs = [];
    scopedAdj = new Map();
  }

  function paint() {
    clearWorld();
    synapseTubeMat = null;
    const scopedProject = scope ? projectById(scope) : null;
    if (isScopedField() && scopedProject) {
      addScopedField(scopedProject);
    } else if (isNeighborhood() && scopedProject) {
      const ids: readonly LabIdentity[] = interiorIdentities(scopedProject, grain);
      const laid = layoutAroundHub(ids);
      hubPos = laid.hub.clone();
      bodies = laid.bodies;
      addHubGlyph(laid.hub);
      for (const b of bodies) addCheckoutOrb(b);
      for (const b of bodies) addHairline(laid.hub, b);
      if (showTufts()) addTufts(sessionTufts(scopedProject), bodies);
    } else {
      bodies = layoutVoxeloField(projectIdentities());
      addNeurons(bodies);
      addPipes(attributionPipes(bodies));
      if (driven) addRegistryHub();
    }
    addDust();
    captureFitBox();
    inspect.textContent = idleInspect();
    applyPresentation();
  }

  function applyPresentation() {
    const dimOthers = brainState === "hover" || brainState === "synapse";
    const raiseId = dimOthers ? (focusId ?? TRACEDECAY_ID) : hoverId;
    for (let i = 0; i < neuronCharge.length; i++) {
      const id = neuronIds[i];
      const hot = raiseId != null && raiseId === id;
      const factor = !dimOthers ? 1 : hot ? 1.35 : 0.22;
      neuronCharge[i].uniforms.uAmp.value = neuronAmp[i] * factor;
    }
    // conduction travels the tube only while the admitted event is shown
    if (synapseTubeMat) {
      synapseTubeMat.uniforms.uAmp.value =
        brainState === "synapse" ? SYNAPSE_EVENT.hopEnergy * 2.6 : 0;
    }
    bloom.strength = brainState === "synapse" ? 0.72 : 0.34;
    for (const c of cores) {
      const mat = c.mesh.material as THREE.MeshPhysicalMaterial;
      const hot = (dimOthers ? raiseId : hoverId) === c.id;
      const dim = dimOthers && c.id !== raiseId;
      mat.emissiveIntensity = (dim ? c.rest * 0.25 : c.rest) + (hot ? 0.35 : 0);
    }
    for (const l of labels) {
      const dim = dimOthers && l.id !== raiseId && l.id !== HUB_ID;
      l.el.classList.toggle("is-dim", dim);
      l.el.style.opacity = dim ? "0.28" : "1";
    }
    applyScopedIsolation();
  }

  /**
   * Scoped hover isolates: the hovered node and its evidenced relations stay
   * lit, the rest of the constellation recedes. Inspection only — no scope
   * change, no synapse, no invented nodes.
   */
  function applyScopedIsolation() {
    if (!isScopedField() || !scopedNodeObjs.size) return;
    const hv = hoverId && scopedNodeObjs.has(hoverId) ? hoverId : null;
    const near = hv ? scopedAdj.get(hv) : null;
    for (const [id, objs] of scopedNodeObjs) {
      const kept = !hv || id === hv || near?.has(id);
      for (const o of objs) {
        (o.obj.material as THREE.MeshBasicMaterial).opacity = o.base * (kept ? 1 : 0.16);
      }
    }
    for (const e of scopedEdgeObjs) {
      const mat = e.line.material as THREE.LineBasicMaterial;
      if (!hv) mat.opacity = e.base;
      else if (e.a === hv || e.b === hv) mat.opacity = Math.min(0.85, e.base * 2.4);
      else mat.opacity = e.base * 0.18;
    }
    for (const l of labels) {
      const kept = !hv || l.id === hv || near?.has(l.id);
      l.el.classList.toggle("is-dim", !kept);
    }
  }

  const controls = new OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;
  controls.dampingFactor = 0.08;
  controls.autoRotate = !reduced;
  controls.autoRotateSpeed = 0.22;
  controls.enablePan = true;
  controls.screenSpacePanning = true;
  controls.rotateSpeed = 0.42;
  controls.panSpeed = 0.32;
  controls.zoomSpeed = 0.55;

  const composer = new EffectComposer(renderer);
  composer.addPass(new RenderPass(scene, camera));
  const bloom = new UnrealBloomPass(new THREE.Vector2(4, 4), 0.34, 0.2, 0.84);
  composer.addPass(bloom);
  composer.addPass(new OutputPass());

  const camFrom = new THREE.Vector3();
  const camTo = new THREE.Vector3();
  const targetFrom = new THREE.Vector3();
  const targetTo = new THREE.Vector3();
  let camT = 1;

  function setCamGoal(pos: THREE.Vector3, target: THREE.Vector3, tween: boolean) {
    if (!tween || reduced) {
      camera.position.copy(pos);
      controls.target.copy(target);
      camera.lookAt(target);
      camT = 1;
      return;
    }
    camFrom.copy(camera.position);
    targetFrom.copy(controls.target);
    camTo.copy(pos);
    targetTo.copy(target);
    camT = 0;
  }

  function fitCamera(tween: boolean) {
    const neighborhood = isNeighborhood();
    if (neighborhood) {
      // Near top-down / slight 3/4 so three orbs read as a triangle of spheres.
      // Fit hub + checkout orbs only (dust excluded). Occupy ~65% of the aperture.
      const occupy = 0.65;
      const center = hubPos ? hubPos.clone() : fitSphere.center.clone();
      const R = Math.max(1.2, fitSphere.radius);
      const vFov = (camera.fov * Math.PI) / 180;
      const aspect = Math.max(0.35, camera.aspect || 1);
      const hFov = 2 * Math.atan(Math.tan(vFov / 2) * aspect);
      const elev = (62 * Math.PI) / 180;
      const azim = (20 * Math.PI) / 180;
      const dist = R / occupy / Math.tan(Math.min(vFov, hFov) / 2) / zoomMul;
      const pos = new THREE.Vector3(
        center.x + dist * Math.cos(elev) * Math.sin(azim),
        center.y + dist * Math.sin(elev),
        center.z + dist * Math.cos(elev) * Math.cos(azim),
      );
      controls.minDistance = Math.max(1.15, R * 0.35);
      controls.maxDistance = Math.max(dist * 1.8, R * 8);
      camera.far = Math.max(140, controls.maxDistance * 1.5);
      camera.near = 0.2;
      setCamGoal(pos, center, tween);
      camera.updateProjectionMatrix();
      return;
    }
    if (isScopedField()) {
      // high 3/4 so the cluster ring reads as a spread constellation field
      const occupy = 0.72;
      const center = fitSphere.center.clone();
      const R = Math.max(4, fitSphere.radius);
      const vFov = (camera.fov * Math.PI) / 180;
      const aspect = Math.max(0.35, camera.aspect || 1);
      const hFov = 2 * Math.atan(Math.tan(vFov / 2) * aspect);
      const elev = (54 * Math.PI) / 180;
      const azim = (14 * Math.PI) / 180;
      const dist = R / occupy / Math.tan(Math.min(vFov, hFov) / 2) / zoomMul;
      const pos = new THREE.Vector3(
        center.x + dist * Math.cos(elev) * Math.sin(azim),
        center.y + dist * Math.sin(elev),
        center.z + dist * Math.cos(elev) * Math.cos(azim),
      );
      controls.minDistance = Math.max(2, R * 0.3);
      controls.maxDistance = Math.max(dist * 1.8, R * 8);
      camera.far = Math.max(140, controls.maxDistance * 1.5);
      camera.near = 0.2;
      setCamGoal(pos, center, tween);
      camera.updateProjectionMatrix();
      return;
    }
    const center = worldCenter.clone();
    const size = worldSize;
    const aspect = camera.aspect || 1;
    const fit = Math.max(size.x / Math.max(0.35, aspect), size.y, size.z * 0.55) * 1.12;
    const baseZ = Math.max(22, fit * 1.72);
    const z = baseZ / zoomMul;
    const pos = new THREE.Vector3(center.x + 2.8, center.y + size.y * 0.2, z);
    controls.minDistance = 12;
    controls.maxDistance = 64;
    camera.near = 0.2;
    camera.far = 140;
    setCamGoal(pos, center, tween);
    camera.updateProjectionMatrix();
  }

  function layout() {
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    renderer.setSize(w, h, false);
    composer.setSize(w, h);
    bloom.setSize(w, h);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
    fitCamera(false);
    controls.update();
  }

  paint();
  layout();
  const ro = new ResizeObserver(layout);
  ro.observe(host);

  const ray = new THREE.Raycaster();
  const pointer = new THREE.Vector2();
  const tmp = new THREE.Vector3();
  const edge = new THREE.Vector3();

  function projectLabels() {
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    type Place = "right" | "under" | "left";
    type Box = {
      el: HTMLDivElement;
      x: number;
      y: number;
      w: number;
      h: number;
      hidden: boolean;
      dim: boolean;
      place: Place;
      id: string;
    };
    const placed: Box[] = [];
    const camRight = new THREE.Vector3().setFromMatrixColumn(camera.matrixWorld, 0).normalize();
    const checkoutIds = new Set(bodies.map((b) => b.project.id));
    // CONCEPT / PROFILE SNAPSHOT: hub keepout so checkout chips never sit on the glyph.
    let hubX = 0;
    let hubY = 0;
    let hubR = 0;
    if (hubPos) {
      tmp.copy(hubPos).project(camera);
      edge.copy(hubPos).addScaledVector(camRight, HUB_HALO).project(camera);
      hubX = (tmp.x * 0.5 + 0.5) * w;
      hubY = (-tmp.y * 0.5 + 0.5) * h;
      hubR = Math.max(12, Math.abs(edge.x - tmp.x) * 0.5 * w) + 10;
    }
    for (const l of labels) {
      tmp.copy(l.pos).project(camera);
      edge.copy(l.pos).addScaledVector(camRight, l.radius).project(camera);
      const rPx = Math.max(14, Math.abs(edge.x - tmp.x) * 0.5 * w);
      const lw = l.el.offsetWidth || 72;
      const lh = l.el.offsetHeight || 18;
      const cx = (tmp.x * 0.5 + 0.5) * w;
      const cy = (-tmp.y * 0.5 + 0.5) * h;
      let place: Place = l.place;
      let x: number;
      let y: number;
      if (place === "under") {
        x = cx - lw * 0.5;
        y = cy + rPx + 22;
      } else {
        // Default: right of checkout orbs. Flip left-of-orb when the right
        // chip would cover the hub / hairline junction (the left orb).
        if (hubPos && checkoutIds.has(l.id)) {
          const rx = cx + rPx + 12;
          const ry = cy - lh * 0.4;
          const overlapsHub =
            rx < hubX + hubR && rx + lw > hubX - hubR && ry < hubY + hubR && ry + lh > hubY - hubR;
          if (overlapsHub) place = "left";
        }
        if (place === "left") {
          x = cx - rPx - 12 - lw;
          y = cy - lh * 0.15;
        } else {
          x = cx + rPx + 12;
          y = cy - lh * 0.4;
        }
      }
      x = Math.min(w - lw - 8, Math.max(8, x));
      y = Math.min(h - lh - 8, Math.max(8, y));
      placed.push({
        el: l.el,
        x,
        y,
        w: lw,
        h: lh,
        hidden: tmp.z > 1,
        dim: l.el.classList.contains("is-dim"),
        place,
        id: l.id,
      });
    }
    const gapX = 10;
    const gapY = 8;
    for (let iter = 0; iter < 24; iter++) {
      placed.sort((a, b) => a.y - b.y || a.x - b.x);
      let moved = false;
      for (let i = 1; i < placed.length; i++) {
        for (let j = 0; j < i; j++) {
          const a = placed[j];
          const b = placed[i];
          const ox = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
          const oy = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
          if (ox > -gapX && oy > -gapY) {
            const aOrb = checkoutIds.has(a.id);
            const bOrb = checkoutIds.has(b.id);
            if (b.place === "under") b.y = Math.max(b.y, a.y + a.h + gapY);
            else if (aOrb !== bOrb && a.place !== "under") {
              // Packet chip stays near its checkout orb — stack above, don't slide into it.
              const tuft = aOrb ? b : a;
              const orb = aOrb ? a : b;
              tuft.y = Math.min(tuft.y, orb.y - tuft.h - gapY);
            } else if (b.place === "left") b.x = Math.min(b.x, a.x - b.w - gapX);
            else b.x = Math.max(b.x, a.x + a.w + gapX);
            moved = true;
          }
        }
      }
      if (!moved) break;
    }
    for (const box of placed) {
      box.x = Math.min(w - box.w - 8, Math.max(8, box.x));
      box.y = Math.min(h - box.h - 8, Math.max(8, box.y));
      box.el.style.transform = `translate(${box.x}px, ${box.y}px)`;
      box.el.style.opacity = box.hidden ? "0" : box.dim ? "0.28" : "1";
    }
    for (const l of labels) {
      l.el.classList.toggle("is-hot", hoverId === l.id);
    }
  }

  function hitId(): string | undefined {
    const hits = ray.intersectObjects(somaHits, true);
    return hits[0]?.object.userData.projectId as string | undefined;
  }

  function pick(ev: PointerEvent) {
    const rect = renderer.domElement.getBoundingClientRect();
    pointer.x = ((ev.clientX - rect.left) / rect.width) * 2 - 1;
    pointer.y = -((ev.clientY - rect.top) / rect.height) * 2 + 1;
    ray.setFromCamera(pointer, camera);
    const id = hitId();
    if (id && id !== hoverId) {
      hoverId = id;
      inspect.textContent = `${nameOf.get(id) ?? id} · inspect`;
      // scoped-field ids are graph-local (h:*, s:*, grp:*…), never projects:
      // hover inspects locally and must not move the global project focus
      if (id !== HUB_ID && !isScopedField()) hooks.onFocus?.(id);
      applyPresentation();
    }
    if (!id && hoverId) {
      hoverId = null;
      inspect.textContent = idleInspect();
      applyPresentation();
    }
    renderer.domElement.style.cursor = id && enterOnClick() && id !== HUB_ID ? "pointer" : "";
  }

  renderer.domElement.addEventListener("pointermove", pick);

  let down: { x: number; y: number } | null = null;
  function onPointerDown(ev: PointerEvent) {
    down = { x: ev.clientX, y: ev.clientY };
  }
  function onPointerUp(ev: PointerEvent) {
    if (!down) return;
    const dx = ev.clientX - down.x;
    const dy = ev.clientY - down.y;
    down = null;
    if (dx * dx + dy * dy > 64) return;
    const rect = renderer.domElement.getBoundingClientRect();
    pointer.x = ((ev.clientX - rect.left) / rect.width) * 2 - 1;
    pointer.y = -((ev.clientY - rect.top) / rect.height) * 2 + 1;
    ray.setFromCamera(pointer, camera);
    const id = hitId();
    if (id === HUB_ID) return;
    if (enterOnClick()) {
      if (id) hooks.onEnterScope?.(id);
      return;
    }
    if (!id && leaveOnEmpty()) hooks.onLeaveScope?.();
  }
  function onKey(ev: KeyboardEvent) {
    if (ev.key === "Escape" && leaveOnEmpty()) hooks.onLeaveScope?.();
  }
  renderer.domElement.addEventListener("pointerdown", onPointerDown);
  renderer.domElement.addEventListener("pointerup", onPointerUp);
  window.addEventListener("keydown", onKey);

  let raf = 0;
  const clock = new THREE.Clock();

  function frame() {
    const dt = clock.getDelta();
    const t = clock.elapsedTime;
    if (camT < 1) {
      camT = Math.min(1, camT + dt * 1.8);
      const k = camT * camT * (3 - 2 * camT);
      camera.position.lerpVectors(camFrom, camTo, k);
      controls.target.lerpVectors(targetFrom, targetTo, k);
    }
    controls.update();
    if (!reduced) {
      for (let i = 0; i < neuronCharge.length; i++) {
        const phase = i * 0.17;
        neuronCharge[i].uniforms.uHead.value = (t * 0.055 + phase) % 1;
      }
      for (const ax of axonCharge) {
        ax.mat.uniforms.uHead.value = (t * ax.speed + ax.phase) % 1;
      }
    }
    if (!(brainState === "hover" || brainState === "synapse")) {
      for (const c of cores) {
        const mat = c.mesh.material as THREE.MeshPhysicalMaterial;
        const hot = hoverId === c.id;
        mat.emissiveIntensity = c.rest + (hot ? 0.35 : 0);
      }
    }
    projectLabels();
    composer.render();
    raf = requestAnimationFrame(frame);
  }
  frame();

  return {
    dispose() {
      cancelAnimationFrame(raf);
      ro.disconnect();
      controls.dispose();
      renderer.domElement.removeEventListener("pointermove", pick);
      renderer.domElement.removeEventListener("pointerdown", onPointerDown);
      renderer.domElement.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("keydown", onKey);
      composer.dispose();
      renderer.dispose();
      renderer.domElement.remove();
      labelLayer.remove();
      inspect.remove();
      envTex.dispose();
      disposeObject(scene);
    },
    setFocus(id: string | null) {
      focusId = id;
      applyPresentation();
    },
    setBrainState(state: BrainState) {
      const prev = topologyKey();
      brainState = state;
      if (state === "hover" || state === "synapse") focusId = TRACEDECAY_ID;
      if (state === "overview") focusId = null;
      if ((state === "repo-zoom" || state === "scoped") && !scope) scope = TRACEDECAY_ID;
      if (driven && state !== "repo-zoom" && state !== "scoped") scope = hooks.scope ?? null;
      if (topologyKey() !== prev) {
        paint();
        fitCamera(!reduced);
      } else {
        applyPresentation();
        fitCamera(!reduced);
      }
      inspect.textContent = idleInspect();
    },
    zoom(delta: number | "fit") {
      if (delta === "fit") zoomMul = 1;
      else zoomMul = Math.max(0.4, Math.min(4, zoomMul * (delta > 0 ? 1.15 : 1 / 1.15)));
      fitCamera(!reduced);
    },
  };
}
