import { useEffect, type ReactNode } from "react";
import { useDemo, useWorkspaceState, type AttentionItem } from "../app/workspace";
import { Corners } from "../app/shell/Corners";
import {
  ADOPTION_SURFACES,
  ANCHOR_TOTAL,
  BRANCHES,
  BUDGET_ROWS,
  CAPTURE_SHORT,
  DOCTOR_CHECKS,
  FACTS_ABSENT,
  FINDING_ROWS,
  fmt,
  HEADS_KNOWN,
  HOOKS,
  LIVE_STATS,
  PACK,
  PIPE_STAGES,
  RETRIEVAL_METRICS,
  STATE_LABEL,
  STORE_MANIFESTS,
  TELEMETRY_ROWS,
  TIMELINE_TICKS,
  type EvidenceState,
} from "./data";
import { OBSERVATORY_ATTENTION } from "./attention";
import { OBSERVATORY_TOPOLOGY, type ObservatoryTopologyNode } from "./topology";
import "./observatory.css";

const WELLS = ["doctor", "adoption", "retrieval", "pipeline", "hooks", "budgets", "topology", "live", "storage", "findings"] as const;
type Well = (typeof WELLS)[number];

const CAPTURE_FOOT = `AS OF ${CAPTURE_SHORT}`;

function Chip(props: { state: EvidenceState }) {
  return <span className={`ob-chip t-${props.state}`}>{STATE_LABEL[props.state]}</span>;
}

function Panel(props: {
  id: Well;
  sel: Well;
  onSel: (id: Well) => void;
  title: string;
  state: EvidenceState;
  chipLeft?: boolean;
  meta?: string;
  footer?: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className={`ob-panel ${props.sel === props.id ? "is-on" : ""} ${props.className ?? ""}`.trim()}
      aria-pressed={props.sel === props.id}
      aria-controls="ob-evidence"
      onKeyDown={(event) => {
        if (!["ArrowRight", "ArrowLeft", "ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
        const panels = Array.from(event.currentTarget.closest(".ob-main")?.querySelectorAll<HTMLButtonElement>(".ob-panel") ?? []);
        const index = panels.indexOf(event.currentTarget);
        const target = event.key === "Home" ? 0 : event.key === "End" ? panels.length - 1
          : (index + (event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1) + panels.length) % panels.length;
        event.preventDefault();
        panels[target]?.focus();
      }}
      onClick={() => props.onSel(props.id)}
    >
      <span className={`ob-panel-head ${props.chipLeft ? "chip-left" : ""}`.trim()}>
        <span className="ob-panel-title">{props.title}</span>
        <Chip state={props.state} />
        {props.meta && <span className="ob-head-meta">{props.meta}</span>}
      </span>
      <span className="ob-panel-body">{props.children}</span>
      {props.footer && <span className="ob-panel-foot">{props.footer}</span>}
    </button>
  );
}

/* ---- canonical observations timeline (thin tick rail) ---- */

const LANE_SLOTS = Array.from({ length: 40 }, (_, i) => 1 + (i / 39) * 98);

function TimelineStrip(props: { mode: "snapshot" | "fixture"; attention: AttentionItem[]; onAttention: (item: AttentionItem) => void }) {
  return (
    <div className="ob-time">
      <div className="ob-time-row">
        <span className="ob-kicker">CANONICAL OBSERVATIONS TIMELINE</span>
        <span className="ob-chip t-exact still">{props.mode === "fixture" ? "FIXTURE · AUTHORED" : `STILL · ${CAPTURE_SHORT}`}</span>
        <div className="ob-legend">
          {(["measured", "partial", "stale", "unavailable"] as const).map((t) => (
            <span key={t} className="ob-leg">
              <i className={`ob-dot t-${t}`} /> {t}
            </span>
          ))}
        </div>
      </div>
      <div className="ob-attention" aria-label="Attention with source-qualified evidence">
        <span className="ob-attention-label">ATTENTION</span>
        {props.attention.length === 0 ? (
          <span className="ob-attention-empty">no source-qualified items in this mode</span>
        ) : (
          props.attention.map((item) => (
            <button
              type="button"
              key={item.id}
              className={`ob-attention-item is-${item.severity}`}
              onClick={() => props.onAttention(item)}
              title={`${item.sourceRef} · ${item.evidence} evidence`}
            >
              <span>{item.title}</span>
              <b>{item.evidence}</b>
            </button>
          ))
        )}
      </div>
      <div className="ob-track">
        <i className="ob-track-line" />
        {LANE_SLOTS.map((pos) => (
          <i key={pos} className="ob-slot" style={{ left: `${pos}%` }} />
        ))}
        {TIMELINE_TICKS.map((t) => (
          <span key={t.label} className="ob-tick" style={{ left: `${t.pos}%` }}>
            <b>{t.label}</b>
          </span>
        ))}
        <i className="ob-snap" style={{ left: "100%" }} />
      </div>
    </div>
  );
}

/* ---- doctor inspection (unlit instrument — no run in pack) ---- */

const MATRIX_COLS = 9;
const MATRIX_ROWS = 8;
const MATRIX_PITCH = 12;

function DoctorCard(props: WellProps) {
  const w = MATRIX_COLS * MATRIX_PITCH;
  const h = MATRIX_ROWS * MATRIX_PITCH;
  return (
    <Panel
      {...props}
      id="doctor"
      title="DOCTOR INSPECTION"
      state="unavailable"
      meta="COVERAGE —"
      footer="LAST RUN — NONE IN SNAPSHOT"
    >
      <span className="ob-doctor">
        <span className="ob-doctor-list">
          {DOCTOR_CHECKS.map((c) => (
            <span key={c} className="ob-doctor-row">
              <span>{c}</span>
              <b>—</b>
            </span>
          ))}
        </span>
        <svg className="ob-matrix" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" aria-hidden="true">
          {Array.from({ length: MATRIX_ROWS }, (_, ri) => (
            <line key={`h${ri}`} x1={2} y1={ri * MATRIX_PITCH + 6} x2={w - 2} y2={ri * MATRIX_PITCH + 6} className="m-grid" />
          ))}
          {Array.from({ length: MATRIX_COLS }, (_, ci) => (
            <line key={`v${ci}`} x1={ci * MATRIX_PITCH + 6} y1={2} x2={ci * MATRIX_PITCH + 6} y2={h - 2} className="m-grid" />
          ))}
          {Array.from({ length: MATRIX_ROWS }, (_, ri) =>
            Array.from({ length: MATRIX_COLS }, (_, ci) => (
              <circle key={`${ri}-${ci}`} cx={ci * MATRIX_PITCH + 6} cy={ri * MATRIX_PITCH + 6} r={0.8} className="m-dot" />
            )),
          )}
        </svg>
      </span>
    </Panel>
  );
}

/* ---- adoption coverage (fill-bar instrument at zero) ---- */

function AdoptionCard(props: WellProps) {
  return (
    <Panel {...props} id="adoption" title="ADOPTION COVERAGE" state="unavailable" meta="COVERAGE —" footer={CAPTURE_FOOT}>
      <span className="ob-adopt-axis">
        <span>surface</span>
        <span>adoption</span>
      </span>
      <span className="ob-bars">
        {ADOPTION_SURFACES.map((s) => (
          <span key={s} className="ob-bar-row">
            <span className="ob-bar-lab">{s}</span>
            <span className="ob-bar-track" />
            <b className="ob-bar-val">—</b>
          </span>
        ))}
      </span>
      <span className="ob-bar-scale">
        <span>0%</span>
        <span>50%</span>
        <span>100%</span>
      </span>
    </Panel>
  );
}

/* ---- retrieval quality (named spark rows; measured-empty rails) ---- */

function RetrievalCard(props: WellProps) {
  return (
    <Panel
      {...props}
      id="retrieval"
      title="RETRIEVAL QUALITY"
      state="empty"
      meta={`ANCHORS ${ANCHOR_TOTAL}`}
      footer={CAPTURE_FOOT}
    >
      <span className="ob-metrics">
        {RETRIEVAL_METRICS.map((m) => (
          <span key={m} className="ob-metric-row">
            <span className="ob-metric-id">
              <span>{m}</span>
              <b>—</b>
            </span>
            <span className="ob-rail">
              <i className="top" />
              <i className="bot" />
              <em className="hi">1.0</em>
              <em className="lo">0</em>
            </span>
          </span>
        ))}
      </span>
    </Panel>
  );
}

/* ---- code-index pipeline: starfield + filament river + orbital ring ---- */

function mulberry32(seed: number) {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const FLOW_W = 1090;
const FLOW_H = 110;
const FLOW_MID = FLOW_H / 2;
const RING_X = FLOW_W * (5.5 / 7);
const SERVE_X = (FLOW_W * 6.5) / 7;
const STAGE5_XS = Array.from({ length: 5 }, (_, i) => (FLOW_W * (i + 0.5)) / 7);

/* a gently wandering centerline — small amplitude so no single stroke can be
 * traced left-to-right */
function centerY(x: number) {
  return FLOW_MID + 5.5 * Math.sin((x / FLOW_W) * Math.PI * 4.6 + 1.1) + 3.5 * Math.sin((x / FLOW_W) * Math.PI * 9.7);
}

/* the luminous body widens and brightens under each stage label (and around
 * the ring), narrowing to bright waists in between — a volume, not a wave */
function bloomAt(x: number) {
  let b = 0;
  for (const sx of STAGE5_XS) b = Math.max(b, 1 - Math.abs(x - sx) / 86);
  b = Math.max(b, 0.8 * (1 - Math.abs(x - RING_X) / 80));
  b = Math.max(b, 0.45 * (1 - Math.abs(x - SERVE_X) / 90));
  return Math.max(0, b);
}

function halfWidth(x: number) {
  return 3 + 30 * bloomAt(x);
}

/* filled river-volume outline from a local half-width function */
function volumePath(widthAt: (x: number) => number) {
  const step = 16;
  const top: string[] = [];
  const bot: string[] = [];
  for (let x = -14; x <= FLOW_W + 14; x += step) {
    const hw = widthAt(x);
    const cy = centerY(x);
    top.push(`${x.toFixed(0)} ${(cy - hw).toFixed(1)}`);
    bot.unshift(`${x.toFixed(0)} ${(cy + hw).toFixed(1)}`);
  }
  return `M ${top.join(" L ")} L ${bot.join(" L ")} Z`;
}

/* faint haze base only — the luminous interior is built from overlapping
 * filaments so nothing reads as continuous paint */
const VOLUME_LAYERS = [
  { d: volumePath((x) => Math.max(5, halfWidth(x))), cls: "v-outer", o: 0.035 },
  { d: volumePath((x) => Math.max(3, halfWidth(x) * 0.6)), cls: "v-mid", o: 0.025 },
];

/* the luminous body: dozens of filaments spanning the volume width — wide at
 * the blooms, converging at the waists; overlap makes the interior bright
 * while no single stroke is bright enough to trace */
type Strand = { d: string; o: number; w: number; kind: "fan" | "in" | "hot" };

const FLOW_STRANDS: Strand[] = (() => {
  const rnd = mulberry32(7);
  const strands: Strand[] = [];
  const make = (u: number, jitter: number) => {
    let x = -12;
    let y = centerY(0) + u * halfWidth(0);
    let d = `M ${x} ${y.toFixed(1)}`;
    while (x < FLOW_W + 12) {
      const nx = x + 26 + rnd() * 34;
      const ny = centerY(nx) + u * halfWidth(nx) * (0.8 + rnd() * 0.4) + (rnd() - 0.5) * jitter;
      d += ` C ${(x + (nx - x) / 3).toFixed(1)} ${y.toFixed(1)}, ${(x + (2 * (nx - x)) / 3).toFixed(1)} ${ny.toFixed(1)}, ${nx.toFixed(1)} ${ny.toFixed(1)}`;
      x = nx;
      y = ny;
    }
    return d;
  };
  /* bimodal spread: filaments ride the fan, not the centerline, so blooms
   * stay striated with a dim middle and only the waists bunch bright */
  for (let i = 0; i < 42; i++) {
    const side = i % 2 === 0 ? 1 : -1;
    const u = side * (0.35 + rnd() * 0.95);
    strands.push({ d: make(u, 4), o: 0.07 + rnd() * 0.16, w: 0.25 + rnd() * 0.35, kind: "fan" });
  }
  for (let i = 0; i < 18; i++) {
    const side = i % 2 === 0 ? 1 : -1;
    const u = side * (0.4 + rnd() * 0.45);
    strands.push({ d: make(u, 3), o: 0.12 + rnd() * 0.18, w: 0.3 + rnd() * 0.4, kind: "in" });
  }
  /* white-hot glints woven through both sides of the body — bimodal, so no
   * single centerline stroke exists */
  for (let i = 0; i < 4; i++) {
    const side = i % 2 === 0 ? 1 : -1;
    const u = side * (0.35 + rnd() * 0.3);
    strands.push({ d: make(u, 2.5), o: 0.24 + rnd() * 0.16, w: 0.4 + rnd() * 0.25, kind: "hot" });
  }
  return strands;
})();

/* bright narrow waists between the stage blooms */
const WAIST_XS = Array.from({ length: 5 }, (_, i) => (FLOW_W * (i + 1)) / 7);

/* star grit packed edge to edge across the whole well */
const STARS: { x: number; y: number; r: number; o: number }[] = (() => {
  const rnd = mulberry32(41);
  return Array.from({ length: 1700 }, () => ({
    x: rnd() * FLOW_W,
    y: rnd() * FLOW_H,
    r: 0.2 + rnd() * 0.65,
    o: 0.04 + rnd() * 0.3,
  }));
})();

/* soft nebula patches for cinematic depth */
const NEBULA: { x: number; y: number; r: number; o: number }[] = (() => {
  const rnd = mulberry32(97);
  return Array.from({ length: 16 }, () => ({
    x: rnd() * FLOW_W,
    y: rnd() * FLOW_H,
    r: 14 + rnd() * 32,
    o: 0.03 + rnd() * 0.05,
  }));
})();

/* particle dust inside the volume, densest at the blooms */
const FLOW_MOTES: { x: number; y: number; r: number; o: number; hot: boolean }[] = (() => {
  const rnd = mulberry32(23);
  return Array.from({ length: 760 }, (_, i) => {
    const x = rnd() * FLOW_W;
    const hw = halfWidth(x);
    const dy = (rnd() + rnd() - 1) * hw * 1.15;
    const closeness = 1 - Math.min(1, Math.abs(dy) / (hw + 2));
    return {
      x,
      y: centerY(x) + dy,
      r: 0.2 + rnd() * (0.3 + closeness * 0.4),
      o: 0.08 + rnd() * (0.28 + closeness * 0.6),
      hot: i % 6 === 0 && closeness > 0.55,
    };
  });
})();

/* dust collar threading through and around the ring annulus */
const RING_COLLAR: { x: number; y: number; r: number; o: number; hot: boolean }[] = (() => {
  const rnd = mulberry32(73);
  return Array.from({ length: 180 }, (_, i) => {
    const a = rnd() * Math.PI * 2;
    const rad = 0.72 + rnd() * 0.68;
    return {
      x: RING_X + Math.cos(a) * 38 * rad,
      y: FLOW_MID + Math.sin(a) * 42 * rad,
      r: 0.3 + rnd() * 1.05,
      o: 0.14 + rnd() * 0.6,
      hot: i % 5 === 0,
    };
  });
})();

function FlowField() {
  return (
    <svg className="ob-flow" viewBox={`0 0 ${FLOW_W} ${FLOW_H}`} preserveAspectRatio="none" aria-hidden="true">
      <defs>
        <filter id="ob-glow" x="-20%" y="-120%" width="140%" height="340%">
          <feGaussianBlur stdDeviation="2.4" />
        </filter>
        <filter id="ob-bloom" x="-20%" y="-200%" width="140%" height="500%">
          <feGaussianBlur stdDeviation="5.5" />
        </filter>
        <filter id="ob-fog" x="-20%" y="-100%" width="140%" height="300%">
          <feGaussianBlur stdDeviation="3.2" />
        </filter>
        <linearGradient id="ob-fade" x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stopColor="#fff" />
          <stop offset="0.78" stopColor="#fff" />
          <stop offset="0.87" stopColor="#999" />
          <stop offset="1" stopColor="#8a8a8a" />
        </linearGradient>
        <mask id="ob-river-fade">
          <rect x="0" y={-FLOW_H} width={FLOW_W} height={FLOW_H * 3} fill="url(#ob-fade)" />
        </mask>
      </defs>
      <g filter="url(#ob-bloom)" className="ob-nebula">
        {NEBULA.map((n, i) => (
          <circle key={i} cx={n.x.toFixed(1)} cy={n.y.toFixed(1)} r={n.r.toFixed(1)} opacity={n.o.toFixed(3)} />
        ))}
        <circle cx={RING_X.toFixed(1)} cy={FLOW_MID} r={52} opacity={0.08} />
      </g>
      <g className="ob-stars">
        {STARS.map((s, i) => (
          <circle key={i} cx={s.x.toFixed(1)} cy={s.y.toFixed(1)} r={s.r.toFixed(2)} opacity={s.o.toFixed(2)} />
        ))}
      </g>
      <g mask="url(#ob-river-fade)">
        <g filter="url(#ob-fog)">
          {VOLUME_LAYERS.map((v) => (
            <path key={v.cls} d={v.d} className={v.cls} opacity={v.o} />
          ))}
        </g>
        <g filter="url(#ob-bloom)" className="ob-flow-halo">
          {FLOW_STRANDS.filter((s) => s.kind === "hot").map((s, i) => (
            <path key={`b${i}`} d={s.d} opacity={(s.o * 0.25).toFixed(2)} strokeWidth={s.w * 3} className="halo" />
          ))}
          {WAIST_XS.map((wx, i) => (
            <circle key={`wg${i}`} cx={wx.toFixed(1)} cy={centerY(wx).toFixed(1)} r={8} opacity={0.62} className="knot" />
          ))}
        </g>
        <g filter="url(#ob-glow)" className="ob-flow-halo">
          {FLOW_STRANDS.filter((s) => s.kind !== "fan").map((s, i) => (
            <path key={`h${i}`} d={s.d} opacity={(s.o * 0.22).toFixed(2)} strokeWidth={s.w * 1.8} className="halo" />
          ))}
          {WAIST_XS.map((wx, i) => (
            <circle key={`wc${i}`} cx={wx.toFixed(1)} cy={centerY(wx).toFixed(1)} r={2.8} opacity={0.95} className="knot" />
          ))}
        </g>
        {FLOW_STRANDS.map((s, i) => (
          <path key={i} d={s.d} opacity={s.o.toFixed(2)} strokeWidth={s.w} className={s.kind} />
        ))}
        {[...FLOW_MOTES, ...RING_COLLAR].map((m, i) => (
          <circle
            key={`m${i}`}
            cx={m.x.toFixed(1)}
            cy={m.y.toFixed(1)}
            r={m.r.toFixed(2)}
            opacity={m.o.toFixed(2)}
            className={m.hot ? "mote hot" : "mote"}
          />
        ))}
      </g>
    </svg>
  );
}

function PipelineCard(props: WellProps) {
  return (
    <Panel {...props} id="pipeline" title="CODE-INDEX PIPELINE" state="unsealed" chipLeft className="ob-pipe" footer={CAPTURE_FOOT}>
      <span className="ob-pipe-flow">
        <FlowField />
        <span className="ob-stages">
          {PIPE_STAGES.map((s) =>
            s.id === "index" ? (
              <span key={s.id} className="ob-stage">
                <span className="ob-ring">
                  <span className="ob-stage-name">{s.id}</span>
                  <b className="ob-stage-val">—</b>
                  <span className="ob-ring-sub">unsealed</span>
                </span>
              </span>
            ) : (
              <span key={s.id} className={`ob-stage ${s.state === "unavailable" ? "is-dim" : ""}`.trim()}>
                <span className="ob-stage-name">{s.id}</span>
                <b className="ob-stage-val">—</b>
                <span className="ob-stage-unit">{s.unit}</span>
              </span>
            ),
          )}
        </span>
        <span className="ob-pipe-arrow" aria-hidden="true">
          ⟶
        </span>
      </span>
      <span className="ob-stage-states">
        {PIPE_STAGES.map((s) => (
          <span key={s.id} className={`ob-stage-state t-${s.state}`}>
            <span>{STATE_LABEL[s.state]}</span>
            <b>—</b>
          </span>
        ))}
      </span>
    </Panel>
  );
}

/* ---- hook census (two stacked count lists) ---- */

function HooksCard(props: WellProps) {
  return (
    <Panel
      {...props}
      id="hooks"
      title="HOOK HINTS / CENSUS"
      state="host"
      meta={`LINES ${fmt(HOOKS.lines)}`}
      footer={CAPTURE_FOOT}
    >
      <span className="ob-hooks">
        <span className="ob-hooks-lab cyan">
          <i /> hook events — host file
        </span>
        {HOOKS.events.map((r) => (
          <span key={r.name} className="ob-hooks-row">
            <span>{r.name}</span>
            <b>{fmt(r.count)}</b>
          </span>
        ))}
        <span className="ob-hooks-lab amber">
          <i /> completed dispositions
        </span>
        {HOOKS.dispositionStatus.slice(0, 3).map((r) => (
          <span key={r.name} className="ob-hooks-row">
            <span>{r.name}</span>
            <b>{fmt(r.count)}</b>
          </span>
        ))}
      </span>
    </Panel>
  );
}

/* ---- performance budgets (dense five-column ledger, typed em-dash cells) ---- */

function BudgetsCard(props: WellProps) {
  return (
    <Panel
      {...props}
      id="budgets"
      title="PERFORMANCE BUDGETS / COMPARISONS"
      state="unavailable"
      meta="COVERAGE —"
      footer={CAPTURE_FOOT}
    >
      <span className="ob-budget">
        <span className="ob-budget-row head">
          <span className="b2">budget</span>
          <span>current</span>
          <span>vs budget</span>
          <span>vs 7d p50</span>
        </span>
        {BUDGET_ROWS.map((b) => (
          <span key={b} className="ob-budget-row">
            <span>{b}</span>
            <b>—</b>
            <b>—</b>
            <b>—</b>
            <b>—</b>
          </span>
        ))}
      </span>
    </Panel>
  );
}

/* ---- dependency topology (fixed spatial authority map) ---- */

function topologyNode(id: string): ObservatoryTopologyNode {
  return OBSERVATORY_TOPOLOGY.nodes.find((node) => node.id === id) ?? OBSERVATORY_TOPOLOGY.nodes[0];
}

function topologyFocusFromKey(current: ObservatoryTopologyNode, key: string): ObservatoryTopologyNode | null {
  if (key === "Home") return OBSERVATORY_TOPOLOGY.nodes[0];
  if (key === "End") return OBSERVATORY_TOPOLOGY.nodes.at(-1) ?? null;
  const direction = key === "ArrowLeft" ? [-1, 0] : key === "ArrowRight" ? [1, 0] : key === "ArrowUp" ? [0, -1] : key === "ArrowDown" ? [0, 1] : null;
  if (!direction) return null;
  const candidates = OBSERVATORY_TOPOLOGY.nodes.filter((node) => {
    const dx = node.x - current.x;
    const dy = node.y - current.y;
    return direction[0] ? Math.sign(dx) === direction[0] : Math.sign(dy) === direction[1];
  });
  return candidates.sort((a, b) => {
    const score = (node: ObservatoryTopologyNode) => {
      const dx = Math.abs(node.x - current.x);
      const dy = Math.abs(node.y - current.y);
      return direction[0] ? dx + dy * 1.7 : dy + dx * 1.7;
    };
    return score(a) - score(b);
  })[0] ?? null;
}

function TopologyCard(props: WellProps & { focused: string; mode: "snapshot" | "fixture"; onFocus: (id: string) => void }) {
  const select = (node: ObservatoryTopologyNode) => {
    props.onFocus(node.id);
    props.onSel(node.well as Well);
  };
  return (
    <section className="ob-panel ob-topology" aria-label="Dependency topology">
      <span className="ob-panel-head">
        <span className="ob-panel-title">DEPENDENCY TOPOLOGY</span>
        <Chip state="partial" />
        <span className="ob-head-meta">{props.mode === "fixture" ? "FIXTURE · AUTHORED RELATIONS" : "SNAPSHOT · 2 DECLARED EDGES"}</span>
      </span>
      <span className="ob-topology-legend">
        <span><i className="role-source" /> source authority</span>
        <span><i className="role-affected" /> affected consumer</span>
        <span>solid data · dashed comparison · no control claims</span>
      </span>
      <span className="ob-topo">
        <svg viewBox="0 0 360 158" preserveAspectRatio="xMidYMid meet" aria-label="Stable source-to-consumer dependency topology">
          <defs>
            <marker id="ob-topo-arrow" markerWidth="5" markerHeight="5" refX="4.2" refY="2.5" orient="auto">
              <path d="M0,0 L5,2.5 L0,5 Z" className="topo-arrow" />
            </marker>
          </defs>
          <g className="topo-edges">
            {OBSERVATORY_TOPOLOGY.edges.map((edge) => {
              const from = topologyNode(edge.from);
              const to = topologyNode(edge.to);
              return <path key={edge.id} className={`is-${edge.kind}`} d={`M ${from.x} ${from.y} L ${to.x} ${to.y}`} markerEnd="url(#ob-topo-arrow)" />;
            })}
          </g>
          <g className="topo-nodes">
            {OBSERVATORY_TOPOLOGY.nodes.map((node) => (
              <g
                key={node.id}
                className={`topo-node t-${node.state} role-${node.role}${props.focused === node.id ? " is-focused" : ""}`}
                transform={`translate(${node.x} ${node.y})`}
                role="button"
                tabIndex={props.focused === node.id ? 0 : -1}
                aria-label={props.mode === "fixture"
                  ? `${node.label}: authored ${node.role} relation; no snapshot provenance implied.`
                  : `${node.label}: ${STATE_LABEL[node.state].toLowerCase()}; ${node.role}; ${node.summary}`}
                data-topology-node={node.id}
                onFocus={() => props.onFocus(node.id)}
                onClick={() => select(node)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    select(node);
                    return;
                  }
                  const next = topologyFocusFromKey(node, event.key);
                  if (!next) return;
                  event.preventDefault();
                  props.onFocus(next.id);
                  event.currentTarget.ownerSVGElement?.querySelector<SVGGElement>(`[data-topology-node="${next.id}"]`)?.focus();
                }}
              >
                <circle r={node.role === "derived" ? 19 : 13} />
                {node.role === "source" && <rect x="-3" y="-3" width="6" height="6" />}
                {node.role === "affected" && <path d="M-5 0h10M0-5v10" />}
                <text y={node.role === "derived" ? 31 : 25} textAnchor="middle">{node.label}</text>
                <text y={node.role === "derived" ? 41 : 35} textAnchor="middle" className="topo-state">
                  {node.role === "source" ? "SOURCE" : node.role === "affected" ? "AFFECTED" : node.role === "derived" ? "DERIVED" : "INDEPENDENT"}
                </text>
              </g>
            ))}
          </g>
        </svg>
      </span>
      <span className="ob-panel-foot">
        {props.mode === "fixture"
          ? "AUTHORED EXAMPLE · topology shape only; no snapshot provenance implied"
          : "fixed layout · choose a node to inspect source evidence"}
      </span>
    </section>
  );
}

/* ---- live pipeline (stat rows + empty spark rail) ---- */

function LiveCard(props: WellProps) {
  return (
    <Panel
      {...props}
      id="live"
      title="LIVE CODE-INDEX PIPELINE"
      state="unavailable"
      meta="COVERAGE —"
      footer={CAPTURE_FOOT}
    >
      <span className="ob-live">
        <span className="ob-live-stats">
          {LIVE_STATS.map((s) => (
            <span key={s} className="ob-live-row">
              <span>{s}</span>
              <b>—</b>
            </span>
          ))}
        </span>
        <span className="ob-live-spark">
          <span className="ob-live-y">
            <span>—</span>
            <span>—</span>
            <span>0</span>
          </span>
          <span className="ob-live-plot">
            <span className="ob-rail tall">
              <i className="top" />
              <i className="mid" />
              <i className="bot" />
            </span>
            <span className="ob-spark-x">
              <span>-60m</span>
              <span>-30m</span>
              <span>now</span>
            </span>
          </span>
        </span>
      </span>
    </Panel>
  );
}

/* ---- storage telemetry (thick tier bars at zero; identity only) ---- */

function TelemetryCard(props: WellProps) {
  return (
    <Panel {...props} id="storage" title="STORAGE TELEMETRY" state="exact" meta="BYTES —" footer={CAPTURE_FOOT}>
      <span className="ob-telem">
        {TELEMETRY_ROWS.map((t, i) => (
          <span key={t} className="ob-telem-row">
            <span className="lab">{t}</span>
            <b className="num">—</b>
            {i < 3 ? <span className="ob-bar-track thick" /> : <span className="ob-bar-none" />}
            <b className="pct">—</b>
          </span>
        ))}
        <span className="ob-micro">
          {STORE_MANIFESTS.length} store_manifest.json exact · kind code_project · bytes not copied
        </span>
      </span>
    </Panel>
  );
}

/* ---- storage / branch findings (tagged rows; selected census row) ---- */

function FindingsCard(props: WellProps) {
  return (
    <Panel
      {...props}
      id="findings"
      title="STORAGE / BRANCH FINDINGS"
      state="measured"
      meta={`REFS ${fmt(BRANCHES.localRefCount)}`}
      footer={CAPTURE_FOOT}
    >
      <span className="ob-findings">
        {FINDING_ROWS.map((f) => (
          <span key={f.id} className={`ob-finding ${f.selected ? "is-sel" : ""} t-${f.state}`.trim()}>
            <span className="ob-finding-tag">{f.state === "measured" ? "MEAS" : "UNAV"}</span>
            <span className="ob-finding-lab">{f.label}</span>
            <b>{f.state === "measured" ? fmt(Number(f.value)) : f.value}</b>
            <i>{f.selected ? "→" : ""}</i>
          </span>
        ))}
      </span>
    </Panel>
  );
}

/* ---- evidence inspector (full-height plate hierarchy) ---- */

type InspectorBody = {
  title: string;
  state: EvidenceState;
  source: string;
  scope: string;
  affected: string;
  details: string;
  notes: string;
};

function inspectBody(well: Well): InspectorBody {
  switch (well) {
    case "doctor":
      return {
        title: "Doctor inspection",
        state: "unavailable",
        source: "Doctor",
        scope: `${DOCTOR_CHECKS.length} check families`,
        affected: "—",
        details: "No inspection run was copied into this pack. The dot matrix stays unlit; not a 71% partial score.",
        notes: "Check families are the instrument, not findings.",
      };
    case "adoption":
      return {
        title: "Adoption coverage",
        state: "unavailable",
        source: "surface adoption",
        scope: `${ADOPTION_SURFACES.length} surfaces`,
        affected: "—",
        details: "Adoption metrics were not copied into this pack. Bar tracks stay unfilled.",
        notes: "The surface axis is drawn; the series is typed absent.",
      };
    case "retrieval":
      return {
        title: "Retrieval quality",
        state: "empty",
        source: "retrieval_anchors",
        scope: `${PACK.projects.length} projects`,
        affected: `${ANCHOR_TOTAL} anchors`,
        details: "Zero anchors is a measured empty table, not a quality score. precision@k, recall@k and mrr@k stay unavailable.",
        notes: "Anchors are empty; quality scores are unavailable.",
      };
    case "pipeline":
      return {
        title: "Code-index pipeline",
        state: "unsealed",
        source: "tracedecay.db",
        scope: `${PACK.projects.length} project spines`,
        affected: `heads known ${HEADS_KNOWN}/${PACK.projects.length} · facts ${FACTS_ABSENT} absent`,
        details: "The river is the instrument silhouette. No 12.4M docs, no building 41%. Serve is unavailable; core heads are unavailable.",
        notes: "The index is never claimed sealed.",
      };
    case "hooks":
      return {
        title: "Hook census",
        state: "host",
        source: "hook_analytics.jsonl",
        scope: `${fmt(HOOKS.lines)} lines · ${fmt(HOOKS.bytes)} bytes`,
        affected: `${fmt(HOOKS.events[0]?.count ?? 0)} invoked · ${fmt(HOOKS.events[1]?.count ?? 0)} completed`,
        details: "Event and disposition counts come from the host file. Not Doctor accepted/rejected argument hints.",
        notes: "Host census, not an inspection verdict.",
      };
    case "budgets":
      return {
        title: "Performance budgets",
        state: "unavailable",
        source: "runtime / provider",
        scope: `${BUDGET_ROWS.length} budget lines`,
        affected: "—",
        details: "No budget or spend series in this snapshot. The ledger renders with typed em-dash cells.",
        notes: "Not a 75th-percentile comparison.",
      };
    case "topology":
      return {
        title: "Execution topology",
        state: "unavailable",
        source: "runtime graph",
        scope: "5 node classes",
        affected: "—",
        details: "No execution topology was copied. The constellation is drawn with typed em-dash counts, not a 12-node cluster.",
        notes: "Discs are the instrument; counts stay absent.",
      };
    case "live":
      return {
        title: "Live code-index pipeline",
        state: "unavailable",
        source: "live sparkline",
        scope: "-60m .. now",
        affected: "—",
        details: "Capture is a still. There is no live NOW series in this pack; the rail renders empty.",
        notes: "Throughput, lag, backlog and ETA are typed absent.",
      };
    case "storage":
      return {
        title: "Store manifests",
        state: "exact",
        source: "store_manifest.json",
        scope: `${STORE_MANIFESTS.length} stores · graph tracedecay.db`,
        affected: "bytes / tiers —",
        details: "Manifests name stores exactly. They are not byte budgets or an orphan-segment census; tier tracks stay unfilled.",
        notes: "Identity, not capacity.",
      };
    case "findings":
      return {
        title: "Stale branch census",
        state: "measured",
        source: "core local refs",
        scope: `${BRANCHES.project} · ${BRANCHES.projectId.slice(0, 16)}…`,
        affected: `${fmt(BRANCHES.localRefCount)} local refs · ${BRANCHES.prefixCount} prefixes`,
        details: `Host-measured refs grouped by prefix — top ${BRANCHES.prefixes[0].prefix} ${fmt(BRANCHES.prefixes[0].count)} · ${BRANCHES.prefixes[1].prefix} ${fmt(BRANCHES.prefixes[1].count)} · ${BRANCHES.prefixes[2].prefix} ${fmt(BRANCHES.prefixes[2].count)}. Not 3,842 orphaned segments.`,
        notes: "Orphan census, prune plan and ETA stay typed absent.",
      };
    default: {
      const exhaustive: never = well;
      throw new Error(`unhandled well: ${exhaustive}`);
    }
  }
}

function Inspector(props: {
  well: Well;
  topologyId: string;
  mode: "snapshot" | "fixture";
  onTopologyFocus: (id: string) => void;
  onNavigate: (surface: string, params?: Record<string, string>) => void;
}) {
  const body = inspectBody(props.well);
  const focusedNode = OBSERVATORY_TOPOLOGY.nodes.find((candidate) => candidate.id === props.topologyId);
  const node = focusedNode?.well === props.well
    ? focusedNode
    : OBSERVATORY_TOPOLOGY.nodes.find((candidate) => candidate.well === props.well);
  const edges = node ? OBSERVATORY_TOPOLOGY.edges.filter((edge) => edge.from === node.id || edge.to === node.id) : [];
  const focusTopology = () => {
    if (!node) return;
    props.onTopologyFocus(node.id);
    requestAnimationFrame(() => {
      const target = document.querySelector<SVGGElement>(`[data-topology-node="${node.id}"]`);
      target?.closest(".ob-topology")?.scrollIntoView({ block: "nearest", behavior: "auto" });
      target?.focus();
    });
  };
  return (
    <aside id="ob-evidence" className="ob-insp" aria-label="Evidence inspector">
      <Corners />
      <span className="ob-kicker">EVIDENCE INSPECTOR</span>
      <h2 className="ob-insp-title">{body.title}</h2>
      <span className={`ob-chip ob-chip-large t-${body.state}`}>{STATE_LABEL[body.state]}</span>
      <div className="ob-insp-body">
        <div className="ob-fact">
          <span className="ob-fact-k">SOURCE</span>
          <span className="ob-fact-v">{body.source}</span>
        </div>
        <div className="ob-fact">
          <span className="ob-fact-k">OBSERVED</span>
          <span className="ob-fact-v">{PACK.capturedAt}</span>
        </div>
        <div className="ob-fact">
          <span className="ob-fact-k">STATUS</span>
          <span className={`ob-fact-v tone t-${body.state}`}>{STATE_LABEL[body.state].toLowerCase()}</span>
        </div>
        <div className="ob-fact">
          <span className="ob-fact-k">SCOPE</span>
          <span className="ob-fact-v">{body.scope}</span>
        </div>
        <div className="ob-fact">
          <span className="ob-fact-k">AFFECTED OBJECTS</span>
          <span className="ob-fact-v">{body.affected}</span>
        </div>
        <div className="ob-fact">
          <span className="ob-fact-k">DETAILS</span>
          <p className="ob-note">{body.details}</p>
        </div>
        <div className="ob-fact-line">
          <span className="ob-fact-k">RECOVERY</span>
          <span className="ob-fact-v danger">unavailable</span>
        </div>
        <div className="ob-fact-line">
          <span className="ob-fact-k">RECOVERY ETA</span>
          <span className="ob-fact-v">—</span>
        </div>
        <div className="ob-fact-line">
          <span className="ob-fact-k">LAST CONFIRMED</span>
          <span className="ob-fact-v">{CAPTURE_SHORT}</span>
        </div>
        <div className="ob-fact">
          <span className="ob-fact-k">NOTES</span>
          <p className="ob-note">{body.notes}</p>
        </div>
        {node && (
          <div className="ob-source-route">
            <span className="ob-fact-k">DEPENDENCY MAP</span>
            <p>{props.mode === "fixture"
              ? "AUTHORED EXAMPLE · relation shape only; no captured source provenance"
              : `${node.role === "source" ? "Source authority" : node.role === "affected" ? "Affected consumer" : node.role === "derived" ? "Derived observation" : "Independent observation"} · ${node.source}`}</p>
            <div>
              <button type="button" onClick={focusTopology}>FOCUS MAP NODE</button>
              <button type="button" onClick={() => props.onNavigate(node.target.surface, node.target.params)}>
                OPEN {node.target.surface.toUpperCase()} SURFACE · GENERIC
              </button>
            </div>
            <small>The exact source identity remains in this inspector; the destination surface does not consume it yet.</small>
          </div>
        )}
        {node && (
          <div className="ob-dependency-basis">
            <span className="ob-fact-k">DEPENDENCY BASIS</span>
            {edges.length === 0 ? (
              <p>This is an independent observation. The map makes no causal or control claim from it.</p>
            ) : edges.map((edge) => {
              const other = topologyNode(edge.from === node.id ? edge.to : edge.from);
              return (
                <div key={edge.id}>
                  <b>{edge.kind.toUpperCase()} {edge.from === node.id ? "TO" : "FROM"} {other.label.toUpperCase()}</b>
                  <p>{edge.summary}</p>
                </div>
              );
            })}
          </div>
        )}
      </div>
      <div className="ob-insp-foot">
        Authorities stay independent. No all-systems-nominal banner. Recovery stays unavailable until an authorized
        production route exists.
      </div>
    </aside>
  );
}

/* ---- page ---- */

type WellProps = { sel: Well; onSel: (id: Well) => void };

function findingFromUrl(): Well {
  const requested = new URLSearchParams(window.location.search).get("observatory_finding");
  return WELLS.find((well) => well === requested) ?? "findings";
}

function topologyFromUrl(): string {
  const requested = new URLSearchParams(window.location.search).get("topology");
  return OBSERVATORY_TOPOLOGY.nodes.some((node) => node.id === requested) ? requested! : "index";
}

function isWell(value: string | undefined): value is Well {
  return WELLS.includes(value as Well);
}

export function ObservatoryPage(props: { onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { mode, navigate } = useDemo();
  const [sel, setSel] = useWorkspaceState<Well>("observatory:selected-source", findingFromUrl());
  const [topologyId, setTopologyId] = useWorkspaceState("observatory:topology-focus", topologyFromUrl());
  const attention = OBSERVATORY_ATTENTION.filter((item) => item.mode === mode && item.status === "active");
  const selectWell = (well: Well) => {
    setSel(well);
    props.onState?.(well);
    const url = new URL(window.location.href);
    url.searchParams.set("observatory_finding", well);
    if (url.href !== window.location.href) window.history.pushState(null, "", url);
  };
  const focusTopology = (id: string) => {
    if (!OBSERVATORY_TOPOLOGY.nodes.some((node) => node.id === id)) return;
    setTopologyId(id);
    const url = new URL(window.location.href);
    url.searchParams.set("topology", id);
    if (url.href !== window.location.href) window.history.replaceState(null, "", url);
  };
  useEffect(() => {
    const restore = () => {
      setSel(findingFromUrl());
      setTopologyId(topologyFromUrl());
    };
    window.addEventListener("popstate", restore);
    return () => window.removeEventListener("popstate", restore);
  }, [setSel, setTopologyId]);
  useEffect(() => {
    if (isWell(props.state)) setSel(props.state);
  }, [props.state, setSel]);
  const wp: WellProps = { sel, onSel: selectWell };
  const selectAttention = (item: AttentionItem) => {
    const nodeId = item.target.params.topology;
    if (nodeId) focusTopology(nodeId);
    const node = nodeId ? OBSERVATORY_TOPOLOGY.nodes.find((candidate) => candidate.id === nodeId) : undefined;
    const requested = item.target.params.observatory_finding;
    const destination = node && isWell(node.well) ? node.well : isWell(requested) ? requested : "findings";
    selectWell(destination);
  };

  return (
    <div className="ob-root" onKeyDown={(event) => {
      if (event.key === "Escape") event.currentTarget.querySelector<HTMLButtonElement>('.ob-panel[aria-pressed="true"]')?.focus();
    }}>
      <TimelineStrip mode={mode} attention={attention} onAttention={selectAttention} />
      <div className="ob-main">
        <div className="ob-row">
          <DoctorCard {...wp} />
          <AdoptionCard {...wp} />
          <RetrievalCard {...wp} />
        </div>
        <PipelineCard {...wp} />
        <div className="ob-row">
          <HooksCard {...wp} />
          <BudgetsCard {...wp} />
          <TopologyCard {...wp} focused={topologyId} mode={mode} onFocus={focusTopology} />
        </div>
        <div className="ob-row">
          <LiveCard {...wp} />
          <TelemetryCard {...wp} />
          <FindingsCard {...wp} />
        </div>
      </div>
      <Inspector well={sel} topologyId={topologyId} mode={mode} onTopologyFocus={focusTopology} onNavigate={navigate} />
    </div>
  );
}
