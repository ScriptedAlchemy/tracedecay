import * as THREE from "three";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";
import { mergeGeometries } from "three/addons/utils/BufferGeometryUtils.js";
import { buildArbor, mulberry32, type SomaRec } from "./firingTreeArbor";

const BG = 0x05070e;
const SOMA = 0x3aa0e6;
const SOMA_DIM = 0x1d5c82;
const SPIKE = 0x152a48;
const TUBE = 0x1e5a88;
const TUBE_DEEP = 0x163e62;
const PULSE_ICE = 0xe8ffff;
const PULSE_CYAN = 0xb8ffff;

const UP = new THREE.Vector3(0, 1, 0);

function somaMat(kind: SomaRec["kind"], dim: boolean) {
  const root = kind === "root";
  const session = kind === "session";
  return new THREE.MeshPhysicalMaterial({
    color: dim ? SOMA_DIM : SOMA,
    roughness: root ? 0.16 : 0.2,
    metalness: 0.2,
    clearcoat: 1,
    clearcoatRoughness: 0.07,
    emissive: new THREE.Color(dim ? 0x071828 : root ? 0x1a6aa0 : session ? 0x145888 : 0x0c3a68),
    emissiveIntensity: dim ? 0.35 : root ? 1.05 : session ? 0.9 : 0.7,
    sheen: 0.28,
    sheenColor: new THREE.Color(0x7ec8ff),
    envMapIntensity: 0.9,
  });
}

function addSpikes(
  group: THREE.Group,
  origin: THREE.Vector3,
  radius: number,
  count: number,
  rng: () => number,
  scale: number,
  mat: THREE.Material,
  major: boolean,
) {
  const geo = new THREE.ConeGeometry((major ? 0.03 : 0.024) * scale, 0.24 * scale, 5);
  const base = major ? 0.19 : 0.12;
  for (let i = 0; i < count; i++) {
    const y = rng() * 2 - 1;
    const phi = rng() * Math.PI * 2;
    const rr = Math.sqrt(Math.max(0, 1 - y * y));
    const dir = new THREE.Vector3(rr * Math.cos(phi), y, rr * Math.sin(phi)).normalize();
    const len = base * scale * (0.72 + rng() * 0.55);
    const mesh = new THREE.Mesh(geo, mat);
    mesh.scale.set(1, len / (0.24 * scale), 1);
    mesh.position.copy(origin).addScaledVector(dir, radius + len * 0.42);
    mesh.quaternion.setFromUnitVectors(UP, dir);
    group.add(mesh);
  }
}

function spikeCount(kind: SomaRec["kind"]) {
  if (kind === "root") return 64;
  if (kind === "session") return 34;
  if (kind === "basal") return 7;
  return 8;
}

type PulseTrain = {
  curve: THREE.CatmullRomCurve3;
  destId?: string;
  capsules: THREE.Mesh[];
  speed: number;
  offset: number;
  spacing: number;
  prevHead: number;
};

type LabelEl = {
  el: HTMLDivElement;
  pos: THREE.Vector3;
  radius: number;
};

export function mountFiringTree(host: HTMLElement): { dispose: () => void } {
  const { tubes, somas, root } = buildArbor();
  const rng = mulberry32(0xc0de);

  const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false });
  renderer.setClearColor(BG, 1);
  renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.12;
  host.appendChild(renderer.domElement);

  const labelLayer = document.createElement("div");
  labelLayer.className = "ft-labels";
  host.appendChild(labelLayer);

  const scene = new THREE.Scene();
  scene.background = new THREE.Color(BG);

  const pmrem = new THREE.PMREMGenerator(renderer);
  const envTex = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environment = envTex;
  scene.environmentIntensity = 0.55;
  pmrem.dispose();

  const camera = new THREE.PerspectiveCamera(32, 1, 0.1, 80);
  scene.add(new THREE.AmbientLight(0x3a5570, 0.85));
  scene.add(new THREE.HemisphereLight(0xc5e4ff, 0x0a1522, 0.95));
  const key = new THREE.DirectionalLight(0xffffff, 2.15);
  key.position.set(-3.4, 5.8, 5.2);
  scene.add(key);
  const fill = new THREE.DirectionalLight(0x7eb7e6, 0.95);
  fill.position.set(4.8, 1.4, 3.4);
  scene.add(fill);
  const rim = new THREE.PointLight(0x66e7ff, 2.4, 14);
  rim.position.copy(root.pos).add(new THREE.Vector3(0.45, 0.7, 1.1));
  scene.add(rim);
  const kick = new THREE.DirectionalLight(0xffffff, 0.7);
  kick.position.set(0.2, 1.2, 6.5);
  scene.add(kick);

  const tree = new THREE.Group();
  scene.add(tree);

  const trunkMat = new THREE.MeshPhysicalMaterial({
    color: TUBE,
    roughness: 0.22,
    metalness: 0.5,
    clearcoat: 0.65,
    clearcoatRoughness: 0.18,
    emissive: 0x0a355c,
    emissiveIntensity: 0.48,
  });
  const twigMat = new THREE.MeshPhysicalMaterial({
    color: TUBE_DEEP,
    roughness: 0.32,
    metalness: 0.38,
    clearcoat: 0.35,
    clearcoatRoughness: 0.28,
    emissive: 0x082438,
    emissiveIntensity: 0.3,
  });

  const twigGeos: THREE.BufferGeometry[] = [];
  const pulseRoutes: { curve: THREE.CatmullRomCurve3; destId?: string; depth: number; radius: number }[] = [];

  for (const t of tubes) {
    const pts = t.pts.slice();
    if (t.destId === "root") {
      if (pts[pts.length - 1].distanceToSquared(root.pos) > pts[0].distanceToSquared(root.pos)) pts.reverse();
    }
    const curve = new THREE.CatmullRomCurve3(pts, false, "catmullrom", 0.32);
    const segs = t.depth <= 1 ? 48 : t.depth <= 2 ? 36 : t.depth <= 4 ? 20 : 10;
    const radial = t.depth <= 2 ? 8 : 6;
    const tubeR =
      t.depth === 0 ? Math.max(t.radius, 0.034) :
      t.depth === 1 ? Math.max(t.radius, 0.028) :
      t.depth === 2 ? Math.max(t.radius, 0.022) :
      t.radius;
    const geo = new THREE.TubeGeometry(curve, segs, tubeR, radial, false);
    if (t.depth <= 2) tree.add(new THREE.Mesh(geo, trunkMat));
    else twigGeos.push(geo);
    if (t.depth <= 4) {
      pulseRoutes.push({ curve, destId: t.destId, depth: t.depth, radius: tubeR });
    }
  }
  if (twigGeos.length) {
    const merged = mergeGeometries(twigGeos, false);
    if (merged) tree.add(new THREE.Mesh(merged, twigMat));
    for (const g of twigGeos) g.dispose();
  }

  const somaGroup = new THREE.Group();
  tree.add(somaGroup);
  const somaMeshes = new Map<string, THREE.Mesh>();
  const somaRest = new Map<string, number>();
  const somaFlash = new Map<string, number>();
  const spikeMat = new THREE.MeshStandardMaterial({
    color: SPIKE,
    roughness: 0.5,
    metalness: 0.38,
    emissive: 0x050a14,
    emissiveIntensity: 0.22,
  });
  for (const s of somas) {
    const mesh = new THREE.Mesh(new THREE.SphereGeometry(s.radius, 36, 28), somaMat(s.kind, !!s.dim));
    mesh.position.copy(s.pos);
    somaGroup.add(mesh);
    somaMeshes.set(s.id, mesh);
    const rest = (mesh.material as THREE.MeshPhysicalMaterial).emissiveIntensity;
    somaRest.set(s.id, rest);
    somaFlash.set(s.id, rest);
    const major = s.kind === "root" || s.kind === "session";
    addSpikes(somaGroup, s.pos, s.radius, spikeCount(s.kind), rng, s.radius / 0.2, spikeMat, major);
  }

  const iceMat = new THREE.MeshBasicMaterial({
    color: PULSE_ICE,
    transparent: true,
    blending: THREE.AdditiveBlending,
    depthWrite: false,
    depthTest: false,
    toneMapped: false,
  });
  const cyanMat = new THREE.MeshBasicMaterial({
    color: PULSE_CYAN,
    transparent: true,
    blending: THREE.AdditiveBlending,
    depthWrite: false,
    depthTest: false,
    toneMapped: false,
  });

  const trains: PulseTrain[] = [];
  for (const route of pulseRoutes) {
    const len = Math.max(0.001, route.curve.getLength());
    // World-unit hero dashes (not 0.28×tiny-tube). Camera fits the tree in ~6
    // units; pulseR 0.028–0.045 / dashH 0.14–0.22 photograph as fat capsules.
    const pulseR =
      route.depth === 0 ? 0.042 :
      route.depth === 1 ? 0.038 :
      route.depth === 2 ? 0.034 :
      0.030;
    const dashH =
      route.depth === 0 ? 0.20 :
      route.depth === 1 ? 0.18 :
      route.depth === 2 ? 0.16 :
      0.145;
    const dashWorld = dashH + pulseR * 2;
    const gapWorld = dashWorld * 0.68;
    const stride = dashWorld + gapWorld;
    // ~60% of axon length as a dashed stream (6-ish fat dashes on a 2-unit axon).
    const n = THREE.MathUtils.clamp(Math.round(len / stride), 2, 10);
    const geo = new THREE.CapsuleGeometry(pulseR, dashH, 4, 10);
    const mat = rng() < 0.55 ? iceMat : cyanMat;
    const capsules = Array.from({ length: n }, () => {
      const m = new THREE.Mesh(geo, mat);
      m.renderOrder = 2;
      tree.add(m);
      return m;
    });
    trains.push({
      curve: route.curve,
      destId: route.destId,
      capsules,
      speed: 0.16 + rng() * 0.1 + route.depth * 0.012,
      offset: (route.depth * 0.17 + rng() * 0.5) % 1,
      spacing: 1 / n,
      prevHead: 0,
    });
  }

  const labels: LabelEl[] = [];
  for (const s of somas) {
    if (s.kind !== "root" && s.kind !== "session") continue;
    if (!s.label) continue;
    const el = document.createElement("div");
    el.className = s.kind === "root" ? "ft-label ft-label-root" : "ft-label ft-label-session";
    const sub = s.sub
      ? s.kind === "session"
        ? `<div class="stats"><div class="ft-wt">${s.sub}</div></div>`
        : `<div class="stats">${s.sub}</div>`
      : "";
    el.innerHTML = `<div class="name">${s.label}</div>${sub}`;
    labelLayer.appendChild(el);
    labels.push({ el, pos: s.pos, radius: s.radius });
  }

  const tmp = new THREE.Vector3();
  const tan = new THREE.Vector3();
  const edge = new THREE.Vector3();
  const box = new THREE.Box3().setFromObject(tree);
  const size = box.getSize(new THREE.Vector3());
  const center = box.getCenter(new THREE.Vector3());

  function layout() {
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    const fit = Math.max(size.x / camera.aspect, size.y) * 1.1;
    camera.position.set(center.x + 0.25, center.y + size.y * 0.03, Math.max(6.2, fit * 1.72));
    camera.lookAt(center.x, center.y + size.y * 0.08, 0);
    camera.updateProjectionMatrix();
  }
  layout();
  const ro = new ResizeObserver(layout);
  ro.observe(host);

  let raf = 0;
  const clock = new THREE.Clock();
  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches ?? false;

  function flashSoma(id: string | undefined) {
    if (!id) return;
    const rest = somaRest.get(id) ?? 0.7;
    somaFlash.set(id, rest + 2.4);
  }

  type LabelBox = { el: HTMLDivElement; x: number; y: number; w: number; h: number; hidden: boolean };

  function projectLabels() {
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    const placed: LabelBox[] = [];
    for (const l of labels) {
      tmp.copy(l.pos).project(camera);
      edge.copy(l.pos).setX(l.pos.x + l.radius).project(camera);
      const rPx = Math.max(14, Math.abs(edge.x - tmp.x) * 0.5 * w);
      const lw = l.el.offsetWidth || 72;
      const lh = l.el.offsetHeight || 28;
      let x = (tmp.x * 0.5 + 0.5) * w + rPx + 10;
      let y = (-tmp.y * 0.5 + 0.5) * h - lh * 0.38;
      x = Math.min(w - lw - 8, Math.max(8, x));
      y = Math.min(h - lh - 8, Math.max(8, y));
      placed.push({ el: l.el, x, y, w: lw, h: lh, hidden: tmp.z > 1 });
    }
    const gapX = 8;
    const gapY = 18;
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
            b.y = a.y + a.h + gapY;
            moved = true;
          }
        }
      }
      if (!moved) break;
    }
    for (const p of placed) {
      p.y = Math.min(h - p.h - 8, Math.max(8, p.y));
      p.el.style.transform = `translate(${p.x}px, ${p.y}px)`;
      p.el.style.opacity = p.hidden ? "0" : "1";
    }
  }

  function wrap01(u: number) {
    u = u % 1;
    return u < 0 ? u + 1 : u;
  }

  function frame() {
    const dt = clock.getDelta();
    const t = clock.getElapsedTime();
    if (!reduced) {
      tree.rotation.y = Math.sin(t * 0.11) * 0.08;
      for (const tr of trains) {
        const head = wrap01(t * tr.speed + tr.offset);
        if (tr.prevHead > 0.72 && head < 0.28) flashSoma(tr.destId);
        tr.prevHead = head;
        for (let i = 0; i < tr.capsules.length; i++) {
          const u = wrap01(head - i * tr.spacing);
          const mesh = tr.capsules[i];
          if (u < 0.025 || u > 0.975) {
            mesh.visible = false;
            continue;
          }
          mesh.visible = true;
          tr.curve.getPointAt(u, mesh.position);
          tr.curve.getTangentAt(u, tan);
          mesh.quaternion.setFromUnitVectors(UP, tan);
        }
      }
      for (const [id, mesh] of somaMeshes) {
        const mat = mesh.material as THREE.MeshPhysicalMaterial;
        const rest = somaRest.get(id) ?? 0.7;
        let v = somaFlash.get(id) ?? rest;
        v = Math.max(rest, v - dt * 2.6);
        somaFlash.set(id, v);
        mat.emissiveIntensity = v;
      }
    }
    projectLabels();
    renderer.render(scene, camera);
    raf = requestAnimationFrame(frame);
  }
  frame();

  return {
    dispose() {
      cancelAnimationFrame(raf);
      ro.disconnect();
      renderer.dispose();
      renderer.domElement.remove();
      labelLayer.remove();
      envTex.dispose();
      scene.traverse((obj) => {
        const mesh = obj as THREE.Mesh;
        if (mesh.geometry) mesh.geometry.dispose();
        const mat = mesh.material as THREE.Material | THREE.Material[] | undefined;
        if (Array.isArray(mat)) mat.forEach((m) => m.dispose());
        else mat?.dispose();
      });
    },
  };
}
