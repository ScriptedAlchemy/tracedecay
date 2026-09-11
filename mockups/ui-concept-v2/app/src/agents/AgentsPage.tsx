import { useEffect, useMemo, useRef, useState } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { PACK, shortId } from "../data/pack";
import { loomPivotUrl } from "../loom/pivots";
import { DenseAgents } from "./DenseAgents";
import {
  CAPTURED_AT,
  DANGLING,
  FAMILIES,
  KIND_COUNTS,
  PARENT_MISSING,
  PARENT_RESOLVED,
  RECENT_TOOLS,
  TOTALS,
  TOOL_COUNTS,
  UNLINKED,
  clock,
  defaultSelectedId,
  nodeById,
  type AgentNode,
} from "./model";
import "./agents.css";

const UNLINKED_ID = "__unlinked__";
const GHOST_PREFIX = "__ghost__:";

function loomSessionUrl(sessionId: string) {
  const url = new URL(location.href);
  ["loom_pivot", "loom_target_session", "loom_target_event", "loom_return"].forEach((key) => url.searchParams.delete(key));
  url.searchParams.set("surface", "loom");
  url.searchParams.delete("state");
  url.searchParams.set("loom_source", "mac");
  url.searchParams.set("loom_session", sessionId);
  url.searchParams.set("loom_follow", "0");
  return url.pathname + url.search;
}

type Kind = "root" | "child" | "bundle" | "ghost";

type Laid = {
  id: string;
  kind: Kind;
  x: number;
  y: number;
  r: number;
  title: string;
  sub: string;
  msg: string;
  amber?: boolean;
};

type Caption = { x: number; y: number; text: string };

type Edge = { a: string; b: string; kind: "parentage"; missing?: boolean };

function radius(messages: number, max: number) {
  const t = Math.sqrt(Math.max(0, messages) / Math.max(1, max));
  return 6.5 + t * 9;
}

function bezier(a: Laid, b: Laid) {
  const mx = (a.x + b.x) / 2;
  return `M ${a.x} ${a.y} C ${mx} ${a.y}, ${mx} ${b.y}, ${b.x} ${b.y}`;
}

type Scene = {
  nodes: Laid[];
  edges: Edge[];
  cols: { x: number; gen: string; role: string }[];
  captions: Caption[];
  height: number;
  pitch: number;
};

function layout(w: number, h: number): Scene {
  const padTop = 38;
  const padBot = 2;
  const famGap = 4;
  const gutter = 22;
  const bundleBlock = 18;
  const col0 = 96;
  // Delegates must stay right of sources even in narrow fields: never let the
  // width clamp pull GEN 1 onto (or left of) GEN 0.
  const col1 = Math.max(col0 + 110, Math.min(Math.max(w * 0.42, 300), w - 240));
  const maxMsg = Math.max(1, ...PACK.sessions.map((s) => s.messages));

  // One ghost source per distinct missing parent — dangling sessions must not
  // be attributed to a parent they never named.
  const ghostGroups = new Map<string, typeof DANGLING>();
  DANGLING.forEach((d) => {
    const pid = d.session.parentId ?? d.session.id;
    const g = ghostGroups.get(pid);
    if (g) g.push(d);
    else ghostGroups.set(pid, [d]);
  });

  const childRows = FAMILIES.reduce((s, f) => s + f.children.length, 0) + DANGLING.length;
  const overhead =
    padTop +
    Math.max(0, FAMILIES.length - 1) * famGap +
    gutter +
    Math.max(0, ghostGroups.size - 1) * famGap +
    bundleBlock +
    padBot;
  const pitch = Math.max(13, Math.min(26, Math.floor((h - overhead) / Math.max(1, childRows))));

  const nodes: Laid[] = [];
  const edges: Edge[] = [];
  const captions: Caption[] = [];

  let y = padTop;
  FAMILIES.forEach((fam, fi) => {
    if (fi > 0) y += famGap;
    const childYs: number[] = [];
    fam.children.forEach((ch) => {
      childYs.push(y);
      nodes.push({
        id: ch.session.id,
        kind: "child",
        x: col1,
        y,
        r: radius(ch.session.messages, maxMsg),
        title: shortId(ch.session.agentId ?? ch.session.id, 12),
        sub: "",
        msg: `${ch.session.messages.toLocaleString()} msg`,
      });
      y += pitch;
    });
    const y0 = childYs.length ? childYs.reduce((a, b) => a + b, 0) / childYs.length : padTop;
    nodes.push({
      id: fam.session.id,
      kind: "root",
      x: col0,
      y: y0,
      r: Math.max(11, radius(fam.session.messages, maxMsg)),
      title: fam.session.project,
      sub: `${fam.children.length} delegates · ${fam.session.messages.toLocaleString()} msg`,
      msg: "",
    });
    fam.children.forEach((ch) => edges.push({ a: fam.session.id, b: ch.session.id, kind: "parentage" }));
  });

  if (ghostGroups.size) {
    y += gutter;
    captions.push({ x: col1 + 14, y: y - 12, text: "parent not in snapshot" });
    let gi = 0;
    ghostGroups.forEach((members, pid) => {
      if (gi > 0) y += famGap;
      const dangYs: number[] = [];
      members.forEach((d) => {
        dangYs.push(y);
        nodes.push({
          id: d.session.id,
          kind: "child",
          x: col1,
          y,
          r: radius(d.session.messages, maxMsg),
          title: shortId(d.session.agentId ?? d.session.id, 12),
          sub: "",
          msg: `${d.session.messages.toLocaleString()} msg`,
          amber: true,
        });
        y += pitch;
      });
      const ghostId = GHOST_PREFIX + pid;
      const gy = dangYs.reduce((a, b) => a + b, 0) / dangYs.length;
      nodes.push({
        id: ghostId,
        kind: "ghost",
        x: col0,
        y: gy,
        r: 7,
        title: shortId(pid, 10),
        sub: "not in snapshot",
        msg: "",
        amber: true,
      });
      members.forEach((d) => edges.push({ a: ghostId, b: d.session.id, kind: "parentage", missing: true }));
      gi += 1;
    });
  }

  nodes.push({
    id: UNLINKED_ID,
    kind: "bundle",
    x: col0,
    y: y + 9,
    r: 9,
    title: `${TOTALS.unlinked} unlinked`,
    sub: "parent_session_id none",
    msg: "",
  });
  y += bundleBlock;

  return {
    nodes,
    edges,
    captions,
    pitch,
    height: y + padBot,
    cols: [
      { x: col0, gen: "GEN 0", role: "SOURCE" },
      { x: col1, gen: "GEN 1", role: "DELEGATES" },
    ],
  };
}

function Bars(props: { rows: { name: string; n: number; pct: number }[]; cap?: number }) {
  const rows = props.rows.slice(0, props.cap ?? 5);
  if (!rows.length) return <p className="ag-empty">none in snapshot</p>;
  return (
    <ul className="ag-bars">
      {rows.map((r) => (
        <li key={r.name}>
          <span>{r.name}</span>
          <b>{r.pct.toFixed(0)}%</b>
          <i>
            <em style={{ width: `${Math.max(2, Math.min(100, r.pct))}%` }} />
          </i>
        </li>
      ))}
    </ul>
  );
}

function HandoffTitle(props: { from: string | null; to: string; kick: string; tag?: string; genFrom?: string; genTo?: string }) {
  return (
    <>
      <div className="ag-kick">
        {props.kick}
        {props.tag ? <em>{props.tag}</em> : null}
      </div>
      <h2 className="ag-pair">
        {props.from ? (
          <>
            <span>{props.from}</span>
            <i aria-hidden="true">→</i>
            <span className="to">{props.to}</span>
          </>
        ) : (
          <span className="to">{props.to}</span>
        )}
      </h2>
      {props.genFrom || props.genTo ? (
        <div className="ag-genline">
          {props.genFrom ?? ""}
          {props.genFrom && props.genTo ? <i aria-hidden="true">→</i> : null}
          {props.genTo ?? ""}
        </div>
      ) : null}
    </>
  );
}

function Inspector(props: { sel: string | null }) {
  if (props.sel === UNLINKED_ID) {
    return (
      <aside className="ag-inspect" aria-label="Selected relation">
        <Corners />
        <HandoffTitle from={null} to="unlinked sources" kick="SELECTED BUNDLE" genFrom="GEN 0" genTo={undefined} />
        <div className="ag-block">
          <div className="k">SOURCE AUTHORITY</div>
          <div className="ag-row"><span>sessions</span><span className="r">{TOTALS.unlinked}</span></div>
          <div className="ag-row"><span>parent_session_id</span><span className="r">none</span></div>
          <div className="ag-row"><span>messages</span><span className="r">{UNLINKED.reduce((s, n) => s + n.session.messages, 0).toLocaleString()}</span></div>
        </div>
        <div className="ag-block">
          <div className="k">DELEGATION</div>
          <p className="ag-copy">No evidenced spawn in this snapshot. Layout does not invent a source session for these identities.</p>
        </div>
        <div className="ag-hint">topology from parent_session_id · generation from pack agents · no invented failure context</div>
      </aside>
    );
  }

  const ghost = props.sel?.startsWith(GHOST_PREFIX) ? props.sel.slice(GHOST_PREFIX.length) : null;
  if (ghost) {
    return (
      <aside className="ag-inspect" aria-label="Selected relation">
        <Corners />
        <HandoffTitle from={null} to={shortId(ghost, 14)} kick="SELECTED SOURCE" tag="NOT IN SNAPSHOT" genFrom="GEN 0" />
      <div className="ag-block">
        <div className="k">SOURCE AUTHORITY</div>
        <div className="ag-row"><span>relation</span><span className="r">parentage reference</span></div>
        <div className="ag-row"><span>basis</span><span className="r">child parent_session_id · EXACT</span></div>
        <div className="ag-row"><span>parent_session_id</span><span className="r">{shortId(ghost, 22)}</span></div>
          <div className="ag-row is-abs"><span>record</span><span className="r abs">unavailable</span></div>
        </div>
        <p className="ag-copy" style={{ marginTop: 12 }}>
          A child in this snapshot names this parent. The parent session was not copied into the pack.
        </p>
        <div className="ag-hint">topology from parent_session_id · generation from pack agents · no invented failure context</div>
      </aside>
    );
  }

  const n: AgentNode | null = props.sel ? nodeById(props.sel) : null;
  if (!n) {
    return (
      <aside className="ag-inspect" aria-label="Selected relation">
        <Corners />
        <HandoffTitle from={null} to="no selection" kick="EMPTY" />
        <p className="ag-copy">Pick a node in the topology.</p>
      </aside>
    );
  }

  const parent = n.session.parentId;
  return (
    <aside className="ag-inspect" aria-label="Selected relation">
      <Corners />
      {parent ? (
        <HandoffTitle
          from={shortId(parent, 12)}
          to={shortId(n.session.agentId ?? n.session.id, 12)}
          kick="RECORDED PARENT EDGE"
          tag={n.parentInSnapshot ? undefined : "PARENT MISSING"}
          genFrom={`GEN ${n.depth - 1}`}
          genTo={`GEN ${n.depth}`}
        />
      ) : (
        <HandoffTitle
          from={null}
          to={shortId(n.session.agentId ?? n.session.id, 16)}
          kick="SOURCE SESSION"
          genFrom={`GEN ${n.depth}`}
        />
      )}
      <div className="ag-block">
        <div className="k">SOURCE AUTHORITY</div>
        {parent ? <>
          <div className="ag-row"><span>relation</span><span className="r">parentage</span></div>
          <div className="ag-row"><span>basis</span><span className="r">parent_session_id · EXACT</span></div>
        </> : null}
        <div className="ag-row"><span>agent</span><span className="r" title={n.session.agentId ?? undefined}>{n.session.agentId ? shortId(n.session.agentId, 18) : "—"}</span></div>
        <div className="ag-row"><span>session</span><span className="r" title={n.session.id}>{shortId(n.session.id, 18)}</span></div>
        <div className="ag-row"><span>provider</span><span className="r">{n.session.provider}</span></div>
        <div className="ag-row">
          <span>delegated by</span>
          <span className="r">
            {parent
              ? `${shortId(parent, 16)}${n.parentInSnapshot ? "" : " · not in snapshot"}`
              : "none"}
          </span>
        </div>
      </div>
      <div className="ag-block">
        <div className="k">PROJECT SCOPE</div>
        <div className="ag-row"><span>project</span><span className="r">{n.session.project}</span></div>
        <div className="ag-row"><span>project id</span><span className="r">{shortId(n.session.projectId, 16)}</span></div>
        <div className="ag-row"><span>path</span><span className="r">{n.session.path ? shortId(n.session.path, 28) : "—"}</span></div>
      </div>
      <nav className="ag-pivots" aria-label="Selected agent destinations">
        <a href={loomSessionUrl(n.session.id)}>LOOM ↗</a>
        {(["sessions", "work", "code", "delivery"] as const).map((surface) => (
          <a key={surface} href={loomPivotUrl(surface, n.session.id)}>{surface.toUpperCase()} ↗</a>
        ))}
      </nav>
      <div className="ag-block">
        <div className="k">TOKEN FRONTIER</div>
        <div className="ag-row is-abs"><span>tokens</span><span className="r abs">unavailable in snapshot</span></div>
        <div className="ag-row is-abs"><span>budget</span><span className="r abs">unavailable</span></div>
      </div>
      <div className="ag-block">
        <div className="k">WORK PRODUCT</div>
        <div className="ag-row is-abs"><span>artifact</span><span className="r abs">unavailable in snapshot</span></div>
      </div>
      <div className="ag-block">
        <div className="k">FAILURE CONTEXT (if any)</div>
        <div className="ag-row is-abs"><span>status</span><span className="r abs">unavailable — not in pack</span></div>
        <div className="ag-row"><span>session status</span><span className="r">{n.session.status}</span></div>
        <p className="ag-copy">{n.session.coverage}. Bodies were not copied. Do not read that as a node failure.</p>
      </div>
      <div className="ag-hint">topology from parent_session_id · generation from pack agents · no invented failure context</div>
    </aside>
  );
}

function Node(props: { n: Laid; on: boolean; onPick: (id: string) => void }) {
  const { n } = props;
  const cls = [
    "ag-node",
    n.kind === "root" ? "is-root" : "",
    n.kind === "bundle" ? "is-bundle" : "",
    n.kind === "ghost" ? "is-ghost" : "",
    n.amber ? "is-amber" : "",
    props.on ? "is-on" : "",
  ]
    .filter(Boolean)
    .join(" ");
  const glowFill = n.amber || n.kind === "ghost" ? "url(#ag-glow-amber)" : "url(#ag-glow-cyan)";
  return (
    <g
      className={cls}
      transform={`translate(${n.x} ${n.y})`}
      role="button"
      tabIndex={0}
      aria-label={`Select ${n.title}`}
      aria-pressed={props.on}
      onClick={() => props.onPick(n.id)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          props.onPick(n.id);
        }
      }}
    >
      {n.kind !== "bundle" ? (
        <circle className="glow" r={n.r * (n.kind === "root" ? 2.9 : 2.4)} fill={glowFill} />
      ) : null}
      {n.kind === "root" ? <circle className="halo" r={n.r + 4.5} /> : null}
      <circle r={n.r} />
      {n.kind === "root" ? (
        <>
          <text className="name" textAnchor="middle" y={n.r + 15}>{n.title}</text>
          <text className="sub" textAnchor="middle" y={n.r + 26.5}>{n.sub}</text>
        </>
      ) : n.kind === "ghost" ? (
        <text className="name" textAnchor="middle" y={n.r + 13.5}>{n.title}</text>
      ) : n.kind === "bundle" ? (
        <text className="name" x={n.r + 8} y={3.5}>
          {n.title}
          <tspan className="dim" dx={7}>{n.sub}</tspan>
        </text>
      ) : (
        <text className="name" x={n.r + 8} y={3.5}>
          {n.title}
          <tspan className="msg" dx={7}>{n.msg}</tspan>
        </text>
      )}
    </g>
  );
}

function Field(props: { sel: string | null; onPick: (id: string) => void }) {
  const wrap = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ w: 800, h: 480 });
  useEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setBox({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setBox({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);
  const w = Math.max(320, box.w);
  const h = Math.max(240, box.h);
  const scene = useMemo(() => layout(w, h), [w, h]);
  const byId = useMemo(() => new Map(scene.nodes.map((n) => [n.id, n])), [scene]);
  const svgH = Math.max(scene.height, h);

  return (
    <div className="ag-field" ref={wrap}>
      <svg
        viewBox={`0 0 ${w} ${svgH}`}
        width={w}
        height={svgH}
        preserveAspectRatio="xMinYMin meet"
        role="img"
        aria-label="Delegation topology"
      >
        <defs>
          <radialGradient id="ag-glow-cyan">
            <stop offset="0%" stopColor="oklch(0.75 0.15 200)" stopOpacity="0.34" />
            <stop offset="55%" stopColor="oklch(0.75 0.15 200)" stopOpacity="0.1" />
            <stop offset="100%" stopColor="oklch(0.75 0.15 200)" stopOpacity="0" />
          </radialGradient>
          <radialGradient id="ag-glow-amber">
            <stop offset="0%" stopColor="oklch(0.78 0.14 85)" stopOpacity="0.3" />
            <stop offset="55%" stopColor="oklch(0.78 0.14 85)" stopOpacity="0.09" />
            <stop offset="100%" stopColor="oklch(0.78 0.14 85)" stopOpacity="0" />
          </radialGradient>
          <radialGradient id="ag-core-cyan">
            <stop offset="0%" stopColor="oklch(0.62 0.12 205)" stopOpacity="0.85" />
            <stop offset="45%" stopColor="oklch(0.32 0.07 230)" stopOpacity="0.9" />
            <stop offset="100%" stopColor="#071018" />
          </radialGradient>
        </defs>
        {scene.cols.map((c) => (
          <g key={c.gen} className="ag-col">
            <text className="ag-col-h" x={c.x} y={13} textAnchor="middle">{c.gen}</text>
            <text className="ag-col-sub" x={c.x} y={24.5} textAnchor="middle">{c.role}</text>
            <line className="ag-col-tick" x1={c.x - 14} x2={c.x + 14} y1={29} y2={29} />
            <line className="ag-col-guide" x1={c.x} x2={c.x} y1={34} y2={svgH - 6} />
          </g>
        ))}
        {scene.captions.map((c) => (
          <text key={`${c.text}-${c.y}`} className="ag-absent" x={c.x} y={c.y} textAnchor="start">
            {c.text}
          </text>
        ))}
        {scene.edges.map((e) => {
          const a = byId.get(e.a);
          const b = byId.get(e.b);
          if (!a || !b) return null;
          const d = bezier(a, b);
          return (
            <g
              key={`${e.a}-${e.b}`}
              className={`ag-link is-${e.kind}${e.b === props.sel ? " is-on" : ""}`}
              role="button"
              tabIndex={0}
              aria-label={`Select recorded parent edge from ${a.title} to ${b.title}`}
              onClick={() => props.onPick(e.b)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  props.onPick(e.b);
                }
              }}
            >
              <path className={e.missing ? "ag-edge is-ghost" : "ag-edge"} d={d} />
              <path className="ag-edge-hit" d={d} />
              {!e.missing ? <text className="ag-edge-grade" x={(a.x + b.x) / 2} y={(a.y + b.y) / 2 - 4}>EXACT · SPAWN</text> : null}
            </g>
          );
        })}
        {scene.nodes.map((n) => (
          <Node key={n.id} n={n} on={n.id === props.sel} onPick={props.onPick} />
        ))}
      </svg>
    </div>
  );
}

function TreeButton(props: { node: AgentNode; selected: boolean; onPick: (id: string) => void }) {
  const label = shortId(props.node.session.agentId ?? props.node.session.id, 18);
  return (
    <button
      type="button"
      className={props.selected ? "ag-tree-row is-on" : "ag-tree-row"}
      aria-pressed={props.selected}
      onClick={() => props.onPick(props.node.session.id)}
    >
      <span>{label}</span>
      <small>GEN {props.node.depth} · {props.node.session.messages.toLocaleString()} msg</small>
    </button>
  );
}

function TreeFallback(props: { sel: string | null; onPick: (id: string) => void }) {
  return (
    <div className="ag-tree" aria-label="Exact delegation tree">
      {FAMILIES.map((family) => (
        <section key={family.session.id}>
          <TreeButton node={family} selected={props.sel === family.session.id} onPick={props.onPick} />
          <ul>
            {family.children.map((child) => (
              <li key={child.session.id}>
                <TreeButton node={child} selected={props.sel === child.session.id} onPick={props.onPick} />
              </li>
            ))}
          </ul>
        </section>
      ))}
      {DANGLING.map((node) => (
        <section className="is-missing" key={node.session.id}>
          <div className="ag-tree-missing">PARENT NOT IN SNAPSHOT · {shortId(node.session.parentId ?? "", 16)}</div>
          <TreeButton node={node} selected={props.sel === node.session.id} onPick={props.onPick} />
        </section>
      ))}
      <section>
        <button
          type="button"
          className={props.sel === UNLINKED_ID ? "ag-tree-row is-on" : "ag-tree-row"}
          aria-pressed={props.sel === UNLINKED_ID}
          onClick={() => props.onPick(UNLINKED_ID)}
        >
          <span>{TOTALS.unlinked} unlinked sources</span>
          <small>GEN 0 · parent_session_id none</small>
        </button>
        <details>
          <summary>Show exact identities</summary>
          <ul>
            {UNLINKED.map((node) => (
              <li key={node.session.id}>
                <TreeButton node={node} selected={props.sel === node.session.id} onPick={props.onPick} />
              </li>
            ))}
          </ul>
        </details>
      </section>
    </div>
  );
}

function SnapshotAgentsPage(props: { initialSessionId?: string; onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { setMode } = useDemo();
  const initialSelection = props.initialSessionId && nodeById(props.initialSessionId) ? props.initialSessionId : defaultSelectedId();
  const [sel, setSel] = useWorkspaceState<string | null>("agents:selected", initialSelection);
  const [pane, setPane] = useWorkspaceState<"topology" | "details">("agents:pane", "topology");
  const [exact, setExact] = useWorkspaceState("agents:snapshot-exact", false);
  const [query, setQuery] = useWorkspaceState("agents:snapshot-query", "");
  const tape = RECENT_TOOLS.slice(0, 5);
  const matching = [...FAMILIES.flatMap((family) => [family, ...family.children]), ...DANGLING, ...UNLINKED].filter((node) => `${node.session.id} ${node.session.agentId ?? ""} ${node.session.project}`.toLowerCase().includes(query.toLowerCase()));

  useEffect(() => {
    if (props.initialSessionId && nodeById(props.initialSessionId)) setSel(props.initialSessionId);
  }, [props.initialSessionId]);

  const pick = (id: string) => {
    setSel(id);
    if (window.matchMedia("(max-width: 900px)").matches) setPane("details");
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!sel || sel === UNLINKED_ID || sel.startsWith(GHOST_PREFIX)) return;
      const n = nodeById(sel);
      if (!n) return;
      if (e.key === "ArrowRight" && n.children[0]) {
        e.preventDefault();
        setSel(n.children[0].session.id);
      } else if (e.key === "ArrowLeft" && n.session.parentId && n.parentInSnapshot) {
        e.preventDefault();
        setSel(n.session.parentId);
      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        const parent = n.session.parentId && n.parentInSnapshot ? nodeById(n.session.parentId) : null;
        const sibs = parent ? parent.children : FAMILIES;
        const ids = sibs.map((s) => s.session.id);
        const i = ids.indexOf(sel);
        if (i < 0) return;
        const next = e.key === "ArrowDown" ? ids[Math.min(ids.length - 1, i + 1)] : ids[Math.max(0, i - 1)];
        e.preventDefault();
        setSel(next);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sel]);

  return (
    <div className={`ag-root is-${pane}`}>
      <div className="ag-mobile-tabs" role="tablist" aria-label="Agents view">
        <button type="button" role="tab" aria-selected={pane === "topology"} onClick={() => setPane("topology")}>Topology</button>
        <button type="button" role="tab" aria-selected={pane === "details"} onClick={() => setPane("details")}>Selected relation</button>
      </div>
      <div className="ag-band">
          <div className="ag-chip">
            <div className="k">EVENT WINDOW (CAPPED)</div>
            <b>snapshot</b>
            <small>{CAPTURED_AT.slice(0, 10)} · not last-60-min live</small>
            <button className="ag-mode" type="button" onClick={() => setMode("fixture")}>USE DENSE FIXTURE</button>
          </div>
          <div className="ag-chip">
            <div className="k">EVENTS BY CATEGORY</div>
            <Bars rows={KIND_COUNTS} />
          </div>
          <div className="ag-chip">
            <div className="k">EVENTS BY TOOL</div>
            <Bars rows={TOOL_COUNTS} />
          </div>
          <div className="ag-chip">
            <div className="k">RECENT TOOL TAPE</div>
            {tape.length ? (
              <ul className="ag-tape">
                {tape.map((e) => (
                  <li key={e.id}>
                    <span className="t">{clock(e.at)}</span>
                    <span className="n">{e.tool}</span>
                    <span className="c">1</span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="ag-empty">no tool events in loom spine</p>
            )}
          </div>
          <div className="ag-chip">
            <div className="k">MANAGED AGENTS</div>
            <b>{TOTALS.agents}</b>
            <small>{TOTALS.agentRows} rows in snapshot</small>
          </div>
          <div className="ag-chip">
            <div className="k">INDEPENDENT AUTHORITY</div>
            <b>spine-only</b>
            <small>tokens 0/{TOTALS.sessions} · bodies not copied</small>
          </div>
      </div>
      <div className="ag-main">
        <section className="ag-topo" aria-label="Delegation topology">
          <div className="ag-topo-head">
            <b>DELEGATION TOPOLOGY <i>(read-only)</i></b>
            <span className="meta">
              {TOTALS.families} families · {PARENT_RESOLVED} resolved / {PARENT_MISSING} parent missing
            </span>
          </div>
          <div className="ag-topo-legend">
            left-to-right generations · size = messages · <i className="cy">solid = EXACT spawn / parent record</i> · <i className="am">dashed = UNAVAILABLE parent record</i> · handoff / rejoin: unavailable
            <span className="ag-risk-unavailable">issue / risk filter unavailable — failure authority was not captured. Token and downstream-work authorities remain unavailable; this topology does not infer them.</span>
          </div>
          <div className="ag-snapshot-controls"><button type="button" aria-pressed={exact} onClick={() => setExact((value) => !value)}>EXACT ROWS</button><button type="button" onClick={() => { setSel(defaultSelectedId()); setExact(false); setQuery(""); }}>FIT SELECTION</button><input type="search" aria-label="Search snapshot agents" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search agent, session, project" /></div>
          <Field sel={sel} onPick={pick} />
          <TreeFallback sel={sel} onPick={pick} />
          {exact || query ? <div className="ag-exact ag-snapshot-exact"><div className="ag-exact-head"><b>EXACT SNAPSHOT ROWS</b><span>{matching.length} / {TOTALS.sessions}</span></div><div className="ag-exact-table" role="table" aria-label="Exact snapshot agents">{matching.map((node) => <button type="button" role="row" key={node.session.id} className={node.session.id === sel ? "is-on" : ""} onClick={() => pick(node.session.id)}><span role="cell">{node.session.agentId ?? "agent unavailable"}</span><span role="cell">{node.session.id}</span><span role="cell">GEN {node.depth}</span><span role="cell">{node.session.messages.toLocaleString()} msg</span><span role="cell">{node.parentInSnapshot ? "EXACT spawn" : node.session.parentId ? "parent unavailable" : "source"}</span><span role="cell">{node.session.project}</span></button>)}</div></div> : null}
        </section>
      </div>
      <Inspector sel={sel} />
    </div>
  );
}

export function AgentsPage(props: { initialSessionId?: string; onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { mode } = useDemo();
  return mode === "fixture" ? <DenseAgents /> : <SnapshotAgentsPage {...props} />;
}
