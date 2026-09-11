import { UMBRELLA_SAMPLE, UMBRELLA_SAMPLE_COUNT } from "./data";
import { useState } from "react";
/** Umbrella / inbox delivery constellations — luminous cluster meshes, beaded umbrella ring. */

function hash32(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function mulberry(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

type MeshSpec = {
  id: string;
  label?: string;
  sub?: string;
  color: string;
  x: number;
  y: number;
  rx: number;
  ry: number;
  n: number;
  ids?: string[];
  idsSide?: "left" | "right";
  idsBelow?: boolean;
  more?: string;
  dim?: boolean;
};

function meshNodes(spec: MeshSpec, spacious = false) {
  const rnd = mulberry(hash32(spec.id));
  const pts: { x: number; y: number; r: number }[] = [];
  for (let i = 0; i < spec.n; i++) {
    const a = spacious ? i * 2.3999632297 - Math.PI / 2 : rnd() * Math.PI * 2;
    const rr = spacious ? (i === 0 ? 0.72 : 0.35 + 0.58 * Math.sqrt(i / spec.n)) : 0.22 + 0.72 * Math.sqrt(rnd());
    pts.push({
      x: spec.x + Math.cos(a) * spec.rx * rr,
      y: spec.y + Math.sin(a) * spec.ry * rr,
      r: i === 0 ? 12 : i === 1 ? 7.5 : 3 + rnd() * 3,
    });
  }
  const edges: [number, number][] = [];
  for (let i = 1; i < pts.length; i++) {
    const order = pts
      .slice(0, i)
      .map((p, j) => ({ j, d: (p.x - pts[i].x) ** 2 + (p.y - pts[i].y) ** 2 }))
      .sort((a, b) => a.d - b.d);
    edges.push([i, order[0].j]);
    if (order[1] && rnd() > 0.35) edges.push([i, order[1].j]);
  }
  return { pts, edges };
}

function ClusterMesh(props: { spec: MeshSpec; faint?: boolean; delicate?: boolean }) {
  const { spec } = props;
  const { pts, edges } = meshNodes(spec, props.delicate);
  const op = spec.dim ? 0.42 : props.faint ? 0.6 : 1;
  return (
    <g opacity={op}>
      <ellipse cx={spec.x} cy={spec.y} rx={spec.rx * 1.45} ry={spec.ry * 1.45} fill={`url(#dlGlow-${spec.id})`} />
      <ellipse
        cx={spec.x}
        cy={spec.y}
        rx={spec.rx * 1.12}
        ry={spec.ry * 1.12}
        fill="none"
        stroke={spec.color}
        strokeOpacity="0.22"
        strokeWidth="0.9"
        strokeDasharray="1 5"
      />
      {edges.map(([a, b], i) => (
        <line
          key={i}
          x1={pts[a].x}
          y1={pts[a].y}
          x2={pts[b].x}
          y2={pts[b].y}
          stroke={spec.color}
          strokeOpacity={i % 3 === 2 ? 0.48 : 0.72}
          strokeWidth="1.1"
          strokeDasharray={i % 3 === 2 ? "2 3" : undefined}
        />
      ))}
      {pts.map((p, i) => (
        <g key={i}>
          <circle cx={p.x} cy={p.y} r={p.r * (props.delicate ? 2.1 : 2.4)} fill={spec.color} opacity={props.delicate ? 0.16 : i < 2 ? 0.3 : 0.16} filter="url(#dlBloom)" />
          <circle cx={p.x} cy={p.y} r={p.r} fill={props.delicate ? `url(#dlNode-${spec.id})` : spec.color} stroke={props.delicate ? spec.color : undefined} strokeWidth="1.2" opacity={props.delicate ? 0.92 : i < 2 ? 1 : 0.92} filter={props.delicate ? undefined : "url(#dlSoft)"} />
          {!props.delicate && i < 2 ? <circle cx={p.x} cy={p.y} r={p.r * 0.42} fill="#fff" opacity="0.8" /> : null}
        </g>
      ))}
      {spec.label ? (
        <>
          <text x={spec.x} y={spec.y - spec.ry - 22} textAnchor="middle" className="dl-svg-cluster" fill={spec.color}>
            {spec.label}
          </text>
          <text x={spec.x} y={spec.y - spec.ry - 6} textAnchor="middle" className="dl-svg-sub">
            {spec.sub}
          </text>
        </>
      ) : null}
      {spec.ids?.map((id, i) => {
        if (spec.idsBelow) {
          const ly = spec.y + spec.ry * 1.24 + 16 + i * 19;
          return (
            <text key={id} x={spec.x} y={ly} textAnchor="middle" className="dl-svg-id" fill={spec.color}>
              {id}
            </text>
          );
        }
        const right = spec.idsSide !== "left";
        const lx = right ? spec.x + spec.rx * 1.42 : spec.x - spec.rx * 1.42;
        const ly = spec.y - 10 + i * 22;
        return (
          <g key={id}>
            <line
              x1={right ? lx - 6 : lx + 6}
              y1={ly - 4}
              x2={right ? lx - 22 : lx + 22}
              y2={ly - 4}
              stroke={spec.color}
              strokeOpacity="0.4"
              strokeWidth="0.7"
            />
            <text x={lx} y={ly} textAnchor={right ? "start" : "end"} className="dl-svg-id" fill={spec.color}>
              {id}
            </text>
          </g>
        );
      })}
      {spec.more ? (
        <text
          x={spec.x}
          y={spec.y + spec.ry * (spec.idsBelow ? 1.24 : 1) + (spec.idsBelow ? 16 + (spec.ids?.length ?? 0) * 19 : 22)}
          textAnchor="middle"
          className="dl-svg-sub"
        >
          {spec.more}
        </text>
      ) : null}
    </g>
  );
}

function Dust(props: { seed: string; w: number; h: number; n: number }) {
  const rnd = mulberry(hash32(props.seed));
  const dots = Array.from({ length: props.n }, () => ({
    x: rnd() * props.w,
    y: rnd() * props.h,
    r: 0.4 + rnd() * 0.9,
    o: 0.06 + rnd() * 0.18,
  }));
  return (
    <g>
      {dots.map((d, i) => (
        <circle key={i} cx={d.x} cy={d.y} r={d.r} fill="#9db4c8" opacity={d.o} />
      ))}
    </g>
  );
}

function Defs(props: { meshes: MeshSpec[] }) {
  return (
    <defs>
      <filter id="dlSoft" x="-80%" y="-80%" width="260%" height="260%">
        <feGaussianBlur stdDeviation="1.4" result="b" />
        <feMerge>
          <feMergeNode in="b" />
          <feMergeNode in="SourceGraphic" />
        </feMerge>
      </filter>
      <filter id="dlBloom" x="-120%" y="-120%" width="340%" height="340%">
        <feGaussianBlur stdDeviation="4.5" />
      </filter>
      <radialGradient id="dlUmbrellaHalo" cx="50%" cy="50%" r="50%">
        <stop offset="0%" stopColor="#f0b429" stopOpacity="0.26" />
        <stop offset="55%" stopColor="#f0b429" stopOpacity="0.09" />
        <stop offset="100%" stopColor="#f0b429" stopOpacity="0" />
      </radialGradient>
      <radialGradient id="dlNightDepth" cx="48%" cy="36%" r="72%">
        <stop offset="0%" stopColor="#0e1a2c" stopOpacity="0.95" />
        <stop offset="60%" stopColor="#080d16" stopOpacity="0.5" />
        <stop offset="100%" stopColor="#05070d" stopOpacity="0" />
      </radialGradient>
      {props.meshes.map((m) => (
        <radialGradient key={`node-${m.id}`} id={`dlNode-${m.id}`} cx="35%" cy="30%" r="75%">
          <stop offset="0%" stopColor="#e3faff" stopOpacity="0.8" />
          <stop offset="40%" stopColor={m.color} stopOpacity="0.75" />
          <stop offset="100%" stopColor={m.color} stopOpacity="0.16" />
        </radialGradient>
      ))}
      {props.meshes.map((m) => (
        <radialGradient key={m.id} id={`dlGlow-${m.id}`} cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor={m.color} stopOpacity={m.dim ? 0.07 : 0.2} />
          <stop offset="60%" stopColor={m.color} stopOpacity={m.dim ? 0.03 : 0.07} />
          <stop offset="100%" stopColor={m.color} stopOpacity="0" />
        </radialGradient>
      ))}
    </defs>
  );
}

function BeadedRing(props: { cx: number; cy: number; rx: number; ry: number; beads: number; delicate?: boolean }) {
  const rnd = mulberry(hash32(`ring-${props.beads}`));
  const beads = Array.from({ length: props.beads }, (_, i) => {
    const a = (i / props.beads) * Math.PI * 2 + (rnd() - 0.5) * 0.035;
    const wobble = 1 + (rnd() - 0.5) * 0.05;
    return {
      x: props.cx + Math.cos(a) * props.rx * wobble,
      y: props.cy + Math.sin(a) * props.ry * wobble,
      r: props.delicate ? (i % 9 === 0 ? 2.1 : 0.65 + rnd() * 0.6) : i % 6 === 0 ? 3.4 + rnd() * 1.2 : 1.3 + rnd() * 1.3,
      a, length: 15 + rnd() * 60,
      o: i % 6 === 0 ? 1 : 0.6 + rnd() * 0.4,
    };
  });
  return (
    <g>
      {props.delicate ? <ellipse cx={props.cx} cy={props.cy} rx={props.rx - 1} ry={props.ry - 1} fill="#070d15" /> : null}
      <ellipse cx={props.cx} cy={props.cy} rx={props.rx * (props.delicate ? 1.65 : 2)} ry={props.ry * (props.delicate ? 1.65 : 2.05)} fill="url(#dlUmbrellaHalo)" opacity={props.delicate ? 0.32 : 1} />
      {props.delicate ? beads.map((b, i) => (
        <line key={`ray-${i}`} x1={b.x} y1={b.y}
          x2={b.x + Math.cos(b.a) * b.length} y2={b.y + Math.sin(b.a) * b.length}
          stroke="#ef9809" strokeWidth={i % 9 === 0 ? 0.8 : 0.5} strokeOpacity={i % 9 === 0 ? 0.75 : 0.4}
          strokeDasharray={i % 3 ? "0.6 3" : "1 2"} />
      )) : null}
      <ellipse
        cx={props.cx}
        cy={props.cy}
        rx={props.rx}
        ry={props.ry}
        fill="none"
        stroke="#f0b429"
        strokeOpacity="0.5"
        strokeWidth="1.4"
        strokeDasharray="2 4"
        filter="url(#dlSoft)"
      />
      <ellipse
        cx={props.cx}
        cy={props.cy}
        rx={props.rx}
        ry={props.ry}
        fill="none"
        stroke={props.delicate ? "#ffab18" : "#f8d37a"}
        strokeOpacity="0.5"
        strokeWidth={props.delicate ? 4 : 9}
        filter="url(#dlBloom)"
      />
      {beads.map((b, i) => (
        <g key={i}>
          <circle cx={b.x} cy={b.y} r={b.r * 2.6} fill={props.delicate ? "#ffaa18" : "#f8d37a"} opacity={b.o * (props.delicate ? 0.22 : 0.38)} filter="url(#dlBloom)" />
          <circle cx={b.x} cy={b.y} r={b.r} fill={props.delicate ? "#ffc145" : "#f8d37a"} opacity={b.o} filter="url(#dlSoft)" />
          {i % 6 === 0 ? <circle cx={b.x} cy={b.y} r={b.r * 0.4} fill="#fff" opacity="0.85" /> : null}
        </g>
      ))}
    </g>
  );
}

const UMBRELLA_STATS = [
  { y: -18, cls: "dl-svg-umbrella", text: "V2 code-intelligence release" },
  { y: 2, cls: "dl-svg-kind", text: "UMBRELLA OUTCOME" },
  { y: 30, cls: "dl-svg-stat", text: "12 projects · 48 PRs · 6 agents" },
  { y: 52, cls: "dl-svg-stat", text: "78,341 files changed · 1.84M LOC" },
  { y: 74, cls: "dl-svg-stat", text: "94% CI coverage · 87% evidence" },
  { y: 100, cls: "dl-svg-sub", text: "Updated 6h ago" },
];

function UmbrellaCore(props: { cx: number; cy: number; rx: number; ry: number; dense?: boolean; example?: boolean }) {
  return (
    <g>
      <BeadedRing cx={props.cx} cy={props.cy} rx={props.rx} ry={props.ry} beads={props.dense ? 288 : 138} delicate={props.dense} />
      {(props.example ? [{y:0,cls:"dl-svg-umbrella",text:"Example outcome"},{y:0,cls:"dl-svg-kind",text:"Synthetic V2 concept"},{y:0,cls:"dl-svg-stat",text:`${UMBRELLA_SAMPLE.length} repos · ${UMBRELLA_SAMPLE_COUNT} PRs`},{y:0,cls:"dl-svg-stat",text:"Inferred membership"},{y:0,cls:"dl-svg-stat",text:"Not a provider PR"}] : UMBRELLA_STATS).map((s, i) => (
        <text key={s.text} x={props.cx} y={props.cy + (props.example ? [-40,-12,15,42,68][i] : props.dense ? [-66, -40, -12, 12, 36, 70][i] : s.y - 24)} textAnchor="middle" className={s.cls} style={props.example ? {letterSpacing:0,fontSize:i===0?24:i===1?16:17} : undefined}>
          {s.text}
        </text>
      ))}
    </g>
  );
}

/** Amber tendrils fanning from the umbrella ring edge down to each cluster. */
function fanCurves(
  u: { cx: number; cy: number; rx: number; ry: number },
  targets: MeshSpec[],
  perCluster: number,
  seedTag: string,
) {
  const paths: { d: string; color: string; dash?: string; o: number; w: number; beads: { x: number; y: number }[] }[] = [];
  for (const t of targets) {
    const rnd = mulberry(hash32(seedTag + t.id));
    for (let k = 0; k < perCluster; k++) {
      const ang = Math.atan2(t.y - u.cy, t.x - u.cx) + (rnd() - 0.5) * 0.85;
      const sx = u.cx + Math.cos(ang) * u.rx;
      const sy = u.cy + Math.sin(ang) * u.ry;
      const ex = t.x + (rnd() - 0.5) * t.rx * 1.3;
      const ey = t.y - t.ry * (0.35 + rnd() * 0.6);
      const mx = (sx + ex) / 2 + (rnd() - 0.5) * 90;
      const my = (sy + ey) / 2 + (rnd() - 0.4) * 60;
      const beads: { x: number; y: number }[] = [];
      if (k % 2 === 0) {
        for (let b = 1; b <= 3; b++) {
          const tt = 0.2 + b * 0.22 + (rnd() - 0.5) * 0.06;
          const bx = (1 - tt) * (1 - tt) * sx + 2 * (1 - tt) * tt * mx + tt * tt * ex;
          const by = (1 - tt) * (1 - tt) * sy + 2 * (1 - tt) * tt * my + tt * tt * ey;
          beads.push({ x: bx, y: by });
        }
      }
      // These remain umbrella correlations: the lower routing is visual separation,
      // not a claim of dependency between repository pairs.
      const lower = seedTag === "umbrella" && k >= 4;
      const sweep = t.x < u.cx ? 1 : -1;
      paths.push({
        d: lower
          ? `M ${sx} ${sy} C ${u.cx + sweep * (180 + k * 18)} ${u.cy + 150}, ${t.x + sweep * (280 + k * 20)} ${950 + k * 9}, ${t.x} ${t.y + t.ry * 0.65}`
          : `M ${sx.toFixed(1)} ${sy.toFixed(1)} Q ${mx.toFixed(1)} ${my.toFixed(1)} ${ex.toFixed(1)} ${ey.toFixed(1)}`,
        color: lower ? t.color : "#f0b429",
        dash: lower ? "1 3" : k > 1 && rnd() > 0.6 ? "4 4" : undefined,
        o: lower ? 0.3 + rnd() * 0.15 : seedTag === "umbrella" ? 0.3 + rnd() * 0.3 : 0.38 + rnd() * 0.42,
        w: lower ? 0.75 : seedTag === "umbrella" ? (k === 0 ? 1.3 : 0.7) : k === 0 ? 1.9 : k === 1 ? 1.4 : 1,
        beads: lower ? [] : beads,
      });
    }
  }
  return paths;
}

/* State 01 — umbrella center, clusters orbiting (square field). */
const GLOBAL_MESHES: MeshSpec[] = [
  { id: "rspack", label: "Rspack", sub: "13 PRs", color: "#38cfe8", x: 95, y: 310, rx: 92, ry: 78, n: 13, ids: ["#18337", "#18315", "#18291"], idsSide: "right" },
  { id: "rspress", label: "Rspress", sub: "7 PRs", color: "#c084fc", x: 150, y: 540, rx: 72, ry: 58, n: 7, ids: ["#5628", "#5612"], idsSide: "left" },
  { id: "lynx", label: "Lynx", sub: "6 PRs", color: "#60a5fa", x: 352, y: 572, rx: 64, ry: 54, n: 6, ids: ["#3241", "#3228"], idsSide: "left" },
  { id: "rslib", label: "Rsbuild / Rslib", sub: "9 PRs", color: "#f0b429", x: 722, y: 440, rx: 78, ry: 66, n: 9, ids: ["#8187", "#8162", "#8139"], idsSide: "right" },
  { id: "mf", label: "Module Federation", sub: "5 PRs", color: "#9be15d", x: 692, y: 620, rx: 68, ry: 56, n: 5, ids: ["#2314", "#2299"], idsSide: "right" },
  { id: "td", label: "TraceDecay", sub: "8 PRs", color: "#5ee7ff", x: 466, y: 760, rx: 74, ry: 58, n: 8, ids: ["#707", "#694", "#681"], idsSide: "right" },
  { id: "tooling", label: "Tooling & Infra", sub: "6 PRs", color: "#8b99a8", x: 138, y: 865, rx: 62, ry: 48, n: 6, dim: true },
  { id: "unrelated", label: "Unrelated Activity", sub: "23 PRs", color: "#6b7784", x: 728, y: 870, rx: 86, ry: 54, n: 23, dim: true },
];

/* State 03 — umbrella at top, clusters ranked below. */
const DENSE_MESHES: MeshSpec[] = [
  { id: "rspack", label: "Rspack", sub: "13 PRs", color: "#38cfe8", x: 108, y: 448, rx: 69, ry: 78, n: 13, ids: ["#18337", "#18315", "#18291"], idsBelow: true, more: "+10 more" },
  { id: "rslib", label: "Rsbuild / Rslib", sub: "9 PRs", color: "#f0b429", x: 288, y: 420, rx: 61, ry: 73, n: 9, ids: ["#8187", "#8162", "#8139"], idsBelow: true },
  { id: "rspress", label: "Rspress", sub: "7 PRs", color: "#c084fc", x: 458, y: 448, rx: 62, ry: 69, n: 7, ids: ["#5628", "#5612"], idsBelow: true },
  { id: "lynx", label: "Lynx", sub: "6 PRs", color: "#60a5fa", x: 616, y: 420, rx: 60, ry: 68, n: 6, ids: ["#3241", "#3228"], idsBelow: true, more: "+4 more" },
  { id: "mf", label: "Module Federation", sub: "5 PRs", color: "#9be15d", x: 782, y: 448, rx: 60, ry: 68, n: 5, ids: ["#2314", "#2299"], idsBelow: true },
  { id: "td", label: "TraceDecay", sub: "8 PRs", color: "#5ee7ff", x: 452, y: 782, rx: 72, ry: 65, n: 8, ids: ["#707", "#694", "#681"], idsSide: "right", more: "+5 more" },
  { id: "tooling", label: "Tooling & Infra", sub: "6 PRs", color: "#8b99a8", x: 140, y: 826, rx: 73, ry: 57, n: 6, dim: true },
  { id: "unrelated", label: "Unrelated Activity", sub: "23 PRs", color: "#6b7784", x: 748, y: 830, rx: 85, ry: 56, n: 23, dim: true },
];

export const umbrellaRepositoryPosition = (id: string) => DENSE_MESHES.find((mesh) => mesh.id === id);
export function GlobalConstellation(props: { dense?: boolean; selectedRepository?: string | null; onSelectRepository?: (id: string | null) => void; camera?: {x:number;y:number;w:number;h:number}; onSelectPr?: (repo:string,id:string)=>void; onSelectMembership?: (repo:string)=>void }) {
  const u = props.dense
    ? { cx: 450, cy: 140, rx: 135, ry: 135 }
    : { cx: 410, cy: 185, rx: 170, ry: 170 };
  const meshes = props.dense ? DENSE_MESHES.filter((mesh) => UMBRELLA_SAMPLE.some((cluster) => cluster.id === mesh.id)).map((mesh) => {const cluster=UMBRELLA_SAMPLE.find((item) => item.id===mesh.id)!;return {...mesh,ids:cluster.prs.map((pr)=>pr.id),sub:`${cluster.prs.length} represented PRs`,more:undefined};}) : GLOBAL_MESHES;
  const colored = meshes.filter((m) => !m.dim);
  return (
    <div className={`dl-field${props.dense ? " is-umbrella" : ""}`} aria-label="Global delivery graph">
      <svg viewBox={props.camera ? `${props.camera.x} ${props.camera.y} ${props.camera.w} ${props.camera.h}` : props.dense ? "0 0 890 950" : "0 0 890 1100"} role={props.onSelectRepository ? "group" : "img"} preserveAspectRatio="xMidYMin meet">
        <title>Example outcome and repository envelopes. Named PR markers are represented fixture records; enclosures mean repository ownership; dashed links mean inferred example membership.</title>
        <Defs meshes={meshes} />
        <rect x="0" y="0" width={890} height={850} fill="url(#dlNightDepth)" />
        {!props.dense && <Dust seed="global-dust" w={890} h={850} n={210} />}
        {(!props.dense ? fanCurves(u, colored, 5, "global") : []).map((p, i) => (
          <g key={i}>
            {!props.dense ? <path d={p.d} fill="none" stroke={p.color} strokeOpacity={p.o * 0.3} strokeWidth={p.w * 3.4} filter="url(#dlBloom)" /> : null}
            <path d={p.d} fill="none" stroke={p.color} strokeOpacity={p.o} strokeWidth={p.w} strokeDasharray={p.dash} />
            {p.beads.map((b, j) => (
              <circle key={j} cx={b.x} cy={b.y} r={1.7} fill="#f8d37a" opacity={Math.min(1, p.o + 0.3)} filter="url(#dlSoft)" />
            ))}
          </g>
        ))}
        {props.dense && colored.filter((mesh)=>!props.selectedRepository || props.selectedRepository===mesh.id).map((mesh) => <g key={`membership-${mesh.id}`} role="button" tabIndex={0} aria-label={`Inspect inferred membership ${mesh.label}`} onClick={()=>props.onSelectMembership?.(mesh.id)} onKeyDown={(event)=>{if(event.key==="Enter" || event.key===" "){event.preventDefault();props.onSelectMembership?.(mesh.id);}}}><path d={`M${u.cx},${u.cy+u.ry} Q${mesh.x},${u.cy+u.ry+70} ${mesh.x},${mesh.y-mesh.ry}`} fill="none" stroke="#bd974e" strokeWidth="2" strokeDasharray="5 5"/><circle cx={mesh.x} cy={mesh.y-mesh.ry*1.3-35} r="10" fill="#1f211b" stroke="#d2b56d"/><title>Inferred example membership: authored outcome grouping; no recorded Work/session correlation</title></g>)}
        {meshes.filter((mesh)=>!props.dense || !props.selectedRepository || props.selectedRepository===mesh.id).map((m) => (
          <g key={m.id} role={props.onSelectRepository && !m.dim ? "button" : undefined}
            tabIndex={props.onSelectRepository && !m.dim ? 0 : undefined}
            aria-label={`Inspect ${m.label}`}
            aria-pressed={props.onSelectRepository && !m.dim ? props.selectedRepository === m.id : undefined}
            className={props.onSelectRepository && !m.dim ? "dl-graphpick" : undefined}
            onClick={() => { if (!m.dim) props.onSelectRepository?.(m.id); }}
            onKeyDown={(event) => { if (!m.dim && (event.key === "Enter" || event.key === " ")) { event.preventDefault(); props.onSelectRepository?.(m.id); } }}>
            <ellipse cx={m.x} cy={m.y} rx={m.rx * 1.4} ry={m.ry * 1.4} fill="transparent" />
            {props.dense ? <><ellipse cx={m.x} cy={m.y} rx={m.rx*1.3} ry={m.ry*1.3} fill={`url(#dlGlow-${m.id})`} stroke={m.color} strokeOpacity=".65"/><text x={m.x} y={m.y-m.ry*1.3-12} textAnchor="middle" fill={m.color} style={{fontSize:props.selectedRepository?16:30}}>{!props.selectedRepository && (m.label ?? m.id).includes(" ") ? (m.label ?? m.id).replace(" / "," ").split(" ").map((word,index)=><tspan key={index} x={m.x} dy={index?28:-22}>{word}</tspan>) : m.label}</text><text x={m.x} y={m.y+m.ry*1.3+24} textAnchor="middle" fill={m.color} style={{fontSize:props.selectedRepository?12:23}}>{m.sub}</text></> : <ClusterMesh spec={m} delicate faint={Boolean(props.selectedRepository && props.selectedRepository !== m.id)} />}
          </g>
        ))}
        {props.dense && props.onSelectPr && meshes.filter((mesh)=>!props.selectedRepository || props.selectedRepository===mesh.id).flatMap((mesh)=>UMBRELLA_SAMPLE.find((item)=>item.id===mesh.id)!.prs.map((pr,index)=>{
          const focus=props.selectedRepository===mesh.id;const count=UMBRELLA_SAMPLE.find((item)=>item.id===mesh.id)!.prs.length;const x=mesh.x;const y=mesh.y+(index-(count-1)/2)*38;
          return <g key={`${mesh.id}:${pr.id}`} role="button" tabIndex={0} aria-label={`Inspect fixture PR ${mesh.label} ${pr.id}`} onClick={()=>props.onSelectPr?.(mesh.id,pr.id)} onKeyDown={(event)=>{if(event.key==="Enter" || event.key===" "){event.preventDefault();props.onSelectPr?.(mesh.id,pr.id);}}}><rect x={x-(focus?84:55)} y={y-13} width={focus?168:110} height="28" rx="5" fill="#0b202c" stroke={mesh.color}/><text x={x} y={y+5} textAnchor="middle" fill="#dbeef6" style={{fontSize:focus?15:18,letterSpacing:0}}>{pr.id}{focus ? " · inspect →" : ""}</text><title>{pr.id} · select for fixture detail</title></g>;
        }))}
        <g role={props.onSelectRepository ? "button" : undefined} tabIndex={props.onSelectRepository ? 0 : undefined}
          aria-label="Inspect umbrella outcome" className={props.onSelectRepository ? "dl-graphpick" : undefined}
          onClick={() => props.onSelectRepository?.(null)}
          onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onSelectRepository?.(null); } }}>
          <ellipse cx={u.cx} cy={u.cy} rx={u.rx} ry={u.ry} fill="transparent" />
          <UmbrellaCore cx={u.cx} cy={u.cy} rx={u.rx} ry={u.ry} dense example={props.dense} />
        </g>
      </svg>
      {props.dense ? null : (
        <div className="dl-graphlegend">
          <div className="k">GRAPH LEGEND</div>
          <div>— Horizontal = Recent activity</div>
          <div>‖ Vertical = Review &amp; evidence</div>
          <div>◉ Node size = Changed surface</div>
          <div>☀ Brightness = Freshness</div>
          <div>~ Edge = Correlated outcome</div>
          <div className="k" style={{ marginTop: 6 }}>Colors = Repository</div>
          {colored.map((m) => (
            <div key={m.id}>
              <i style={{ background: m.color }} /> {m.label}
            </div>
          ))}
          <div>
            <i style={{ background: "#8b99a8" }} /> Other
          </div>
        </div>
      )}
      <div className="caption">
        {props.dense
          ? "Envelope = repository · named marker = sample PR · dashed link = inferred example membership. No internal edges imply dependencies."
          : "Select a repository or umbrella to open its outcome. Individual loaded PRs remain in the accessible list."}
      </div>
    </div>
  );
}

type Satellite = { id: string; title: string[]; x: number; y: number; r: number; color: string };

const SATELLITES: Satellite[] = [
  { id: "#5628", title: ["docs: improve", "build guide"], x: 262, y: 226, r: 58, color: "#c084fc" },
  { id: "#2328", title: ["scheduler", "fairness"], x: 610, y: 220, r: 52, color: "#5ee7ff" },
  { id: "#3241", title: ["perf: reduce", "bundle size"], x: 230, y: 420, r: 54, color: "#5ee7ff" },
  { id: "#2314", title: ["telemetry", "v2"], x: 648, y: 415, r: 48, color: "#5ee7ff" },
  { id: "#2134", title: ["feat: remote", "entry policy"], x: 330, y: 580, r: 52, color: "#5ee7ff" },
  { id: "#681", title: ["refactor: emit", "cache"], x: 528, y: 590, r: 48, color: "#5ee7ff" },
];

const PROJECT_REPOS: MeshSpec[] = [
  { id: "p-rspack", label: "Rspack", sub: "13 PRs", color: "#38cfe8", x: 700, y: 108, rx: 56, ry: 44, n: 13, ids: ["#18337", "#18315"], idsSide: "right" },
  { id: "p-rslib", label: "Rsbuild / Rslib", sub: "9 PRs", color: "#f0b429", x: 758, y: 330, rx: 48, ry: 42, n: 9, ids: ["#8187", "#8162", "#8139"], idsSide: "right" },
  { id: "p-rspress", label: "Rspress", sub: "7 PRs", color: "#c084fc", x: 118, y: 500, rx: 46, ry: 40, n: 7, ids: ["#5628", "#5612"], idsSide: "left" },
  { id: "p-lynx", label: "Lynx", sub: "6 PRs", color: "#60a5fa", x: 208, y: 712, rx: 48, ry: 38, n: 6, ids: ["#3241", "#3228"], idsSide: "right" },
  { id: "p-mf", label: "Module Federation", sub: "5 PRs", color: "#9be15d", x: 682, y: 690, rx: 50, ry: 40, n: 5, ids: ["#2314", "#2299"], idsSide: "right" },
];

export function ProjectConstellation() {
  const [selected, setSelected] = useState<Satellite | null>(null);
  const hub = { x: 442, y: 392 };
  return (
    <div className="dl-field" aria-label="Project delivery constellation">
      <svg viewBox="0 0 890 850" role="group" preserveAspectRatio="xMidYMin meet">
        <title>Project-scoped PR constellation. The selected project is the primary hub.</title>
        <Defs meshes={PROJECT_REPOS} />
        <defs>
          <radialGradient id="dlHubHalo" cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor="#5ee7ff" stopOpacity="0.2" />
            <stop offset="60%" stopColor="#5ee7ff" stopOpacity="0.06" />
            <stop offset="100%" stopColor="#5ee7ff" stopOpacity="0" />
          </radialGradient>
        </defs>
        <rect x="0" y="0" width={890} height={850} fill="url(#dlNightDepth)" />
        <Dust seed="project-dust" w={890} h={850} n={190} />
        {PROJECT_REPOS.map((m) => (
          <a key={m.id} href={`?data=fixture&surface=delivery&state=03&repository=${m.id.slice(2)}`} aria-label={`Open ${m.label} umbrella`}><ellipse cx={m.x} cy={m.y} rx={m.rx} ry={m.ry} fill={`url(#dlGlow-${m.id})`} stroke={m.color}/><text x={m.x} y={m.y} textAnchor="middle" fill={m.color}>{m.label}</text></a>
        ))}
        {SATELLITES.map((s) => (
          <g key={s.id} role="button" tabIndex={0} aria-label={`Inspect PR ${s.id}`} aria-pressed={selected?.id === s.id} onClick={() => setSelected(s)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); setSelected(s); } }}>
            <circle cx={s.x} cy={s.y} r={s.r * 1.4} fill="url(#dlHubHalo)" opacity="0.5" />
            <circle cx={s.x} cy={s.y} r={s.r} fill={s.color} fillOpacity="0.06" stroke={s.color} strokeOpacity="0.6" strokeWidth="1.1" strokeDasharray="2 4" />
            <circle cx={s.x} cy={s.y - s.r + 10} r={7.5} fill={s.color} opacity="0.35" filter="url(#dlBloom)" />
            <circle cx={s.x} cy={s.y - s.r + 10} r={5} fill={s.color} filter="url(#dlSoft)" />
            <text x={s.x} y={s.y - 8} textAnchor="middle" className="dl-svg-id" fill={s.color}>
              {s.id}
            </text>
            {s.title.map((line, i) => (
              <text key={line} x={s.x} y={s.y + 9 + i * 15} textAnchor="middle" className="dl-svg-sat">
                {line}
              </text>
            ))}
          </g>
        ))}
        <a href="?data=fixture&surface=delivery&state=04&pr=707" aria-label="Open PR707 journey example">
          <circle cx={hub.x} cy={hub.y} r={168} fill="url(#dlHubHalo)" />
          <circle cx={hub.x} cy={hub.y} r={108} fill="none" stroke="#5ee7ff" strokeOpacity="0.2" strokeWidth="1" strokeDasharray="1 6" />
          <circle cx={hub.x} cy={hub.y} r={68} fill="#5ee7ff" fillOpacity="0.06" stroke="#5ee7ff" strokeOpacity="0.4" strokeWidth="6" filter="url(#dlBloom)" />
          <circle cx={hub.x} cy={hub.y} r={68} fill="none" stroke="#5ee7ff" strokeOpacity="0.9" strokeWidth="1.6" filter="url(#dlSoft)" />
          <text x={hub.x} y={hub.y - 122} textAnchor="middle" className="dl-svg-hub">
            tracedecay
          </text>
          <text x={hub.x} y={hub.y - 100} textAnchor="middle" className="dl-svg-stat">
            {SATELLITES.length + 1} illustrated PRs
          </text>
          <text x={hub.x} y={hub.y - 8} textAnchor="middle" className="dl-svg-id" fill="#5ee7ff">
            #707
          </text>
          <text x={hub.x} y={hub.y + 10} textAnchor="middle" className="dl-svg-sat">
            feat: add ingest retry
          </text>
          <text x={hub.x} y={hub.y + 25} textAnchor="middle" className="dl-svg-sat">
            backoff
          </text>
        </a>
        <text x={356} y={116} className="dl-svg-annot">HIGH ACTIVITY</text>
        <text x={356} y={132} className="dl-svg-annot">HIGH EXPOSURE</text>
        <text x={396} y={796} className="dl-svg-annot">LOW ACTIVITY</text>
        <text x={396} y={812} className="dl-svg-annot">LOW EXPOSURE</text>
      </svg>
      {selected ? <div className="dl-weave-detail" role="dialog" aria-label="Project PR snapshot"><button className="dl-btn" autoFocus onClick={() => setSelected(null)}>Close PR snapshot</button><h4>{selected.id} · {selected.title.join(" ")}</h4><p>Authored design example. Detailed source evidence and the full journey are not attached to this PR snapshot.</p></div> : null}
      <div className="dl-graphlegend at-left">
        <div className="k">Authored layout:</div>
        <div>Position does not encode measured recency</div>
        <div>Size does not encode measured review exposure</div>
        <div>— Connections = authored visual grouping</div>
        <div className="ind">No recorded dependency or handoff source</div>
        <div className="ind"></div>
        <div className="ind"></div>
      </div>
      <div className="caption">
        Select a PR for its authored detail. #707 opens its separate journey example; repository links open the example umbrella.
      </div>
    </div>
  );
}
