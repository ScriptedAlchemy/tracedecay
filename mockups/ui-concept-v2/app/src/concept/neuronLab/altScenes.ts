import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { Line2 } from "three/addons/lines/Line2.js";
import { LineGeometry } from "three/addons/lines/LineGeometry.js";
import { LineMaterial } from "three/addons/lines/LineMaterial.js";
import { TRACEDECAY_PROJECT_ID, TRACEDECAY_WORKTREES, tracedecayArbor } from "../../data/tracedecay-pack";
import { PROJECTS } from "../../data/fixtures";
import {
  axonPts,
  disposeObject,
  hash32,
  layoutLabBodies,
  mulberry32,
  attributionPipes,
  type LabHooks,
} from "./labUtil";

function shortId(id: string) {
  if (id.startsWith("agent-")) return id.length > 9 ? `${id.slice(0, 9)}…` : id;
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

function boot(host: HTMLElement, bg: number) {
  const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false });
  renderer.setClearColor(bg, 1);
  renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  host.appendChild(renderer.domElement);
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(bg);
  const camera = new THREE.PerspectiveCamera(42, 1, 0.1, 80);
  const controls = new OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;
  controls.autoRotate = true;
  controls.autoRotateSpeed = 0.55;
  controls.enablePan = false;
  const labelLayer = document.createElement("div");
  labelLayer.className = "nl-labels";
  host.appendChild(labelLayer);
  const inspect = document.createElement("div");
  inspect.className = "nl-inspect";
  host.appendChild(inspect);
  return { renderer, scene, camera, controls, labelLayer, inspect };
}

function fit(camera: THREE.PerspectiveCamera, controls: OrbitControls, root: THREE.Object3D, host: HTMLElement) {
  const box = new THREE.Box3().setFromObject(root);
  const size = box.getSize(new THREE.Vector3());
  const center = box.getCenter(new THREE.Vector3());
  const w = host.clientWidth || 1;
  const h = host.clientHeight || 1;
  camera.aspect = w / h;
  const span = Math.max(size.x / Math.max(0.4, camera.aspect), size.y, size.z * 0.6);
  camera.position.set(center.x + 0.8, center.y + 0.6, Math.max(7.5, span * 1.7));
  camera.lookAt(center);
  camera.updateProjectionMatrix();
  controls.target.copy(center);
}

function bindFocus(
  renderer: THREE.WebGLRenderer,
  camera: THREE.PerspectiveCamera,
  hits: THREE.Object3D[],
  inspect: HTMLElement,
  hooks: LabHooks,
  labelOf: (id: string) => string,
) {
  const ray = new THREE.Raycaster();
  const pointer = new THREE.Vector2();
  let hover: string | null = null;
  const pick = (ev: PointerEvent) => {
    const rect = renderer.domElement.getBoundingClientRect();
    pointer.x = ((ev.clientX - rect.left) / rect.width) * 2 - 1;
    pointer.y = -((ev.clientY - rect.top) / rect.height) * 2 + 1;
    ray.setFromCamera(pointer, camera);
    const hit = ray.intersectObjects(hits, true)[0];
    const id = hit?.object.userData.projectId as string | undefined;
    if (id !== hover) {
      hover = id ?? null;
      inspect.textContent = id ? `${labelOf(id)} · inspect` : "hover · inspect only";
      if (id) hooks.onFocus?.(id);
    }
    renderer.domElement.style.cursor = id ? "pointer" : "";
  };
  renderer.domElement.addEventListener("pointermove", pick);
  return () => renderer.domElement.removeEventListener("pointermove", pick);
}

export function mountTubes(host: HTMLElement, hooks: LabHooks = {}): { dispose: () => void } {
  const { renderer, scene, camera, controls, labelLayer, inspect } = boot(host, 0x03040a);
  inspect.textContent = "TUBES · prisoner849-ish · hover inspect";
  scene.add(new THREE.AmbientLight(0x334466, 0.6));
  const world = new THREE.Group();
  scene.add(world);

  const ico = new THREE.Mesh(
    new THREE.IcosahedronGeometry(1.65, 1),
    new THREE.MeshBasicMaterial({ color: 0x3a6a99, wireframe: true, transparent: true, opacity: 0.35 }),
  );
  world.add(ico);

  const bodies = layoutLabBodies();
  const pipes = attributionPipes(bodies);
  const hits: THREE.Object3D[] = [];
  const labels: { el: HTMLDivElement; pos: THREE.Vector3 }[] = [];
  const signals: { mesh: THREE.Mesh; curve: THREE.CatmullRomCurve3; speed: number; phase: number }[] = [];

  const nodeMat = new THREE.MeshBasicMaterial({ color: 0xd0e6ff });
  for (const b of bodies) {
    const m = new THREE.Mesh(new THREE.SphereGeometry(0.09, 12, 10), nodeMat);
    m.position.copy(b.pos);
    m.userData.projectId = b.project.id;
    world.add(m);
    hits.push(m);
    const el = document.createElement("div");
    el.className = "nl-label";
    el.innerHTML = `<div class="name">${b.project.name}</div>`;
    labelLayer.appendChild(el);
    labels.push({ el, pos: b.pos });
  }

  const lineMat = new LineMaterial({
    color: 0xff40c8,
    linewidth: 2.4,
    transparent: true,
    opacity: 0.85,
    dashed: false,
    worldUnits: false,
  });
  lineMat.resolution.set(host.clientWidth || 1, host.clientHeight || 1);

  const sigMat = new THREE.MeshBasicMaterial({
    color: 0xffe066,
    transparent: true,
    blending: THREE.AdditiveBlending,
    depthWrite: false,
    toneMapped: false,
  });
  const sigGeo = new THREE.SphereGeometry(0.045, 8, 8);

  for (const pipe of pipes) {
    const rng = mulberry32(hash32(pipe.a.project.id + ">" + pipe.b.project.id));
    const pts = axonPts(pipe.a.pos, pipe.b.pos, rng);
    const curve = new THREE.CatmullRomCurve3(pts, false, "catmullrom", 0.3);
    const sampled = curve.getPoints(40);
    const geo = new LineGeometry();
    const flat: number[] = [];
    for (const p of sampled) flat.push(p.x, p.y, p.z);
    geo.setPositions(flat);
    const line = new Line2(geo, lineMat);
    world.add(line);
    for (let k = 0; k < 3; k++) {
      const mesh = new THREE.Mesh(sigGeo, sigMat);
      world.add(mesh);
      signals.push({ mesh, curve, speed: 0.12 + rng() * 0.1, phase: rng() });
    }
  }

  const arbor = tracedecayArbor();
  const td = bodies.find((b) => b.project.id === TRACEDECAY_PROJECT_ID);
  if (td) {
    const rng = mulberry32(0x849);
    for (let i = 0; i < Math.min(arbor.sessionWorktrees?.length ?? 0, 3); i += 1) {
      const p = td.pos.clone().add(new THREE.Vector3(rng() - 0.5, rng() * 0.4, rng() - 0.5).multiplyScalar(1.1));
      const m = new THREE.Mesh(new THREE.SphereGeometry(0.05, 8, 8), new THREE.MeshBasicMaterial({ color: 0x88aadd }));
      m.position.copy(p);
      m.userData.projectId = td.project.id;
      world.add(m);
    }
  }

  fit(camera, controls, world, host);
  const unbind = bindFocus(renderer, camera, hits, inspect, hooks, (id) => PROJECTS.find((p) => p.id === id)?.name ?? id);
  const tmp = new THREE.Vector3();
  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches ?? false;
  if (reduced) controls.autoRotate = false;
  let raf = 0;
  const clock = new THREE.Clock();
  const layout = () => {
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
    lineMat.resolution.set(w, h);
    fit(camera, controls, world, host);
  };
  layout();
  const ro = new ResizeObserver(layout);
  ro.observe(host);

  function frame() {
    const t = clock.getElapsedTime();
    controls.update();
    ico.rotation.y = t * 0.08;
    if (!reduced) {
      for (const s of signals) {
        const u = (t * s.speed + s.phase) % 1;
        s.curve.getPointAt(u, s.mesh.position);
      }
    }
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    for (const l of labels) {
      tmp.copy(l.pos).project(camera);
      const x = (tmp.x * 0.5 + 0.5) * w + 12;
      const y = (-tmp.y * 0.5 + 0.5) * h - 8;
      l.el.style.transform = `translate(${x}px, ${y}px)`;
    }
    renderer.render(scene, camera);
    raf = requestAnimationFrame(frame);
  }
  frame();
  return {
    dispose() {
      cancelAnimationFrame(raf);
      ro.disconnect();
      unbind();
      controls.dispose();
      renderer.dispose();
      renderer.domElement.remove();
      labelLayer.remove();
      inspect.remove();
      disposeObject(scene);
    },
  };
}

export function mountNet(host: HTMLElement, hooks: LabHooks = {}): { dispose: () => void } {
  const { renderer, scene, camera, controls, labelLayer, inspect } = boot(host, 0x05070e);
  inspect.textContent = "NET · nearby lines · hover inspect";
  const world = new THREE.Group();
  scene.add(world);
  const bodies = layoutLabBodies();
  const arbor = tracedecayArbor();
  const sessionIds = (arbor.sessionIds ?? []).slice(0, 4);

  type P = { pos: THREE.Vector3; vel: THREE.Vector3; id: string; label: string; ident: boolean };
  const particles: P[] = [];
  for (const b of bodies) {
    particles.push({
      pos: b.pos.clone().multiplyScalar(0.55),
      vel: new THREE.Vector3(),
      id: b.project.id,
      label: b.project.name,
      ident: true,
    });
  }
  const td = particles.find((p) => p.id === TRACEDECAY_PROJECT_ID);
  const origin = td?.pos.clone() ?? new THREE.Vector3();
  TRACEDECAY_WORKTREES.forEach((wt, i) => {
    const a = (i / 3) * Math.PI * 2;
    particles.push({
      pos: origin.clone().add(new THREE.Vector3(Math.cos(a), 0.2, Math.sin(a)).multiplyScalar(1.15)),
      vel: new THREE.Vector3(),
      id: TRACEDECAY_PROJECT_ID,
      label: wt,
      ident: true,
    });
  });
  sessionIds.forEach((sid, i) => {
    const a = (i / 4) * Math.PI * 2 + 0.3;
    particles.push({
      pos: origin.clone().add(new THREE.Vector3(Math.cos(a), -0.15, Math.sin(a)).multiplyScalar(1.7)),
      vel: new THREE.Vector3(),
      id: TRACEDECAY_PROJECT_ID,
      label: shortId(sid),
      ident: true,
    });
  });
  const rng = mulberry32(0x49172);
  const cloud = 90;
  for (let i = 0; i < cloud; i++) {
    const y = rng() * 2 - 1;
    const phi = rng() * Math.PI * 2;
    const rr = Math.sqrt(Math.max(0, 1 - y * y));
    const r = 1.6 + rng() * 2.4;
    particles.push({
      pos: origin.clone().add(new THREE.Vector3(rr * Math.cos(phi) * r, y * 1.5, rr * Math.sin(phi) * r)),
      vel: new THREE.Vector3(rng() - 0.5, rng() - 0.5, rng() - 0.5).multiplyScalar(0.18),
      id: TRACEDECAY_PROJECT_ID,
      label: "cloud",
      ident: false,
    });
  }

  const soma = new THREE.Mesh(
    new THREE.SphereGeometry(0.28, 20, 16),
    new THREE.MeshBasicMaterial({ color: 0xb8e8ff, transparent: true, opacity: 0.85 }),
  );
  soma.position.copy(origin);
  soma.userData.projectId = TRACEDECAY_PROJECT_ID;
  world.add(soma);

  const max = particles.length;
  const pPos = new Float32Array(max * 3);
  const pGeo = new THREE.BufferGeometry();
  pGeo.setAttribute("position", new THREE.BufferAttribute(pPos, 3).setUsage(THREE.DynamicDrawUsage));
  const pMat = new THREE.PointsMaterial({
    color: 0xffffff,
    size: 0.07,
    transparent: true,
    blending: THREE.AdditiveBlending,
    depthWrite: false,
    sizeAttenuation: true,
  });
  const cloudPts = new THREE.Points(pGeo, pMat);
  world.add(cloudPts);

  const identMeshes: THREE.Mesh[] = [];
  const labels: { el: HTMLDivElement; pos: THREE.Vector3 }[] = [];
  const identMat = new THREE.MeshBasicMaterial({ color: 0xcfefff });
  for (const p of particles) {
    if (!p.ident) continue;
    const m = new THREE.Mesh(new THREE.SphereGeometry(0.08, 10, 8), identMat);
    m.position.copy(p.pos);
    m.userData.projectId = p.id;
    world.add(m);
    identMeshes.push(m);
    if (p.label !== "cloud" && (PROJECTS.some((x) => x.id === p.id) || p.label.length < 22)) {
      const el = document.createElement("div");
      el.className = "nl-label";
      el.innerHTML = `<div class="name">${p.label}</div>`;
      labelLayer.appendChild(el);
      labels.push({ el, pos: p.pos });
    }
  }

  const minDist = 1.15;
  const maxPairs = 1800;
  const linePos = new Float32Array(maxPairs * 2 * 3);
  const lineCol = new Float32Array(maxPairs * 2 * 3);
  const lineGeo = new THREE.BufferGeometry();
  lineGeo.setAttribute("position", new THREE.BufferAttribute(linePos, 3).setUsage(THREE.DynamicDrawUsage));
  lineGeo.setAttribute("color", new THREE.BufferAttribute(lineCol, 3).setUsage(THREE.DynamicDrawUsage));
  lineGeo.setDrawRange(0, 0);
  const lines = new THREE.LineSegments(
    lineGeo,
    new THREE.LineBasicMaterial({
      vertexColors: true,
      transparent: true,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    }),
  );
  world.add(lines);

  fit(camera, controls, world, host);
  const unbind = bindFocus(renderer, camera, [soma, ...identMeshes], inspect, hooks, (id) => PROJECTS.find((p) => p.id === id)?.name ?? id);
  const tmp = new THREE.Vector3();
  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches ?? false;
  if (reduced) controls.autoRotate = false;
  let raf = 0;
  const clock = new THREE.Clock();
  const bound = 4.6;
  const layout = () => {
    renderer.setSize(host.clientWidth || 1, host.clientHeight || 1, false);
    camera.aspect = (host.clientWidth || 1) / (host.clientHeight || 1);
    camera.updateProjectionMatrix();
  };
  layout();
  const ro = new ResizeObserver(layout);
  ro.observe(host);

  let drawN = 0;
  function frame() {
    const dt = Math.min(0.033, clock.getDelta());
    controls.update();
    if (!reduced) {
      for (const p of particles) {
        if (!p.ident) {
          p.pos.addScaledVector(p.vel, dt);
          for (const k of ["x", "y", "z"] as const) {
            const o = origin[k];
            if (p.pos[k] < o - bound || p.pos[k] > o + bound) p.vel[k] *= -1;
          }
        }
      }
    }
    for (let i = 0; i < particles.length; i++) {
      pPos[i * 3] = particles[i].pos.x;
      pPos[i * 3 + 1] = particles[i].pos.y;
      pPos[i * 3 + 2] = particles[i].pos.z;
    }
    pGeo.attributes.position.needsUpdate = true;

    let vertexpos = 0;
    let colorpos = 0;
    let numConnected = 0;
    const count = particles.length;
    for (let i = 0; i < count; i++) {
      for (let j = i + 1; j < count; j++) {
        const dx = particles[i].pos.x - particles[j].pos.x;
        const dy = particles[i].pos.y - particles[j].pos.y;
        const dz = particles[i].pos.z - particles[j].pos.z;
        const dist = Math.sqrt(dx * dx + dy * dy + dz * dz);
        if (dist < minDist && numConnected < maxPairs) {
          const alpha = 1 - dist / minDist;
          linePos[vertexpos++] = particles[i].pos.x;
          linePos[vertexpos++] = particles[i].pos.y;
          linePos[vertexpos++] = particles[i].pos.z;
          linePos[vertexpos++] = particles[j].pos.x;
          linePos[vertexpos++] = particles[j].pos.y;
          linePos[vertexpos++] = particles[j].pos.z;
          lineCol[colorpos++] = alpha * 0.55;
          lineCol[colorpos++] = alpha * 0.85;
          lineCol[colorpos++] = alpha;
          lineCol[colorpos++] = alpha * 0.55;
          lineCol[colorpos++] = alpha * 0.85;
          lineCol[colorpos++] = alpha;
          numConnected++;
        }
      }
    }
    const target = numConnected * 2;
    drawN = reduced ? target : Math.min(target, drawN + Math.max(4, Math.floor(target * 0.08)));
    if (drawN > target) drawN = target;
    lineGeo.setDrawRange(0, drawN);
    lineGeo.attributes.position.needsUpdate = true;
    lineGeo.attributes.color.needsUpdate = true;

    let im = 0;
    for (const p of particles) {
      if (!p.ident) continue;
      identMeshes[im].position.copy(p.pos);
      im++;
    }
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    let li = 0;
    for (const p of particles) {
      if (!p.ident) continue;
      if (li >= labels.length) break;
      if (labels[li] && (PROJECTS.some((x) => x.name === p.label) || p.label.includes("…") || TRACEDECAY_WORKTREES.includes(p.label))) {
        tmp.copy(p.pos).project(camera);
        labels[li].el.style.transform = `translate(${(tmp.x * 0.5 + 0.5) * w + 10}px, ${(-tmp.y * 0.5 + 0.5) * h - 8}px)`;
        li++;
      }
    }
    renderer.render(scene, camera);
    raf = requestAnimationFrame(frame);
  }
  frame();
  return {
    dispose() {
      cancelAnimationFrame(raf);
      ro.disconnect();
      unbind();
      controls.dispose();
      renderer.dispose();
      renderer.domElement.remove();
      labelLayer.remove();
      inspect.remove();
      disposeObject(scene);
    },
  };
}
