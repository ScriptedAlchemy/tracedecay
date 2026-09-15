import { useEffect, useMemo, useRef, type MutableRefObject } from "react";
import { bakeBody, geodesicBall, hash, mulberry32, DEFAULT_BODY_APPEARANCE, type BodyAppearance, type BakedBody } from "./particles";
import { BRIGHTNESS, hitTest, layoutField, type FieldLayout } from "./layout";
import { layoutRepoField } from "./repoLayout";
import {
  buildScopedGraph,
  layoutScoped,
  type ScopedGraph,
  type ScopedLayout,
} from "./scopedGraph";
import {
  HUB,
  PROJECTS,
  SYNAPSE_EVENT,
  type BrainView,
  type ProjectBody,
} from "../data/fixtures";
import { HUB_ID } from "../concept/neuronLab/labUtil";
import { projectById } from "../concept/neuronLab/interior";

const bodyCache = new Map<string, BakedBody>();

function getBake(p: ProjectBody, appearance: BodyAppearance): BakedBody {
  const key = `${p.id}:${appearance.glow}:${appearance.dust}:${appearance.branch}`;
  const hit = bodyCache.get(key);
  if (hit) return hit;
  const baked = bakeBody({
    id: p.id,
    color: p.color,
    mass: p.indexedMass,
    brightness: BRIGHTNESS[p.recency],
    hook: p.id === SYNAPSE_EVENT.projectId,
  }, appearance);
  // Keep only the current sprite per identity while tuning.
  for (const cached of bodyCache.keys()) {
    if (cached.startsWith(`${p.id}:`)) bodyCache.delete(cached);
  }
  bodyCache.set(key, baked);
  return baked;
}

function starfield(ctx: CanvasRenderingContext2D, w: number, h: number) {
  ctx.save();
  ctx.globalCompositeOperation = "source-over";
  ctx.fillStyle = "#02070b";
  ctx.fillRect(0, 0, w, h);
  const g = ctx.createRadialGradient(w * 0.5, h * 0.38, 20, w * 0.5, h * 0.4, Math.max(w, h) * 0.72);
  g.addColorStop(0, "#040d14");
  g.addColorStop(1, "#02070b");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  let s = 7;
  const rand = () => {
    s = (s * 1103515245 + 12345) & 0x7fffffff;
    return (s % 10000) / 10000;
  };
  for (let i = 0; i < 320; i++) {
    const x = rand() * w;
    const y = rand() * h;
    const a = 0.05 + rand() * 0.24;
    ctx.globalAlpha = a;
    ctx.fillStyle = rand() < 0.12 ? "rgba(150,200,235,0.9)" : "rgba(175,205,228,0.8)";
    ctx.fillRect(x, y, rand() < 0.06 ? 1.6 : 1, 1);
  }
  ctx.restore();
}

function drawBody(ctx: CanvasRenderingContext2D, b: { x: number; y: number }, baked: BakedBody, alpha: number) {
  const dw = baked.canvas.width / 2;
  const dh = baked.canvas.height / 2;
  const dx = b.x - baked.ox / 2;
  const dy = b.y - baked.oy / 2;
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.globalCompositeOperation = "lighter";
  ctx.drawImage(baked.canvas, dx, dy, dw, dh);
  ctx.restore();
  return { dx, dy, dw, dh };
}

function drawRegistry(
  ctx: CanvasRenderingContext2D,
  field: FieldLayout,
  view: BrainView,
  hoverId: string | null,
  t: number,
  appearance: BodyAppearance,
) {
  const w = field.width;
  const h = field.height;
  starfield(ctx, w, h);

  // hub links: barely-there filaments; the registry is idle
  ctx.save();
  ctx.strokeStyle = "rgba(120,170,200,0.055)";
  ctx.lineWidth = 1;
  for (const b of field.bodies) {
    if (!b.project.linkedHub) continue;
    const by = b.y + b.capR * 0.9;
    ctx.beginPath();
    ctx.moveTo(b.x, by);
    ctx.quadraticCurveTo(
      b.x * 0.45 + field.hub.x * 0.55,
      (by + field.hub.y) / 2 + 14,
      field.hub.x,
      field.hub.y,
    );
    ctx.stroke();
  }
  ctx.restore();

  for (const b of field.bodies) {
    // hover inspects: unrelated bodies dim only enough to establish focus
    const dim = view !== "synapse" && hoverId && hoverId !== b.project.id ? 0.6 : 1;
    const baked = getBake(b.project, appearance);
    const d = drawBody(ctx, b, baked, dim);
    const raised =
      (view !== "synapse" && hoverId === b.project.id) ||
      (view === "synapse" && b.project.id === SYNAPSE_EVENT.projectId);
    if (raised) {
      ctx.save();
      ctx.globalCompositeOperation = "lighter";
      ctx.globalAlpha =
        view === "synapse" ? 0.42 * Math.max(0, 1 - t / 8) : 0.32;
      ctx.drawImage(baked.canvas, d.dx - 4, d.dy - 4, d.dw + 8, d.dh + 8);
      ctx.restore();
    }
  }

  // massless hub: a small identity dot, never a body
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  const hg = ctx.createRadialGradient(field.hub.x, field.hub.y, 0, field.hub.x, field.hub.y, 9);
  hg.addColorStop(0, "rgba(215,232,244,0.9)");
  hg.addColorStop(1, "rgba(215,232,244,0)");
  ctx.fillStyle = hg;
  ctx.beginPath();
  ctx.arc(field.hub.x, field.hub.y, 9, 0, Math.PI * 2);
  ctx.fill();
  ctx.fillStyle = "rgba(226,238,246,0.95)";
  ctx.beginPath();
  ctx.arc(field.hub.x, field.hub.y, 2.1, 0, Math.PI * 2);
  ctx.fill();
  ctx.restore();

  if (view === "synapse") {
    const src = field.bodies.find((b) => b.project.id === SYNAPSE_EVENT.projectId);
    if (src) drawSynapsePath(ctx, src, field.hub, t);
  }
}

/** Admitted activity only: dotted energy from the exact touched body to the hub. */
function drawSynapsePath(
  ctx: CanvasRenderingContext2D,
  src: { x: number; y: number; capR: number },
  hub: { x: number; y: number },
  t: number,
) {
  const x0 = src.x;
  const y0 = src.y + src.capR * 0.35;
  const cx = x0 + 24;
  const cy = (y0 + hub.y) / 2 + 48;
  const at = (u: number) => ({
    x: (1 - u) * (1 - u) * x0 + 2 * (1 - u) * u * cx + u * u * hub.x,
    y: (1 - u) * (1 - u) * y0 + 2 * (1 - u) * u * cy + u * u * hub.y,
  });
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  ctx.globalAlpha = Math.max(0, 1 - t / 8);
  ctx.strokeStyle = "rgba(73,213,244,0.75)";
  ctx.lineWidth = 0.85;
  ctx.setLineDash([4, 5]);
  ctx.beginPath(); ctx.moveTo(x0, y0); ctx.quadraticCurveTo(cx, cy, hub.x, hub.y); ctx.stroke();
  ctx.setLineDash([]);
  const pulseU = Math.min(1, 0.25 + t * 0.25);
  const pp = at(pulseU);
  ctx.fillStyle = "#9ffaff";
  ctx.shadowColor = "#5ee7ff";
  ctx.shadowBlur = 14;
  ctx.beginPath();
  ctx.arc(pp.x, pp.y, 2, 0, Math.PI * 2);
  ctx.fill();
  ctx.shadowBlur = 0;
  // evidenced hop energy, printed on the path
  const lp = at(0.42);
  ctx.font = "10px 'IBM Plex Mono', monospace";
  ctx.fillStyle = "rgba(158,236,255,0.92)";
  ctx.fillText(`${Math.round(SYNAPSE_EVENT.hopEnergy * 100)}%`, lp.x + 10, lp.y - 4);
  ctx.restore();
}

// ---------------------------------------------------------------------------
// repository neighborhood
// ---------------------------------------------------------------------------

const orbCache = new Map<string, HTMLCanvasElement>();

function getOrb(id: string, color: string, radius: number) {
  const key = `${id}@${color}@${Math.round(radius)}`;
  let hit = orbCache.get(key);
  if (!hit) {
    hit = geodesicBall(id, color, radius);
    orbCache.set(key, hit);
  }
  return hit;
}

const BG_ORBS: { fx: number; fy: number; r: number; color: string; id: string; blur: number; a: number }[] = [
  { fx: 0.06, fy: 0.16, r: 90, color: "#38bdf8", id: "bg-a", blur: 2.5, a: 0.3 },
  { fx: 0.93, fy: 0.12, r: 74, color: "#38bdf8", id: "bg-b", blur: 1.2, a: 0.26 },
  { fx: 0.1, fy: 0.85, r: 70, color: "#f0b429", id: "bg-c", blur: 2.5, a: 0.24 },
  { fx: 0.92, fy: 0.8, r: 96, color: "#f0b429", id: "bg-d", blur: 1.4, a: 0.26 },
  { fx: 0.55, fy: 0.06, r: 56, color: "#2dd4bf", id: "bg-e", blur: 1.2, a: 0.22 },
  { fx: 0.3, fy: 0.97, r: 64, color: "#38bdf8", id: "bg-f", blur: 1.6, a: 0.2 },
  { fx: 0.99, fy: 0.45, r: 58, color: "#2dd4bf", id: "bg-g", blur: 2.2, a: 0.2 },
  { fx: 0.02, fy: 0.5, r: 48, color: "#f0b429", id: "bg-h", blur: 2.8, a: 0.16 },
];

function drawRepo(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  project: ProjectBody,
  zoom: number,
  pan: { x: number; y: number },
) {
  starfield(ctx, w, h);

  // out-of-focus sibling constellations give the neighborhood depth
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  for (const bg of BG_ORBS) {
    const orb = getOrb(bg.id, bg.color, bg.r);
    ctx.filter = `blur(${bg.blur}px)`;
    ctx.globalAlpha = bg.a * 0.5;
    ctx.drawImage(orb, bg.fx * w - orb.width / 4, bg.fy * h - orb.height / 4, orb.width / 2, orb.height / 2);
  }
  ctx.restore();

  const field = layoutRepoField(w, h, project, zoom, pan);

  // exact registered relations: solid quiet lines hub -> checkout
  ctx.save();
  ctx.strokeStyle = "rgba(190,205,220,0.34)";
  ctx.lineWidth = 1.1;
  for (const orb of field.orbs) {
    const dx = orb.x - field.hub.x;
    const dy = orb.y - field.hub.y;
    const d = Math.hypot(dx, dy) || 1;
    const inner = 16;
    const outer = Math.max(0, d - orb.radius * 0.94);
    ctx.beginPath();
    ctx.moveTo(field.hub.x + (dx / d) * inner, field.hub.y + (dy / d) * inner);
    ctx.lineTo(field.hub.x + (dx / d) * outer, field.hub.y + (dy / d) * outer);
    ctx.stroke();
  }
  ctx.restore();

  for (const orb of field.orbs) {
    const baked = getOrb(orb.id, orb.color, Math.max(30, orb.radius));
    ctx.save();
    ctx.globalCompositeOperation = "lighter";
    ctx.drawImage(
      baked,
      orb.x - baked.width / 4,
      orb.y - baked.height / 4,
      baked.width / 2,
      baked.height / 2,
    );
    const towardHub = Math.atan2(field.hub.y - orb.y, field.hub.x - orb.x);
    const ex = orb.x + Math.cos(towardHub) * orb.radius * 0.94;
    const ey = orb.y + Math.sin(towardHub) * orb.radius * 0.94;
    const endpoint = ctx.createRadialGradient(ex, ey, 0, ex, ey, 12);
    endpoint.addColorStop(0, "#e4f7ff");
    endpoint.addColorStop(0.2, orb.color);
    endpoint.addColorStop(1, "transparent");
    ctx.fillStyle = endpoint;
    ctx.beginPath(); ctx.arc(ex, ey, 12, 0, Math.PI * 2); ctx.fill();
    ctx.restore();
  }

  // massless hub: matte identity orb + orbit ring, categorically not a project
  const hx = field.hub.x;
  const hy = field.hub.y;
  ctx.save();
  const sphere = ctx.createRadialGradient(hx - 4, hy - 5, 1, hx, hy, 13);
  sphere.addColorStop(0, "#c3ccd6");
  sphere.addColorStop(0.55, "#5d6873");
  sphere.addColorStop(1, "#1d242c");
  ctx.fillStyle = sphere;
  ctx.beginPath();
  ctx.arc(hx, hy, 12, 0, Math.PI * 2);
  ctx.fill();
  ctx.strokeStyle = "rgba(180,196,210,0.4)";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.arc(hx, hy, 24, 0, Math.PI * 2);
  ctx.stroke();
  ctx.fillStyle = "rgba(224,234,242,0.95)";
  ctx.font = "12px 'IBM Plex Mono', monospace";
  ctx.textAlign = "center";
  ctx.fillText("repo:<git_common_dir>", hx, hy + 46);
  ctx.textAlign = "start";
  ctx.restore();
}

// ---------------------------------------------------------------------------
// scoped: what the project knows
// ---------------------------------------------------------------------------

function mix(hex: string, toward: string, k: number) {
  const a = parseInt(hex.slice(1), 16);
  const b = parseInt(toward.slice(1), 16);
  const ch = (sh: number) =>
    Math.round(((a >> sh) & 255) * (1 - k) + ((b >> sh) & 255) * k);
  return `rgb(${ch(16)},${ch(8)},${ch(0)})`;
}

function drawScoped(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  graph: ScopedGraph,
  layout: ScopedLayout,
  hoverId: string | null,
) {
  starfield(ctx, w, h);
  // hover isolates: the hovered identity and its evidenced relations stay
  // lit while the rest recedes — inspection only, nothing fires or scopes
  const hover = hoverId && layout.byId.has(hoverId) ? hoverId : null;
  const near = new Set<string>();
  if (hover) {
    near.add(hover);
    for (const e of layout.edges) {
      if (e.a === hover) near.add(e.b);
      if (e.b === hover) near.add(e.a);
    }
  }
  const keepA = (id: string) => (!hover || near.has(id) ? 1 : 0.18);

  // ambient colored dust around each cluster
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  for (const n of layout.nodes) {
    if (!n.isHub) continue;
    const rng = mulberry32(hash(`${n.id}:dust`));
    for (let i = 0; i < 130; i++) {
      const ang = rng() * Math.PI * 2;
      const rad = Math.min(w, h) * (0.04 + Math.pow(rng(), 0.62) * 0.2);
      ctx.globalAlpha = 0.05 + rng() * 0.11;
      ctx.fillStyle = n.color;
      ctx.fillRect(n.x + Math.cos(ang) * rad * 1.35, n.y + Math.sin(ang) * rad, 1, 1);
    }
  }
  // faint neutral dust across the whole field
  {
    const rng = mulberry32(hash("scoped:fielddust"));
    for (let i = 0; i < 240; i++) {
      ctx.globalAlpha = 0.03 + rng() * 0.07;
      ctx.fillStyle = rng() < 0.5 ? "#7fb3d5" : "#8ea7bd";
      ctx.fillRect(rng() * w, rng() * h, 1, 1);
    }
  }
  ctx.restore();

  ctx.save();
  for (const e of layout.edges) {
    const a = layout.byId.get(e.a);
    const b = layout.byId.get(e.b);
    if (!a || !b) continue;
    const sameCluster = a.cluster === b.cluster;
    const incident = hover != null && (e.a === hover || e.b === hover);
    ctx.strokeStyle = sameCluster
      ? `${mix(a.color, "#0a0f18", 0.25)}`
      : "rgba(150,190,215,0.6)";
    const base = sameCluster ? 0.3 : 0.1;
    ctx.globalAlpha = !hover ? base : incident ? Math.min(0.8, base * 2.6) : base * 0.18;
    ctx.lineWidth = sameCluster ? 0.8 : 0.9;
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.lineTo(b.x, b.y);
    ctx.stroke();
  }
  ctx.restore();

  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  for (const n of layout.nodes) {
    const iso = keepA(n.id);
    if (n.dim) {
      // typed absence: a hollow, unlit identity — present, sealed, honest
      ctx.globalAlpha = 0.75 * iso;
      ctx.strokeStyle = "rgba(148,163,178,0.55)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.arc(n.x, n.y, Math.max(3.4, n.r), 0, Math.PI * 2);
      ctx.stroke();
      ctx.globalAlpha = 1;
      continue;
    }
    const g = ctx.createRadialGradient(n.x, n.y, 0, n.x, n.y, n.r * 3.4);
    g.addColorStop(0, `${mix(n.color, "#ffffff", 0.25)}`);
    g.addColorStop(0.35, n.color);
    g.addColorStop(1, "rgba(0,0,0,0)");
    ctx.globalAlpha = (n.isHub ? 0.5 : 0.34) * iso;
    ctx.fillStyle = g;
    ctx.beginPath();
    ctx.arc(n.x, n.y, n.r * 3.4, 0, Math.PI * 2);
    ctx.fill();
    ctx.globalAlpha = iso;
    const core = ctx.createRadialGradient(n.x - n.r * 0.3, n.y - n.r * 0.3, 0, n.x, n.y, n.r);
    core.addColorStop(0, mix(n.color, "#ffffff", 0.55));
    core.addColorStop(1, mix(n.color, "#07101a", 0.35));
    ctx.fillStyle = core;
    ctx.beginPath();
    ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
    ctx.fill();
    if (n.isHub) {
      ctx.strokeStyle = mix(n.color, "#ffffff", 0.6);
      ctx.lineWidth = 1.1;
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
  }
  // non-color focus cue on the inspected node
  if (hover) {
    const n = layout.byId.get(hover)!;
    ctx.strokeStyle = "rgba(226,238,246,0.85)";
    ctx.lineWidth = 1.4;
    ctx.beginPath();
    ctx.arc(n.x, n.y, Math.max(5, n.r + 4), 0, Math.PI * 2);
    ctx.stroke();
  }
  ctx.restore();

  ctx.save();
  for (const n of layout.nodes) {
    const hub = n.isHub;
    ctx.font = hub
      ? "600 11px 'IBM Plex Mono', monospace"
      : "9px 'IBM Plex Mono', monospace";
    ctx.globalAlpha = keepA(n.id);
    ctx.fillStyle = n.dim
      ? "rgba(148,163,178,0.8)"
      : hub
        ? mix(n.color, "#ffffff", 0.42)
        : mix(n.color, "#93a3b2", 0.42);
    if (n.labelSide === "left") {
      ctx.textAlign = "right";
      ctx.fillText(n.label, n.x - n.r - 6, n.y + 3);
      ctx.textAlign = "start";
    } else {
      ctx.fillText(n.label, n.x + n.r + 6, n.y + 3);
    }
  }
  ctx.restore();

  // in-scene captions
  ctx.save();
  ctx.font = "9.5px 'IBM Plex Mono', monospace";
  ctx.fillStyle = "rgba(140,155,170,0.85)";
  const nn = graph.nodes.length;
  const ne = graph.edges.length;
  const absences = graph.absences.length ? ` / ${graph.absences.join(" / ")}` : "";
  ctx.fillText(
    `identities: ${nn} · relations: ${ne} (profile)${absences} / size = connectedness / hover isolates`,
    18,
    h - 12,
  );
  ctx.restore();
}

export function BrainCanvas(props: {
  view: BrainView;
  hoverId: string | null;
  onHover: (id: string | null) => void;
  onPick: (id: string) => void;
  onCheckoutPick?: (id: string) => void;
  onField?: (f: FieldLayout) => void;
  fieldRef: MutableRefObject<FieldLayout | null>;
  projectId?: string;
  zoom?: number;
  appearance?: BodyAppearance;
  pan: { x: number; y: number };
  onPan: (pan: { x: number; y: number }) => void;
  onRepoZoom: (factor: number, anchor: { x: number; y: number }) => void;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  const activityStartedAt = useRef(performance.now());
  const drag = useRef<{ x: number; y: number; moved: boolean } | null>(null);
  const project = projectById(props.projectId ?? "") ?? PROJECTS[0];
  const zoom = props.zoom ?? 1;
  const scopedGraph = useMemo(
    () => (props.view === "scoped" ? buildScopedGraph(project) : null),
    [props.view, project],
  );
  // scoped hover is graph-local inspection state; it never leaves this canvas
  const scopedLayoutRef = useRef<ScopedLayout | null>(null);
  const scopedHoverRef = useRef<string | null>(null);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const parent = canvas.parentElement!;
    let raf = 0;
    let running = true;
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    const loop = (tms: number) => {
      if (!running) return;
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      const w = parent.clientWidth;
      const h = parent.clientHeight;
      if (canvas.width !== Math.floor(w * dpr) || canvas.height !== Math.floor(h * dpr)) {
        canvas.width = Math.floor(w * dpr);
        canvas.height = Math.floor(h * dpr);
        canvas.style.width = `${w}px`;
        canvas.style.height = `${h}px`;
      }
      const ctx = canvas.getContext("2d")!;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      const prev = props.fieldRef.current;
      const field = prev?.width === w && prev.height === h ? prev : layoutField(w, h);
      props.fieldRef.current = field;
      // keep DOM labels bound to the exact canvas layout
      if (!prev || prev.width !== field.width || prev.height !== field.height) {
        props.onField?.(field);
      }
      const t = reduced ? 0 : (tms - activityStartedAt.current) / 1000;
      if (props.view === "repo-zoom") drawRepo(ctx, w, h, project, zoom, props.pan);
      else if (props.view === "scoped" && scopedGraph) {
        const sl = layoutScoped(w, h - 106, scopedGraph);
        scopedLayoutRef.current = sl;
        ctx.save();
        ctx.translate(0, 106);
        drawScoped(ctx, w, h - 106, scopedGraph, sl, scopedHoverRef.current);
        ctx.restore();
      }
      else drawRegistry(ctx, field, props.view, props.hoverId, t, props.appearance ?? DEFAULT_BODY_APPEARANCE);
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => {
      running = false;
      cancelAnimationFrame(raf);
    };
  }, [props.view, props.hoverId, props.fieldRef, project, zoom, scopedGraph, props.appearance, props.pan]);

  return (
    <canvas
      ref={ref}
      tabIndex={props.view === "repo-zoom" ? 0 : undefined}
      aria-label={props.view === "repo-zoom" ? "Repository neighborhood. Drag to pan; scroll to zoom. Keyboard controls available above and below." : "Project evidence scene; use named controls or evidence table to inspect."}
      onWheel={(event) => {
        if (props.view !== "repo-zoom") return;
        const bounds = event.currentTarget.getBoundingClientRect();
        props.onRepoZoom(event.deltaY < 0 ? 1.1 : 1 / 1.1, { x: event.clientX - bounds.left, y: event.clientY - bounds.top });
      }}
      onPointerDown={(event) => {
        if (props.view !== "repo-zoom") return;
        event.currentTarget.setPointerCapture(event.pointerId);
        event.currentTarget.parentElement?.focus();
        drag.current = { x: event.clientX, y: event.clientY, moved: false };
      }}
      onPointerMove={(event) => {
        if (!drag.current || event.buttons !== 1) return;
        const dx = event.clientX - drag.current.x, dy = event.clientY - drag.current.y;
        if (Math.abs(dx) + Math.abs(dy) > 2) drag.current.moved = true;
        props.onPan({ x: props.pan.x + dx, y: props.pan.y + dy });
        drag.current.x = event.clientX; drag.current.y = event.clientY;
      }}
      onPointerCancel={() => { drag.current = null; }}
      onMouseMove={(e) => {
        if (props.view === "repo-zoom") return;
        const r = (e.target as HTMLCanvasElement).getBoundingClientRect();
        if (props.view === "scoped") {
          // hover isolates within the constellation — local inspection only,
          // never global project focus, never a synapse
          const sl = scopedLayoutRef.current;
          if (!sl) return;
          const mx = e.clientX - r.left;
          const my = e.clientY - r.top - 106;
          let best: { id: string; d: number } | null = null;
          for (const n of sl.nodes) {
            const d = Math.hypot(mx - n.x, my - n.y);
            if (d < Math.max(10, n.r + 7) && (!best || d < best.d)) best = { id: n.id, d };
          }
          scopedHoverRef.current = best?.id ?? null;
          return;
        }
        const field = props.fieldRef.current;
        if (!field) return;
        props.onHover(hitTest(field, e.clientX - r.left, e.clientY - r.top));
      }}
      onMouseLeave={() => {
        scopedHoverRef.current = null;
        props.onHover(null);
      }}
      onClick={(e) => {
        if (props.view === "scoped") return;
        if (props.view === "repo-zoom") {
          if (drag.current?.moved) { drag.current = null; return; }
          drag.current = null;
          const bounds = e.currentTarget.getBoundingClientRect();
          const field = layoutRepoField(bounds.width, bounds.height, project, zoom, props.pan);
          const hit = field.orbs.find((orb) => Math.hypot(e.clientX - bounds.left - orb.x, e.clientY - bounds.top - orb.y) <= orb.radius);
          if (hit) props.onCheckoutPick?.(hit.id);
          return;
        }
        const field = props.fieldRef.current;
        if (!field) return;
        const r = (e.target as HTMLCanvasElement).getBoundingClientRect();
        const id = hitTest(field, e.clientX - r.left, e.clientY - r.top);
        if (id && id !== HUB_ID) props.onPick(id);
      }}
    />
  );
}

export { HUB, PROJECTS };
