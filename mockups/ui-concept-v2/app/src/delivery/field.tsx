import { useId, useLayoutEffect, useRef, useState } from "react";
import { JOURNEY_PHASES, type JourneyLane, type LaneEvent } from "./data";

export function hash32(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

export function mulberry(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Only source-addressable episodes are rendered; no invented activity dots. */
export function laneBursts(_lane: JourneyLane): { t: number; r: number; o: number }[] { return []; }

const HOURS = ["08:00", "09:00", "10:00", "11:00", "12:00", "13:00", "14:00", "15:00", "16:00", "17:00", "18:00", "19:00", "20:00", "21:00", "22:00"];

export function TimeRuler(props: { hours?: string[]; chips?: boolean }) {
  const hours = props.hours ?? HOURS;
  const [zoom, setZoom] = useState(1);
  const root = useRef<HTMLDivElement>(null);
  const setScale = (value: number) => {
    setZoom(value);
    root.current?.closest<HTMLElement>(".dl-pane")?.style.setProperty("--dl-timezoom", String(value));
  };
  return <div className="dl-ruler2" ref={root}>
    <span className="lab">TIME RULER (UTC)</span>
    <div className="hours">{hours.map((h) => <span key={h}>{h}</span>)}</div>
    {props.chips ? <div className="chips">
      <button aria-label="Pan timeline left" onClick={() => root.current?.closest(".dl-pane")?.querySelector(".dl-jbody")?.scrollBy({ left: -180 })}>‹</button>
      <button aria-label="Pan timeline right" onClick={() => root.current?.closest(".dl-pane")?.querySelector(".dl-jbody")?.scrollBy({ left: 180 })}>›</button>
      <button aria-label="Zoom timeline out" onClick={() => setScale(Math.max(1, zoom - 0.5))}>−</button>
      <span className="on">{Math.round(zoom * 100)}%</span>
      <button aria-label="Zoom timeline in" onClick={() => setScale(Math.min(4, zoom + 0.5))}>+</button>
      <button onClick={() => setScale(1)}>Fit</button>
    </div> : null}
  </div>;
}

export function EpisodeDetails(props: { ev: LaneEvent; lane: string }) {
  return <>
    <h4>{props.ev.label}</h4>
    <p>{props.lane} · {props.ev.kind} · {props.ev.grade}</p>
    <p>{props.ev.time ? `Recorded caption: ${props.ev.time}` : `Position in loaded example: ${Math.round(props.ev.t * 100)}%`}</p>
    <p>{props.ev.kind === "gap" ? "This interval is missing or qualified evidence. Its source cannot be reconstructed from the drawing." : "Authored design example. The episode metadata is available; a source transcript, command or exact diff is not attached to this snapshot."}</p>
    <a className="dl-btn" href="?data=fixture&surface=delivery&state=09&example=compiler-cache-8127">Explore separate PR8127 review example →</a>
  </>;
}

export function EpisodeTable(props: { lanes: JourneyLane[] }) {
  return <details className="dl-exact-fallback"><summary>Exact event table · loaded example</summary>
    <table><thead><tr><th>Agent / rail</th><th>Episode</th><th>Time</th><th>Evidence grade</th></tr></thead><tbody>
      {props.lanes.flatMap((lane) => lane.events.map((ev, i) => ({ lane, ev, i }))).sort((a, b) => a.ev.t - b.ev.t).map(({ lane, ev, i }) => <tr key={`${lane.id}-${i}`}><td>{lane.label}</td><td>{ev.label}</td><td>{ev.time ?? `${Math.round(ev.t * 100)}%`}</td><td>{ev.grade}</td></tr>)}
    </tbody></table>
  </details>;
}

export function PhaseRow(props: { phases?: { t: number; label: string; time: string }[] }) {
  const phases = props.phases ?? JOURNEY_PHASES;
  return (
    <div className="dl-phaserow">
      <i className="spine" />
      {phases.map((p) => (
        <span className="ms" key={p.label} style={{ left: `${p.t * 100}%` }} data-evkey={`phase|${p.label}`}>
          <b>{p.label}</b>
          <i className="ring" />
          <em>{p.time}</em>
        </span>
      ))}
    </div>
  );
}

const RING_GLYPH: Record<LaneEvent["kind"], string> = {
  task: "▣",
  spawn: "✳",
  handoff: "⇢",
  commit: "⬡",
  test: "✓",
  review: "◉",
  ci: "⚙",
  gap: "▨",
  fail: "!",
  ghost: "·",
};

function EventMark(props: {
  ev: LaneEvent;
  laneId: string;
  color: string;
  showLabel?: boolean;
  selected?: boolean;
  ring?: boolean;
  onInspect?: () => void;
}) {
  const { ev } = props;
  const detailId = useId();
  const details = <div popover="auto" id={detailId} className="dl-evidence-popover"><button popoverTarget={detailId} popoverTargetAction="hide" aria-label="Close episode details">×</button><EpisodeDetails ev={ev} lane={props.laneId} /></div>;
  const left = `${ev.t * 100}%`;
  if (ev.kind === "gap" && ev.end != null) {
    return (
      <><button type="button" onClick={props.onInspect} popoverTarget={detailId} aria-label={`Inspect gap: ${ev.label}`}
        className="dl-gapspan"
        style={{ left, width: `${(ev.end - ev.t) * 100}%` }}
        data-evkey={`${props.laneId}|${ev.label}`}
        data-kind="gap"
        data-grade={ev.grade}
      >
        <span className="cap">
          {ev.label}
          {ev.time ? <em>{ev.time}</em> : null}
        </span>
      </button>{details}</>
    );
  }
  const cls =
    ev.kind === "ghost"
      ? "dl-ev2 is-ghost"
      : ev.kind === "fail"
        ? `dl-ev2 is-fail${props.selected ? " is-selected" : ""}`
        : ev.kind === "gap"
          ? "dl-ev2 is-gapdot"
          : props.ring
            ? `dl-ev2 is-ring${ev.big ? " is-big" : ""}`
            : `dl-ev2${ev.big ? " is-big" : ""}`;
  return (
    <><button type="button" disabled={ev.kind === "ghost"} onClick={props.onInspect} popoverTarget={detailId} aria-label={`Inspect episode: ${ev.label}`}
      className={cls}
      style={{ left, color: props.color, marginTop: ev.row ? ev.row * 24 : 0 }}
      title={`${ev.label} · ${ev.grade}`}
      data-evkey={`${props.laneId}|${ev.label}`}
      data-kind={ev.kind}
      data-grade={ev.grade}
      data-row={ev.row ?? 0}
    >
      <i className="dot">{props.ring && ev.kind !== "ghost" ? RING_GLYPH[ev.kind] : null}</i>
      {props.showLabel !== false ? (
        <span className="cap">
          {ev.label}
          {ev.time ? <em>{ev.time}</em> : null}
        </span>
      ) : null}
      {props.selected ? <span className="seltag">SELECTED EVENT</span> : null}
    </button>{details}</>
  );
}

function LaneGlyph(props: { lane: JourneyLane }) {
  const ch = props.lane.label.charAt(0).toUpperCase();
  return (
    <i className="glyph" style={{ borderColor: props.lane.color, color: props.lane.color }}>
      {ch}
    </i>
  );
}

export function LaneRow(props: {
  lane: JourneyLane;
  query?: string;
  showLabels?: boolean;
  cursor?: number;
  maskFuture?: boolean;
  selected?: string;
  tall?: boolean;
  noSpine?: boolean;
  rings?: boolean;
  onInspect?: (lane: JourneyLane, ev: LaneEvent) => void;
}) {
  const lane = props.query && !`${props.lane.label} ${props.lane.role}`.toLowerCase().includes(props.query.toLowerCase())
    ? { ...props.lane, events: props.lane.events.filter((ev) => `${ev.label} ${ev.kind} ${ev.grade}`.toLowerCase().includes(props.query!.toLowerCase())), burst: 0 }
    : props.lane;
  if (!lane.events.length) return null;
  const bursts = laneBursts(lane);
  const anchors = [
    ...lane.events.filter((e) => e.kind !== "ghost").map((e) => (e.kind === "gap" && e.end != null ? e.end : e.t)),
    ...lane.events.map((e) => e.t),
    ...bursts.map((b) => b.t),
  ];
  const lo = anchors.length ? Math.max(0, Math.min(...anchors) - 0.012) : 0;
  const hi = anchors.length ? Math.min(1, Math.max(...anchors) + 0.012) : 1;
  return (
    <div className={props.tall ? "dl-lane2 tall" : "dl-lane2"}>
      <div className="who">
        <LaneGlyph lane={lane} />
        <div>
          <b>{lane.label}</b>
          <em>{lane.role}</em>
        </div>
      </div>
      <div className="track">
        {props.noSpine ? null : (
          <i
            className="spine"
            style={{
              background: lane.color,
              left: `${lo * 100}%`,
              right: `${(1 - hi) * 100}%`,
              boxShadow: `0 0 6px ${lane.color}`,
            }}
          />
        )}
        {bursts.map((b, i) => {
          if (props.maskFuture && props.cursor != null && b.t > props.cursor) return null;
          return (
            <i
              key={`b${i}`}
              className="burst"
              style={{
                left: `${b.t * 100}%`,
                width: b.r * 2,
                height: b.r * 2,
                background: lane.color,
                opacity: b.o,
                boxShadow: `0 0 4px ${lane.color}`,
              }}
            />
          );
        })}
        {lane.events.map((ev, i) => {
          const future = props.cursor != null && ev.t > props.cursor;
          if (props.maskFuture && future && ev.kind !== "ghost") return null;
          if (!props.maskFuture && ev.kind === "ghost") return null;
          return (
            <EventMark
              key={i}
              ev={ev}
              laneId={lane.id}
              color={lane.color}
              showLabel={props.showLabels}
              selected={props.selected != null && props.selected === ev.label}
              ring={props.rings}
              onInspect={() => props.onInspect?.(props.lane,ev)}
            />
          );
        })}
      </div>
    </div>
  );
}

/* ---- Measured SVG link overlay: per-lane chains + cross-lane spawn/return curves ---- */

export type Connector = {
  from: string;
  to: string;
  color: string;
  dash?: boolean;
  /** Horizontal bulge added to the curve control points. */
  sweep?: number;
};

type LinkPath = { d: string; stroke: string; dash?: string; w: number; o: number; key: string };
type GhostBox = { x: number; y: number; w: number; h: number; key: string };
type LinkDot = { x: number; y: number; r: number; color: string; key: string };

const SOFT_GRADES = new Set(["INFERRED", "AMBIGUOUS", "STALE"]);

function buildPaths(
  host: HTMLElement,
  lanes: JourneyLane[],
  connectors: Connector[] | undefined,
): { paths: LinkPath[]; boxes: GhostBox[]; dots: LinkDot[] } {
  const origin = host.getBoundingClientRect();
  const paths: LinkPath[] = [];
  const boxes: GhostBox[] = [];
  const dots: LinkDot[] = [];
  const find = (key: string) => host.querySelector<HTMLElement>(`[data-evkey="${CSS.escape(key)}"]`);
  const rectOf = (el: HTMLElement) => {
    const r = el.getBoundingClientRect();
    return { x1: r.left - origin.left, x2: r.right - origin.left, cx: r.left + r.width / 2 - origin.left, cy: r.top + r.height / 2 - origin.top };
  };

  for (const lane of lanes) {
    type Item = { ev: LaneEvent; r: ReturnType<typeof rectOf> };
    const items: Item[] = [];
    const ghosts: Item[] = [];
    for (const ev of lane.events) {
      const el = find(`${lane.id}|${ev.label}`);
      if (!el) continue;
      const it = { ev, r: rectOf(el) };
      if (ev.kind === "ghost") ghosts.push(it);
      else items.push(it);
    }
    for (let i = 1; i < items.length; i++) {
      const a = items[i - 1];
      const b = items[i];
      const sx = a.ev.kind === "gap" ? a.r.x2 : a.r.cx;
      const ex = b.ev.kind === "gap" ? b.r.x1 : b.r.cx;
      if (ex - sx < 3) continue;
      const soft = SOFT_GRADES.has(a.ev.grade) || SOFT_GRADES.has(b.ev.grade);
      paths.push({
        d: `M ${sx.toFixed(1)} ${a.r.cy.toFixed(1)} L ${ex.toFixed(1)} ${b.r.cy.toFixed(1)}`,
        stroke: lane.color,
        dash: soft ? "6 4" : undefined,
        w: soft ? 1.4 : 1.9,
        o: soft ? 0.65 : 0.92,
        key: `${lane.id}-seg-${i}`,
      });
      if (!soft && ex - sx > 44) {
        const n = Math.min(6, Math.floor((ex - sx - 28) / 15));
        for (let k = 1; k <= n; k++) {
          const f = k / (n + 1);
          dots.push({
            x: sx + (ex - sx) * f,
            y: a.r.cy + (b.r.cy - a.r.cy) * f,
            r: k % 3 === 0 ? 2.6 : 2,
            color: lane.color,
            key: `${lane.id}-bead-${i}-${k}`,
          });
        }
      }
    }
    if (ghosts.length) {
      const base = items.length ? items[items.length - 1] : null;
      const byRow = new Map<number, Item[]>();
      for (const g of ghosts) {
        const row = g.ev.row ?? 0;
        if (!byRow.has(row)) byRow.set(row, []);
        byRow.get(row)!.push(g);
      }
      for (const [row, group] of byRow) {
        let px = base ? base.r.cx : group[0].r.cx;
        let py = base ? base.r.cy : group[0].r.cy;
        for (const g of group) {
          paths.push({
            d:
              Math.abs(g.r.cy - py) > 3
                ? `M ${px.toFixed(1)} ${py.toFixed(1)} C ${(px + 14).toFixed(1)} ${py.toFixed(1)}, ${(g.r.cx - 14).toFixed(1)} ${g.r.cy.toFixed(1)}, ${g.r.cx.toFixed(1)} ${g.r.cy.toFixed(1)}`
                : `M ${px.toFixed(1)} ${py.toFixed(1)} L ${g.r.cx.toFixed(1)} ${g.r.cy.toFixed(1)}`,
            stroke: "#6b7784",
            dash: "3 4",
            w: 1,
            o: 0.6,
            key: `${lane.id}-ghost-${row}-${g.ev.label}-${g.r.cx.toFixed(0)}`,
          });
          px = g.r.cx;
          py = g.r.cy;
        }
      }
      const gx1 = Math.min(...ghosts.map((g) => g.r.cx)) - 16;
      const gx2 = Math.max(...ghosts.map((g) => g.r.cx)) + 20;
      const gy1 = Math.min(...ghosts.map((g) => g.r.cy)) - 18;
      const gy2 = Math.max(...ghosts.map((g) => g.r.cy)) + 26;
      boxes.push({ x: gx1, y: gy1, w: gx2 - gx1, h: gy2 - gy1, key: `${lane.id}-ghostbox` });
    }
  }

  for (const c of connectors ?? []) {
    const fromEl = find(c.from);
    const toEl = find(c.to);
    if (!fromEl || !toEl) continue;
    const a = rectOf(fromEl);
    const b = rectOf(toEl);
    const dy = b.cy - a.cy;
    const sweep = c.sweep ?? 0;
    paths.push({
      d: `M ${a.cx.toFixed(1)} ${a.cy.toFixed(1)} C ${(a.cx + sweep).toFixed(1)} ${(a.cy + dy * 0.55).toFixed(1)}, ${(b.cx + sweep).toFixed(1)} ${(b.cy - dy * 0.55).toFixed(1)}, ${b.cx.toFixed(1)} ${b.cy.toFixed(1)}`,
      stroke: c.color,
      dash: c.dash ? "5 4" : undefined,
      w: 1.5,
      o: 0.65,
      key: `conn-${c.from}-${c.to}`,
    });
  }
  return { paths, boxes, dots };
}

export function LaneLinks(props: { lanes: JourneyLane[]; connectors?: Connector[] }) {
  const ref = useRef<SVGSVGElement>(null);
  const [state, setState] = useState<{ paths: LinkPath[]; boxes: GhostBox[]; dots: LinkDot[] }>({ paths: [], boxes: [], dots: [] });
  const serialized = useRef("");

  useLayoutEffect(() => {
    const svg = ref.current;
    if (!svg) return;
    const host = svg.parentElement;
    if (!host) return;
    const draw = () => {
      const next = buildPaths(host, props.lanes, props.connectors);
      const sig = next.paths.map((p) => p.d).join(";") + next.boxes.map((b) => `${b.x},${b.y},${b.w},${b.h}`).join(";");
      if (sig !== serialized.current) {
        serialized.current = sig;
        setState(next);
      }
    };
    draw();
    const t = window.setTimeout(draw, 350);
    const ro = new ResizeObserver(draw);
    ro.observe(host);
    return () => {
      window.clearTimeout(t);
      ro.disconnect();
    };
  });

  return (
    <svg ref={ref} className="dl-links" aria-hidden="true">
      {state.boxes.map((b) => (
        <rect
          key={b.key}
          x={b.x}
          y={b.y}
          width={b.w}
          height={b.h}
          rx={14}
          fill="rgba(107, 119, 132, 0.05)"
          stroke="#6b7784"
          strokeOpacity="0.55"
          strokeWidth="1"
          strokeDasharray="4 4"
        />
      ))}
      {state.paths.map((p) => (
        <g key={p.key}>
          {p.stroke !== "#6b7784" ? (
            <path d={p.d} fill="none" stroke={p.stroke} strokeOpacity={p.o * 0.22} strokeWidth={p.w * 3.6} />
          ) : null}
          <path d={p.d} fill="none" stroke={p.stroke} strokeOpacity={p.o} strokeWidth={p.w} strokeDasharray={p.dash} />
        </g>
      ))}
      {state.dots.map((d) => (
        <g key={d.key}>
          <circle cx={d.x} cy={d.y} r={d.r * 2} fill={d.color} opacity="0.2" />
          <circle cx={d.x} cy={d.y} r={d.r} fill={d.color} opacity="0.92" />
        </g>
      ))}
    </svg>
  );
}

export function JourneyCanvas(props: {
  lanes: JourneyLane[];
  rails?: JourneyLane[];
  railsTitle?: string;
  lanesTitle?: string;
  phases?: { t: number; label: string; time: string }[];
  showLabels?: boolean;
  cursor?: number;
  cursorTime?: string;
  cursorPct?: string;
  maskFuture?: boolean;
  selected?: string;
  legend?: React.ReactNode;
  hideRuler?: boolean;
  tall?: boolean;
  /** Draw connected per-lane chains instead of full-width spines. */
  link?: boolean;
  rings?: boolean;
  connectors?: Connector[];
}) {
  const phases = props.phases ?? JOURNEY_PHASES;
  const allLanes = [...props.lanes, ...(props.rails ?? [])];
  return (
    <div className="dl-jcanvas">
      {props.hideRuler ? null : <TimeRuler chips />}
      <div className="dl-jbody">
        <div className="grid" aria-hidden="true">
          {phases.map((p) => (
            <i key={p.label} style={{ left: `${p.t * 100}%` }} />
          ))}
        </div>
        {props.link ? <LaneLinks lanes={allLanes} connectors={props.connectors} /> : null}
        {props.cursor != null ? (
          <div className="cursor" style={{ left: `calc(150px + (100% - 158px) * ${props.cursor})` }}>
            <span className="t">
              {props.cursorTime}
              {props.cursorPct ? <em>{props.cursorPct}</em> : null}
            </span>
          </div>
        ) : null}
        <div className="inner">
          {phases.length ? <PhaseRow phases={phases} /> : null}
          {props.lanesTitle ? <div className="dl-lanesec">{props.lanesTitle}</div> : null}
          <div className="lanes">
            {props.lanes.map((lane) => (
              <LaneRow
                key={lane.id}
                lane={lane}
                showLabels={props.showLabels}
                cursor={props.cursor}
                maskFuture={props.maskFuture}
                selected={props.selected}
                tall={props.tall}
                noSpine={props.link}
                rings={props.rings}
              />
            ))}
          </div>
          {props.rails ? (
            <>
              <div className="dl-lanesec">
                {props.railsTitle ?? "CROSS-REPO PR RAILS"} <span className="chip">Collapse all {props.rails.length}</span>
              </div>
              <div className="lanes rails">
                {props.rails.map((lane) => (
                  <LaneRow
                    key={lane.id}
                    lane={lane}
                    showLabels={props.showLabels}
                    cursor={props.cursor}
                    maskFuture={props.maskFuture}
                    tall={props.tall}
                    noSpine={props.link}
                    rings={props.rings}
                  />
                ))}
              </div>
            </>
          ) : null}
          {props.legend}
        </div>
      </div>
    </div>
  );
}

export function JourneyMinimap(props: {
  lanes: JourneyLane[];
  window?: [number, number];
  title?: string;
  legend?: { label: string; color: string }[];
  hours?: string[];
  admittedThrough?: number;
}) {
  const rows = props.lanes;
  const h = rows.length * 9 + 12;
  return (
    <div className="dl-minimap2">
      {props.title ? <div className="cap">{props.title}</div> : null}
      <div className="strip">
        <svg viewBox={`0 0 1000 ${h}`} preserveAspectRatio="none" aria-hidden="true">
          {rows.map((lane, r) => {
            const y = 8 + r * 9;
            const bursts = props.admittedThrough == null ? laneBursts({ ...lane, quiet: false, burst: lane.burst ?? 26 }) : [];
            const evs = lane.events.filter((e) => e.kind !== "ghost" && (props.admittedThrough == null || e.t <= props.admittedThrough));
            return (
              <g key={lane.id}>
                <line x1="0" x2="1000" y1={y} y2={y} stroke={lane.color} strokeOpacity="0.14" strokeDasharray="2 5" />
                {bursts.map((b, i) => (
                  <circle key={i} cx={b.t * 1000} cy={y + (((i * 7919) % 7) - 3) * 0.9} r={1.7} fill={lane.color} opacity={b.o * 0.9} />
                ))}
                {evs.map((e, i) => (
                  <circle key={`e${i}`} cx={e.t * 1000} cy={y} r={2.6} fill={lane.color} opacity="0.95" />
                ))}
              </g>
            );
          })}
          {props.window ? (
            <rect
              x={props.window[0] * 1000}
              y={1}
              width={(props.window[1] - props.window[0]) * 1000}
              height={h - 2}
              fill="rgba(94,231,255,0.06)"
              stroke="#5ee7ff"
              strokeWidth="1.5"
              vectorEffect="non-scaling-stroke"
            />
          ) : null}
        </svg>
        {props.legend ? (
          <div className="mlegend">
            {props.legend.map((l) => (
              <div key={l.label}>
                <i style={{ background: l.color }} /> {l.label}
              </div>
            ))}
          </div>
        ) : null}
      </div>
      {props.hours ? (
        <div className="hours">
          {props.hours.map((x) => (
            <span key={x}>{x}</span>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** Dense 128-agent fan-out (state 12): labeled horizontal workstream bands ending at distinct evidence boundaries. */
export function DenseFanout(props: {
  streams: { id: string; label: string; agents: number; episodes: number; color: string }[];
  focus?: string;
  onSelect?: (id: string) => void;
}) {
  const W = 1200;
  const H = 560;
  const x0 = 152;
  const outcome = { x: 1130, y: 268 };
  const xEnd = outcome.x - 100;

  /** Band point: context braid. No recorded outcome relation is available to justify convergence. */
  function bandPoint(baseY: number, t: number, wobble: number) {
    const x = x0 + (xEnd - x0) * t;
    return { x, y: baseY + wobble };
  }

  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="dl-densesvg" role="group" aria-label="Dense agent fan-out across workstreams" preserveAspectRatio="none">
      <title>128 agents across six workstreams. Concept population, not a product ceiling.</title>
      <defs>
        <filter id="dlSoft2" x="-80%" y="-80%" width="260%" height="260%">
          <feGaussianBlur stdDeviation="1.2" result="b" />
          <feMerge>
            <feMergeNode in="b" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <filter id="dlBloom2" x="-140%" y="-140%" width="380%" height="380%">
          <feGaussianBlur stdDeviation="5" />
        </filter>
        <radialGradient id="dlOutcomeHalo" cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor="#5ee7ff" stopOpacity="0.32" />
          <stop offset="60%" stopColor="#5ee7ff" stopOpacity="0.08" />
          <stop offset="100%" stopColor="#5ee7ff" stopOpacity="0" />
        </radialGradient>
        <radialGradient id="dlDenseDepth" cx="52%" cy="42%" r="70%">
          <stop offset="0%" stopColor="#0c1522" stopOpacity="0.9" />
          <stop offset="65%" stopColor="#070b12" stopOpacity="0.4" />
          <stop offset="100%" stopColor="#05080e" stopOpacity="0" />
        </radialGradient>
      </defs>
      <rect x="0" y="0" width={W} height={H} fill="url(#dlDenseDepth)" />
      {Array.from({ length: 13 }, (_, i) => {
        const x = x0 + ((xEnd - x0) * i) / 12;
        return <line key={i} x1={x} x2={x} y1={10} y2={H - 8} stroke="#8caabe" strokeOpacity="0.08" strokeDasharray="2 7" />;
      })}

      {props.streams.map((s, si) => {
        const rnd = mulberry(hash32(s.id));
        const baseY = 58 + si * 88;
        const focused = props.focus === s.id;
        const dimmed = props.focus ? !focused : false;
        const filaments = Array.from({ length: 12 }, (_, i) => {
          const off = (rnd() - 0.5) * 24;
          const amp = 3 + rnd() * 8;
          const phase = rnd() * Math.PI * 2;
          const freq = 5 + rnd() * 5;
          const pts: { x: number; y: number }[] = [];
          for (let b = 0; b <= 44; b++) {
            const t = b / 44;
            pts.push(bandPoint(baseY + off, t, Math.sin(phase + t * freq) * amp));
          }
          const d = pts.map((p, b) => `${b === 0 ? "M" : "L"} ${p.x.toFixed(1)} ${p.y.toFixed(1)}`).join(" ");
          return { d, pts, bright: i < 3, o: i < 3 ? 0.7 : 0.28 + rnd() * 0.18, w: i < 3 ? 1.4 : 1 };
        });
        return (
          <g key={s.id} opacity={dimmed ? 0.62 : 1} role="button" tabIndex={0} aria-label={`Focus workstream ${s.label}`} aria-pressed={focused}
            onClick={() => props.onSelect?.(s.id)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onSelect?.(s.id); } }}>
            <rect x="5" y={baseY - 32} width={160} height={65} fill="transparent" />
            {filaments.map((f, i) => (
              <g key={`f${i}`}>
                {f.bright ? <path d={f.d} fill="none" stroke={s.color} strokeOpacity="0.18" strokeWidth="4.5" filter="url(#dlBloom2)" /> : null}
                <path d={f.d} fill="none" stroke={s.color} strokeOpacity={f.o} strokeWidth={f.w} filter={f.bright ? "url(#dlSoft2)" : undefined} />
              </g>
            ))}
            <g>
              <path d={`M ${xEnd} ${baseY - 10} v20 m-5 -20 v20`} stroke={s.color} strokeWidth="1.5" />
              <rect x={4} y={baseY - 27} width={140} height={54} rx={9} fill="rgba(7,11,18,0.85)" stroke={s.color} strokeOpacity={focused ? 0.9 : 0.4} strokeWidth={focused ? 1.6 : 1} filter={focused ? "url(#dlSoft2)" : undefined} />
              <circle cx={24} cy={baseY} r={11} fill={s.color} opacity="0.22" filter="url(#dlBloom2)" />
              <circle cx={24} cy={baseY} r={8.5} fill="none" stroke={s.color} strokeWidth="1.4" filter="url(#dlSoft2)" />
              <circle cx={24} cy={baseY} r={3} fill={s.color} />
              <text x={41} y={baseY - 7} className="dl-svg-stream" fill={s.color}>
                {s.label}
              </text>
              <text x={41} y={baseY + 6} className="dl-svg-streamsub">
                {s.agents} agents
              </text>
              <text x={41} y={baseY + 18} className="dl-svg-streamsub">
                {s.episodes} episodes
              </text>
            </g>
          </g>
        );
      })}

      <g aria-label="Outcome evidence unavailable">
        <circle cx={outcome.x} cy={outcome.y} r={14} fill="none" stroke="#8b99a8" strokeWidth="1.8" strokeDasharray="3 4" />
        <path d={`M ${outcome.x - 6} ${outcome.y - 6} l12 12 m0 -12 l-12 12`} stroke="#8b99a8" strokeWidth="1" />
      </g>
      <text x={outcome.x} y={outcome.y + 48} textAnchor="middle" className="dl-svg-streamsub" fill="#5ee7ff">
        Outcome evidence
      </text>
      <text x={outcome.x} y={outcome.y + 65} textAnchor="middle" className="dl-svg-streamsub">not attached</text>
    </svg>
  );
}
