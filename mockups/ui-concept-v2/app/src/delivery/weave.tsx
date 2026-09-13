import { useWorkspaceState } from "../app/workspace";
/** State 04 — PR journey overview as a connected luminous lane weave (plate species). */
import { useState } from "react";
import { JOURNEY_PHASES, type JourneyLane, type LaneEvent } from "./data";
import { EpisodeDetails, hash32, mulberry } from "./field";

const W = 1200;
const H = 830;
const X0 = 152;
const X1 = 1178;
const SPINE_Y = 58;

type WavePt = { x: number; y: number };

/** Wandering lane path: mostly flat with plate-style level shifts. */
function wanderPath(seed: string, yc: number, xStart: number, xEnd: number, amp: number): WavePt[] {
  const rnd = mulberry(hash32(seed));
  const pts: WavePt[] = [];
  let level = 0;
  for (let x = xStart; x <= xEnd; x += 46 + rnd() * 26) {
    if (rnd() > 0.48) {
      level += (rnd() - 0.5) * amp * 1.9;
      level = Math.max(-amp, Math.min(amp, level));
    }
    pts.push({ x: Math.min(x, xEnd), y: yc + level });
  }
  if (pts[pts.length - 1].x < xEnd) pts.push({ x: xEnd, y: yc + level * 0.4 });
  return pts;
}

function smoothPath(pts: WavePt[]): string {
  if (pts.length < 2) return "";
  let d = `M ${pts[0].x.toFixed(1)} ${pts[0].y.toFixed(1)}`;
  for (let i = 1; i < pts.length; i++) {
    const p = pts[i - 1];
    const c = pts[i];
    const mx = (p.x + c.x) / 2;
    d += ` C ${mx.toFixed(1)} ${p.y.toFixed(1)}, ${mx.toFixed(1)} ${c.y.toFixed(1)}, ${c.x.toFixed(1)} ${c.y.toFixed(1)}`;
  }
  return d;
}

function yAt(pts: WavePt[], x: number): number {
  if (x <= pts[0].x) return pts[0].y;
  for (let i = 1; i < pts.length; i++) {
    if (x <= pts[i].x) {
      const a = pts[i - 1];
      const b = pts[i];
      const t = (x - a.x) / (b.x - a.x || 1);
      const s = t * t * (3 - 2 * t);
      return a.y + (b.y - a.y) * s;
    }
  }
  return pts[pts.length - 1].y;
}

type WeaveLane = {
  lane: JourneyLane;
  yc: number;
  xStart: number;
  xEnd: number;
  pts: WavePt[];
};

function tx(t: number): number {
  return X0 + (X1 - X0) * t;
}

function LaneWeaveRow(props: { wl: WeaveLane; seed: string; onSelect: (ev: LaneEvent, lane: string) => void }) {
  const { lane, pts } = props.wl;
  const rnd = mulberry(hash32(`beads-${props.seed}-${lane.id}`));
  const anchors = lane.events.filter((e) => e.kind !== "gap" && e.kind !== "ghost").map((e) => e.t);
  const centers = [...anchors];
  const extra = 5 + Math.floor(rnd() * 3);
  for (let i = 0; i < extra; i++) centers.push(0.1 + rnd() * 0.8);
  const beads: { x: number; y: number; r: number }[] = [];
  for (const c of centers) {
    const n = 4 + Math.floor(rnd() * 5);
    const cx = tx(c);
    for (let i = 0; i < n; i++) {
      const x = Math.max(props.wl.xStart + 4, Math.min(props.wl.xEnd - 4, cx + (i - (n - 1) / 2) * 10));
      beads.push({ x, y: yAt(pts, x), r: 2.1 + rnd() * 0.7 });
    }
  }
  const gaps = lane.events.filter((e) => e.kind === "gap" && e.end != null);
  const d = smoothPath(pts);
  return (
    <g>
      {[-4, 4].map((offset) => <path key={offset} d={d} transform={`translate(0 ${offset})`} fill="none" stroke={lane.color} strokeOpacity="0.16" strokeWidth="0.7" />)}
      <path d={d} fill="none" stroke={lane.color} strokeOpacity="0.22" strokeWidth="7" filter="url(#wvBloom)" />
      <path d={d} fill="none" stroke={lane.color} strokeOpacity="0.95" strokeWidth="1.3" filter="url(#wvSoft)" />
      {beads.map((b, i) => (
        <g key={i}>
          <circle cx={b.x} cy={b.y} r={b.r * 2} fill={lane.color} opacity="0.22" filter="url(#wvBloom)" />
          <circle cx={b.x} cy={b.y} r={b.r} fill={lane.color} opacity="0.95" />
        </g>
      ))}
      {anchors.map((t, i) => {
        const x = tx(t);
        const y = yAt(pts, x);
        return (
          <g key={`a${i}`} role="button" tabIndex={0} aria-label={`Inspect ${lane.label}: ${lane.events.filter((e) => e.kind !== "gap" && e.kind !== "ghost")[i].label}`}
            onClick={() => props.onSelect(lane.events.filter((e) => e.kind !== "gap" && e.kind !== "ghost")[i], lane.label)}
            onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onSelect(lane.events.filter((e) => e.kind !== "gap" && e.kind !== "ghost")[i], lane.label); } }}>
            <circle cx={x} cy={y} r={15} fill="transparent" />
            <circle cx={x} cy={y} r={8} fill={lane.color} opacity="0.28" filter="url(#wvBloom)" />
            <circle cx={x} cy={y} r={5.5} fill="#0a0f18" stroke={lane.color} strokeWidth="1.6" />
            <circle cx={x} cy={y} r={1.6} fill={lane.color} />
          </g>
        );
      })}
      {gaps.map((g) => {
        const x1 = tx(g.t);
        const x2 = tx(g.end!);
        const y = yAt(pts, (x1 + x2) / 2);
        return (
          <g key={g.label}>
            <rect x={x1} y={y - 6} width={x2 - x1} height={12} fill="url(#wvHatch)" stroke="#8b99a8" strokeOpacity="0.5" strokeDasharray="3 3" strokeWidth="0.8" />
            <text x={(x1 + x2) / 2} y={y + 22} textAnchor="middle" className="wv-gap">
              {g.label}
            </text>
          </g>
        );
      })}
    </g>
  );
}

function LaneLabel(props: { wl: WeaveLane }) {
  const { lane, yc } = props.wl;
  return (
    <g>
      <circle cx={22} cy={yc} r={9} fill="none" stroke={lane.color} strokeWidth="1.2" />
      <text x={22} y={yc + 3} textAnchor="middle" className="wv-glyph" fill={lane.color}>
        {lane.label.charAt(0).toUpperCase()}
      </text>
      <text x={40} y={yc - 1} className="wv-name">
        {lane.label}
      </text>
      <text x={40} y={yc + 13} className="wv-role">
        {lane.role}
      </text>
    </g>
  );
}

export function JourneyWeave(props: { lanes: JourneyLane[]; rails: JourneyLane[] }) {
  const [camera, setCamera] = useWorkspaceState("delivery.fixture.overview.camera", { x: 0, y: 0, zoom: 1 });
  const [selected, setSelected] = useState<{ ev: LaneEvent; lane: string } | null>(null);
  const laneTop = 130;
  const laneGap = 78;
  const lanes: WeaveLane[] = props.lanes.map((lane, i) => {
    const rnd = mulberry(hash32(`span-${lane.id}`));
    const ts = lane.events.map((e) => e.t);
    const xStart = tx(Math.max(0.005, Math.min(...ts) - 0.18 - rnd() * 0.12));
    const xEnd = tx(Math.min(0.995, Math.max(...ts) + 0.4 + rnd() * 0.3));
    const yc = laneTop + i * laneGap;
    return { lane, yc, xStart, xEnd, pts: wanderPath(`w-${lane.id}`, yc, xStart, xEnd, 18) };
  });
  const railTop = laneTop + props.lanes.length * laneGap + 32;
  const rails: WeaveLane[] = props.rails.map((lane, i) => {
    const yc = railTop + i * 56;
    return { lane, yc, xStart: tx(0.06), xEnd: tx(0.985), pts: wanderPath(`r-${lane.id}`, yc, tx(0.06), tx(0.985), 9) };
  });

  return (
    <div className="dl-weave">
      <svg viewBox={`${camera.x} ${camera.y} ${W / camera.zoom} ${H / camera.zoom}`} preserveAspectRatio="xMidYMid meet" role="group" aria-label="PR journey overview lane weave">
        <title>Connected journey lanes. Authored context lanes with selectable fixture episodes; cross-repo rails below.</title>
        <defs>
          <filter id="wvSoft" x="-80%" y="-80%" width="260%" height="260%">
            <feGaussianBlur stdDeviation="1.1" result="b" />
            <feMerge>
              <feMergeNode in="b" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
          <filter id="wvBloom" x="-120%" y="-120%" width="340%" height="340%">
            <feGaussianBlur stdDeviation="4" />
          </filter>
          {/* Horizontal lines have a zero-height bbox, which collapses percentage filter regions. */}
          <filter id="wvSpineBloom" filterUnits="userSpaceOnUse" x={0} y={SPINE_Y - 40} width={W} height={80}>
            <feGaussianBlur stdDeviation="4" />
          </filter>
          <filter id="wvSpineSoft" filterUnits="userSpaceOnUse" x={0} y={SPINE_Y - 20} width={W} height={40}>
            <feGaussianBlur stdDeviation="1" result="b" />
            <feMerge>
              <feMergeNode in="b" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
          <linearGradient id="wvSpine" gradientUnits="userSpaceOnUse" x1={X0} y1="0" x2={X1} y2="0">
            <stop offset="0%" stopColor="#38cfe8" />
            <stop offset="28%" stopColor="#f0b429" />
            <stop offset="62%" stopColor="#f0b429" />
            <stop offset="82%" stopColor="#c084fc" />
            <stop offset="100%" stopColor="#5ee7ff" />
          </linearGradient>
          <pattern id="wvHatch" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(-45)">
            <rect width="6" height="6" fill="transparent" />
            <rect width="2" height="6" fill="rgba(139,153,168,0.28)" />
          </pattern>
          <radialGradient id="wvDepth" cx="46%" cy="34%" r="72%">
            <stop offset="0%" stopColor="#0d1726" stopOpacity="0.9" />
            <stop offset="60%" stopColor="#070c14" stopOpacity="0.5" />
            <stop offset="100%" stopColor="#05080e" stopOpacity="0" />
          </radialGradient>
        </defs>
        <rect x="0" y="0" width={W} height={H} fill="url(#wvDepth)" />

        {JOURNEY_PHASES.map((p) => (
          <line key={p.label} x1={tx(p.t)} x2={tx(p.t)} y1={SPINE_Y + 42} y2={H - 8} stroke="#8caabe" strokeOpacity="0.12" strokeDasharray="2 6" />
        ))}

        <g>
          <circle cx={22} cy={SPINE_Y} r={9} fill="none" stroke="#5ee7ff" strokeWidth="1.2" />
          <text x={22} y={SPINE_Y + 3} textAnchor="middle" className="wv-glyph" fill="#5ee7ff">
            H
          </text>
          <text x={40} y={SPINE_Y - 1} className="wv-name">
            Human
          </text>
          <text x={40} y={SPINE_Y + 13} className="wv-role">
            author
          </text>
          <line x1={X0} x2={X1} y1={SPINE_Y} y2={SPINE_Y} stroke="url(#wvSpine)" strokeWidth="7" strokeOpacity="0.32" filter="url(#wvSpineBloom)" />
          <line x1={X0} x2={X1} y1={SPINE_Y} y2={SPINE_Y} stroke="url(#wvSpine)" strokeWidth="2.6" filter="url(#wvSpineSoft)" />
          {JOURNEY_PHASES.map((p, i) => {
            const x = tx(p.t);
            const c = i === 0 || i === JOURNEY_PHASES.length - 1 ? "#5ee7ff" : i >= 6 ? "#c084fc" : "#f0b429";
            return (
              <g key={p.label}>
                <text x={x} y={SPINE_Y - 26} textAnchor="middle" className="wv-phase">
                  {p.label}
                </text>
                <circle cx={x} cy={SPINE_Y} r={10} fill={c} opacity="0.35" filter="url(#wvBloom)" />
                <circle cx={x} cy={SPINE_Y} r={6} fill="#0a0f18" stroke={c} strokeWidth="2.4" filter="url(#wvSoft)" />
                <text x={x} y={SPINE_Y + 24} textAnchor="middle" className="wv-time">
                  {p.time}
                </text>
              </g>
            );
          })}
        </g>

        <text x={10} y={laneTop - 40} className="wv-sec">
          AGENTS · 7
        </text>
        <line x1={64} x2={X1} y1={laneTop - 44} y2={laneTop - 44} stroke="#8caabe" strokeOpacity="0.14" />

        <text x={X0} y={H - 8} className="wv-gap">Context strands · no handoff or completion inferred from lane order</text>
        {lanes.map((wl) => (
          <g key={wl.lane.id}>
            <LaneLabel wl={wl} />
            <LaneWeaveRow wl={wl} seed="agents" onSelect={(ev, lane) => setSelected({ ev, lane })} />
          </g>
        ))}

        <text x={10} y={railTop - 30} className="wv-sec">
          CROSS-REPO PR RAILS
        </text>
        <text x={186} y={railTop - 30} className="wv-chip">
          3 independent rails
        </text>
        <line x1={266} x2={X1} y1={railTop - 34} y2={railTop - 34} stroke="#8caabe" strokeOpacity="0.14" />
        {rails.map((wl) => (
          <g key={wl.lane.id}>
            <LaneLabel wl={wl} />
            <LaneWeaveRow wl={wl} seed="rails" onSelect={(ev, lane) => setSelected({ ev, lane })} />
          </g>
        ))}
      </svg>
      <div className="dl-weave-controls"><button aria-label="Pan overview left" onClick={() => setCamera({ ...camera, x: Math.max(0, camera.x - 100) })}>←</button><button aria-label="Pan overview right" onClick={() => setCamera({ ...camera, x: Math.min(W-W/camera.zoom, camera.x + 100) })}>→</button><button aria-label="Pan overview down" onClick={() => setCamera({ ...camera, y: Math.min(H-H/camera.zoom, camera.y + 80) })}>↓</button><button aria-label="Pan overview up" onClick={() => setCamera({ ...camera, y: Math.max(0,camera.y-80) })}>↑</button><button aria-label="Zoom overview in" onClick={() => setCamera({ ...camera, zoom: Math.min(4,camera.zoom+0.5) })}>+</button><button aria-label="Fit overview" onClick={() => setCamera({x:0,y:0,zoom:1})}>Fit</button></div>
      {selected ? <div role="dialog" aria-label="Selected journey episode" className="dl-evidence-popover dl-weave-detail"><button autoFocus onClick={() => setSelected(null)} aria-label="Close episode details">×</button><EpisodeDetails ev={selected.ev} lane={selected.lane} /></div> : null}
    </div>
  );
}
