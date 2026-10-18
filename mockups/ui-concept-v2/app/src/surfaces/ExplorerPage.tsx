import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { PACK, shortId } from "../data/pack";
import sparksRaw from "../data/session-sparks.json";
import { RepositoryAtlas, atlasData, nodeById, type AtlasNode } from "../structure";
import { Badge, Panel } from "./ui";
import type { SurfaceInspect } from "./inspect";

const CARD_COUNT = 5;

type SparkKind = "series" | "collapsed" | "empty";
type SparkRow = { v: number[]; uniqueTs: number; kind: SparkKind };
type SparkFile = { buckets: number; source: string; rows: Record<string, SparkRow> };

const SPARKS = sparksRaw as SparkFile;

const CAPTURED_MS = Date.parse(PACK.capturedAt);

const ORDERED_SESSIONS = [...PACK.sessions].sort(
  (a, b) => (b.startedTs ?? 0) - (a.startedTs ?? 0),
);
const PROVIDERS = [...new Set(ORDERED_SESSIONS.map((s) => s.provider))].sort();

function ago(ts: number | null): string {
  if (ts == null || !Number.isFinite(CAPTURED_MS)) return "—";
  const sec = Math.max(0, Math.floor(CAPTURED_MS / 1000 - ts));
  if (sec < 60) return "just now";
  if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}h ago`;
  if (sec < 86400 * 7) return `${Math.floor(sec / 86400)}d ago`;
  if (sec < 86400 * 30) return `${Math.floor(sec / 86400 / 7)}w ago`;
  return `${Math.floor(sec / 86400 / 30)}mo ago`;
}

function span(start: number | null, end: number | null): string {
  if (start == null || end == null) return "—";
  const sec = Math.max(0, end - start);
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m`;
  return `${Math.floor(sec / 3600)}h ${Math.floor((sec % 3600) / 60)}m`;
}

function Sparkline(props: { values: number[]; kind: SparkKind }) {
  const w = 120;
  const h = 30;
  const vals = props.values.length ? props.values : [0];
  const max = Math.max(1, ...vals);
  const step = w / vals.length;
  const pts: string[] = [];
  const bars: { x: number; y: number }[] = [];
  vals.forEach((v, i) => {
    const x = i * step + step / 2;
    const y = h - 1.5 - (v / max) * (h - 4);
    pts.push(`${x.toFixed(2)},${y.toFixed(2)}`);
    bars.push({ x, y });
  });
  const line = pts.join(" ");
  const area = `${(step / 2).toFixed(2)},${h} ${line} ${(w - step / 2).toFixed(2)},${h}`;
  const quiet = props.kind !== "series";
  return (
    <svg className={quiet ? "sess-spark is-quiet" : "sess-spark"} viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" aria-hidden="true">
      {bars.map((b, i) => (
        <line key={i} className="spk-bar" x1={b.x} y1={h - 0.5} x2={b.x} y2={b.y} />
      ))}
      <polygon points={area} />
      <polyline points={line} />
    </svg>
  );
}

type LaneKind = "CODE" | "SESSIONS" | "KNOWLEDGE" | "SEMANTIC";

function LaneIco(props: { kind: LaneKind }) {
  if (props.kind === "CODE") {
    return (
      <svg className="lane-ico" viewBox="0 0 16 16" aria-hidden="true">
        <rect x="3.5" y="2.5" width="9" height="11" rx="0.8" fill="none" stroke="currentColor" strokeWidth="1.15" />
        <path d="M6 5.5h4M6 8h4M6 10.5h2.5" fill="none" stroke="currentColor" strokeWidth="1.05" />
      </svg>
    );
  }
  if (props.kind === "SESSIONS") {
    return (
      <svg className="lane-ico" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M1.6 8h2.2l1.4-3.6 2 7.2 1.8-5.4 1.2 1.8h4.2" fill="none" stroke="currentColor" strokeWidth="1.15" strokeLinejoin="round" strokeLinecap="round" />
      </svg>
    );
  }
  if (props.kind === "KNOWLEDGE") {
    return (
      <svg className="lane-ico" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M8 1.9 13.3 5v6L8 14.1 2.7 11V5Z" fill="none" stroke="currentColor" strokeWidth="1.15" />
        <circle cx="8" cy="8" r="1.7" fill="none" stroke="currentColor" strokeWidth="1" />
      </svg>
    );
  }
  return (
    <svg className="lane-ico" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M8 1.6v3.2M8 11.2v3.2M1.6 8h3.2M11.2 8h3.2M3.5 3.5l2.2 2.2M10.3 10.3l2.2 2.2M12.5 3.5l-2.2 2.2M5.7 10.3l-2.2 2.2" fill="none" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
      <circle cx="8" cy="8" r="1.1" fill="currentColor" />
    </svg>
  );
}

function hash32(n: number) {
  let h = n >>> 0;
  h = Math.imul(h ^ (h >>> 16), 2246822507);
  h = Math.imul(h ^ (h >>> 13), 3266489909);
  return (h ^ (h >>> 16)) >>> 0;
}

function rnd(seed: number, i: number) {
  return hash32(seed ^ Math.imul(i + 1, 2654435761)) / 4294967296;
}

type Star = { x: number; y: number; r: number; o: number; hot: boolean };

/** Dense violet particle/constellation field for the SEMANTIC lane. */
function StarField() {
  const { stars, lines, cores } = useMemo(() => {
    const pts: Star[] = [];
    const clusters = [
      { cx: 30, cy: 32, R: 21, n: 42 },
      { cx: 72, cy: 52, R: 17, n: 34 },
      { cx: 50, cy: 84, R: 14, n: 26 },
      { cx: 34, cy: 118, R: 19, n: 38 },
      { cx: 72, cy: 132, R: 17, n: 34 },
    ];
    const clusterIdx: number[][] = [];
    clusters.forEach((c, ci) => {
      const mine: number[] = [];
      for (let i = 0; i < c.n; i++) {
        const seed = 0x5eed0 + ci * 977;
        const a = rnd(seed, i * 2) * Math.PI * 2;
        const d = Math.pow(rnd(seed, i * 2 + 1), 0.72) * c.R;
        const rr = rnd(seed + 7, i);
        mine.push(pts.length);
        pts.push({
          x: c.cx + Math.cos(a) * d,
          y: c.cy + Math.sin(a) * d * 1.08,
          r: rr > 0.955 ? 0.5 + rr * 0.22 : 0.2 + rr * 0.3,
          o: 0.3 + rnd(seed + 13, i) * 0.55,
          hot: rr > 0.955,
        });
      }
      clusterIdx.push(mine);
    });
    for (let i = 0; i < 190; i++) {
      const s = 0xd05e;
      pts.push({
        x: 2 + rnd(s, i * 3) * 96,
        y: 2 + rnd(s, i * 3 + 1) * 156,
        r: 0.14 + rnd(s, i * 3 + 2) * 0.26,
        o: 0.12 + rnd(s + 3, i) * 0.34,
        hot: false,
      });
    }
    const segs: [number, number][] = [];
    for (const mine of clusterIdx) {
      for (const i of mine) {
        let best = -1;
        let bestD = 8.5;
        let second = -1;
        let secondD = 7;
        for (const j of mine) {
          if (i === j) continue;
          const d = Math.hypot(pts[i].x - pts[j].x, pts[i].y - pts[j].y);
          if (d < bestD) {
            second = best;
            secondD = bestD;
            best = j;
            bestD = d;
          } else if (d < secondD) {
            second = j;
            secondD = d;
          }
        }
        if (best >= 0 && i < best) segs.push([i, best]);
        if (second >= 0 && i < second) segs.push([i, second]);
      }
    }
    return { stars: pts, lines: segs, cores: clusters };
  }, []);

  return (
    <div className="sem-field">
      <svg className="sem-constellation" viewBox="0 0 100 160" preserveAspectRatio="xMidYMid slice" aria-hidden="true">
        <defs>
          <radialGradient id="sem-neb" cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor="rgba(126,84,214,0.30)" />
            <stop offset="55%" stopColor="rgba(70,40,140,0.12)" />
            <stop offset="100%" stopColor="rgba(10,8,24,0)" />
          </radialGradient>
        </defs>
        <rect width="100" height="160" fill="rgba(10,8,22,0.55)" />
        {cores.map((c, i) => (
          <ellipse key={`n${i}`} cx={c.cx} cy={c.cy} rx={c.R * 1.5} ry={c.R * 1.35} fill="url(#sem-neb)" />
        ))}
        {lines.map(([a, b], i) => (
          <line
            key={`l${i}`}
            x1={stars[a].x}
            y1={stars[a].y}
            x2={stars[b].x}
            y2={stars[b].y}
            stroke="rgba(176,132,255,0.22)"
            strokeWidth="0.22"
          />
        ))}
        {stars.map((s, i) =>
          s.hot ? (
            <g key={i}>
              <circle cx={s.x} cy={s.y} r={s.r * 2.4} fill="rgba(178,140,255,0.11)" />
              <circle cx={s.x} cy={s.y} r={s.r} fill="rgba(236,226,255,0.92)" />
            </g>
          ) : (
            <circle key={i} cx={s.x} cy={s.y} r={s.r} fill={`rgba(214,190,255,${s.o.toFixed(2)})`} />
          ),
        )}
      </svg>
      <div className="sem-copy">
        <span>Indexing semantic space</span>
        <small>retrieval_anchors = 0</small>
        <svg className="sem-spinner" viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeWidth="1.4" strokeDasharray="3.4 3.4" strokeLinecap="round" />
        </svg>
      </div>
    </div>
  );
}

/** Faint decorative particle dust behind ghost cards (plate night-glass grain). */
function DustField(props: { seed: number }) {
  const dots = useMemo(() => {
    const out: { x: number; y: number; r: number; o: number }[] = [];
    for (let i = 0; i < 64; i++) {
      out.push({
        x: 2 + rnd(props.seed, i * 3) * 96,
        y: 2 + rnd(props.seed, i * 3 + 1) * 156,
        r: 0.16 + rnd(props.seed, i * 3 + 2) * 0.4,
        o: 0.1 + rnd(props.seed + 5, i) * 0.4,
      });
    }
    const segs: [number, number][] = [];
    for (let i = 0; i < out.length; i++) {
      for (let j = i + 1; j < out.length; j++) {
        if (Math.hypot(out[i].x - out[j].x, out[i].y - out[j].y) < 8) segs.push([i, j]);
      }
    }
    return { out, segs };
  }, [props.seed]);
  return (
    <svg className="lane-dust" viewBox="0 0 100 160" preserveAspectRatio="xMidYMid slice" aria-hidden="true">
      {dots.segs.map(([a, b], i) => (
        <line key={`s${i}`} x1={dots.out[a].x} y1={dots.out[a].y} x2={dots.out[b].x} y2={dots.out[b].y} stroke="currentColor" strokeWidth="0.2" opacity="0.22" />
      ))}
      {dots.out.map((d, i) => (
        <circle key={i} cx={d.x} cy={d.y} r={d.r} fill="currentColor" opacity={d.o.toFixed(2)} />
      ))}
    </svg>
  );
}

const GHOST_W: Record<"code" | "know", { t: number; p: number }[]> = {
  code: [
    { t: 62, p: 78 },
    { t: 70, p: 84 },
    { t: 56, p: 72 },
    { t: 66, p: 80 },
    { t: 58, p: 74 },
  ],
  know: [
    { t: 58, p: 70 },
    { t: 50, p: 64 },
    { t: 64, p: 76 },
    { t: 55, p: 68 },
    { t: 60, p: 72 },
  ],
};

function GhostCards(props: { lane: "code" | "know" }) {
  return (
    <ul className="lane-list ghost-cards" aria-hidden="true">
      {GHOST_W[props.lane].map((w, i) => (
        <li key={i}>
          <div className="ghost-card">
            <span className="gbar g-title" style={{ width: `${w.t}%` }} />
            {props.lane === "code" ? <span className="ghost-tag">—</span> : null}
            <span className="gbar g-path" style={{ width: `${w.p}%` }} />
            <span className="ghost-meta">
              ◉ —%<i />—{props.lane === "code" ? <><i />— LOC</> : null}
            </span>
          </div>
        </li>
      ))}
    </ul>
  );
}

function LaneAbsence(props: { title: string; detail: string }) {
  return (
    <div className="lane-absence">
      <Badge state="unavailable" />
      <b>{props.title}</b>
      <small>{props.detail}</small>
    </div>
  );
}

function Filter(props: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
}) {
  return (
    <label className="ex-filter">
      <span className="ex-filter-lab">{props.label}</span>
      <select
        className="ex-filter-box"
        value={props.value}
        onChange={(event) => props.onChange(event.target.value)}
        aria-label={`${props.label} filter`}
      >
        {props.options.map((option) => (
          <option key={option.value} value={option.value}>{option.label}</option>
        ))}
      </select>
      <svg className="ex-filter-chevron" viewBox="0 0 8 5" aria-hidden="true">
        <path d="M1 1l3 3 3-3" fill="none" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
      </svg>
    </label>
  );
}

function sessionInspector(
  session: (typeof PACK.sessions)[number],
  rank: number,
  shown: number,
): SurfaceInspect {
  const spark = SPARKS.rows[session.id];
  return {
    title: shortId(session.id, 18),
    kind: "SESSION IDENTITY",
    id: session.id,
    sections: [
      { k: "SOURCE IDENTITY", rows: [
        { l: "project", r: session.project },
        { l: "provider", r: session.provider },
        { l: "messages", r: String(session.messages) },
        { l: "coverage", r: session.coverage },
        { l: "tokens", r: "unknown" },
      ]},
      { k: "QUERY LANE", rows: [
        { l: "lane", r: "SESSIONS · ready" },
        { l: "position", r: rank >= 0 ? `${rank + 1} of ${shown} shown` : "—" },
        { l: "ordering", r: "captured recency" },
      ]},
      { k: "PROVENANCE", rows: [
        { l: "started", r: session.startedAt ?? "—" },
        { l: "ended", r: session.endedAt ?? "—" },
        { l: "parent", r: session.parentId ? shortId(session.parentId) : "—" },
        { l: "sparkline", r: spark?.kind === "series" ? "spine timestamps" : spark?.kind === "collapsed" ? "collapsed timestamp" : "no timestamps" },
      ]},
    ],
    hint: "exact session identity from pack\ntranscript bodies unavailable",
  };
}

export function ExplorerPage(props: { onInspect: (i: SurfaceInspect) => void; query?: string }) {
  const { navigate } = useDemo();
  const [selected, setSelected] = useState<string | null>(ORDERED_SESSIONS[0]?.id ?? null);
  const [atlasSelection, setAtlasSelection] = useWorkspaceState<string>("atlas.selection", "crates/tracedecay");
  const explicitNode = new URLSearchParams(window.location.search).get("node")
    || new URLSearchParams(window.location.search).get("path");
  const [lane, setLane] = useState("all");
  const [sessionPage, setSessionPage] = useState(0);
  const [provider, setProvider] = useState("all");
  const [kind, setKind] = useState("all");
  const [time, setTime] = useState("any");
  const [size, setSize] = useState("any");

  const matchingSessions = useMemo(() => ORDERED_SESSIONS.filter((session) => {
    if (props.query && ![session.id,session.provider,session.project].join(' ').toLowerCase().includes(props.query.toLowerCase())) return false;
    if (provider !== "all" && session.provider !== provider) return false;
    const age = session.startedTs == null ? Infinity : CAPTURED_MS / 1000 - session.startedTs;
    if (time === "day" && age > 86400) return false;
    if (time === "week" && age > 86400 * 7) return false;
    if (time === "month" && age > 86400 * 30) return false;
    if (size === "small" && session.messages >= 10) return false;
    if (size === "medium" && (session.messages < 10 || session.messages >= 50)) return false;
    if (size === "large" && session.messages < 50) return false;
    return kind === "all" || kind === "session";
  }), [kind, provider, size, time, props.query]);
  const visibleSessions = useMemo(
    () => matchingSessions.slice(sessionPage * CARD_COUNT, (sessionPage + 1) * CARD_COUNT),
    [matchingSessions, sessionPage],
  );

  useEffect(() => { setSessionPage(0); }, [matchingSessions]);

  useEffect(() => {
    if (explicitNode && nodeById.has(explicitNode)) setAtlasSelection(explicitNode);
  }, [explicitNode, setAtlasSelection]);

  useEffect(() => {
    if (visibleSessions.some((session) => session.id === selected)) return;
    setSelected(visibleSessions[0]?.id ?? null);
  }, [selected, visibleSessions]);

  // Deferred one tick: the shell clears its inspector on surface change in a
  // parent effect that runs after ours, which would wipe a synchronous default
  // selection dispatched during the same commit.
  useEffect(() => {
    if (!selected) {
      props.onInspect({
        title: "No matching session",
        kind: "FILTERED RESULT",
        sections: [{
          k: "SESSIONS · READY",
          rows: [
            { l: "result", r: "served empty" },
            { l: "filters", r: "no matching source records" },
          ],
        }],
        hint: "The Sessions authority is ready; these filters returned no records.",
      });
      return;
    }
    const rank = visibleSessions.findIndex((x) => x.id === selected);
    const s = PACK.sessions.find((x) => x.id === selected);
    if (!s) return;
    const spec = sessionInspector(s, rank, visibleSessions.length);
    const timer = window.setTimeout(() => props.onInspect(spec), 0);
    return () => window.clearTimeout(timer);
  }, [props.onInspect, selected, visibleSessions]);

  function resetFilters() {
    setLane("all");
    setProvider("all");
    setKind("all");
    setTime("any");
    setSize("any");
  }

  function selectStructure(node: AtlasNode) {
    setAtlasSelection(node.id);
    const change = atlasData.changes.find((item) => item.path === node.path);
    props.onInspect({
      title: node.path || "tracedecay repository",
      kind: "EXTRACTED REPOSITORY STRUCTURE",
      id: node.id,
      sections: [
        { k: "STABLE FOOTPRINT", rows: [
          { l: "kind", r: node.kind },
          { l: "files", r: String(node.files) },
          { l: "bytes", r: node.bytes.toLocaleString() },
          { l: "focused change", r: change?.status ?? "not touched" },
        ] },
        { k: "SOURCE", rows: [
          { l: "revision", r: atlasData.revision.slice(0, 12) },
          { l: "repository", r: atlasData.repository },
        ] },
        { k: "QUERY AUTHORITY", rows: [
          { l: "path containment", r: "extracted snapshot" },
          { l: "symbol search", r: "unavailable" },
          { l: "call graph", r: "unavailable" },
        ] },
      ],
      hint: "Structure is source-backed and stable. Explorer query and source filters still apply only to the ready Sessions lane.",
    });
  }

  function traverseSessions(event: KeyboardEvent<HTMLUListElement>) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const cards = [...event.currentTarget.querySelectorAll<HTMLButtonElement>(".sess-card")];
    if (!cards.length) return;
    const current = cards.indexOf(document.activeElement as HTMLButtonElement);
    const delta = event.key === "ArrowDown" ? 1 : -1;
    cards[(current + delta + cards.length) % cards.length].focus();
    event.preventDefault();
  }

  return (
    <div className="surf explorer">
      <div className="ex-filters">
        <Filter label="LANES" value={lane} onChange={setLane} options={[
          { value: "all", label: "All 4" },
          { value: "code", label: "Code" },
          { value: "sessions", label: "Sessions" },
          { value: "knowledge", label: "Knowledge" },
          { value: "semantic", label: "Semantic" },
        ]} />
        <Filter label="SESSION SOURCE" value={provider} onChange={setProvider} options={[
          { value: "all", label: "All" },
          ...PROVIDERS.map((value) => ({ value, label: value })),
        ]} />
        <Filter label="KIND" value={kind} onChange={setKind} options={[
          { value: "all", label: "All" },
          { value: "session", label: "Session" },
        ]} />
        <Filter label="TIME" value={time} onChange={setTime} options={[
          { value: "any", label: "Any time" },
          { value: "day", label: "Last 24h" },
          { value: "week", label: "Last 7d" },
          { value: "month", label: "Last 30d" },
        ]} />
        <Filter label="SIZE" value={size} onChange={setSize} options={[
          { value: "any", label: "Any" },
          { value: "small", label: "<10 msg" },
          { value: "medium", label: "10–49 msg" },
          { value: "large", label: "50+ msg" },
        ]} />
        <button type="button" className="ex-clear" onClick={resetFilters}>
          Clear
        </button>
      </div>
      <div className={lane === "all" ? "explorer-lanes" : "explorer-lanes is-single"}>
        {lane === "all" || lane === "code" ? <Panel corners={false} className="lane lane-code">
          <header className="ex-head">
            <div>
              <b>CODE</b>
              <span className="ex-state">STRUCTURE SNAPSHOT</span>
            </div>
            <LaneIco kind="CODE" />
          </header>
          <div className="ex-metric">
            <span>EXTRACTED PATHS</span>
            <em>{atlasData.nodes.length.toLocaleString()}</em>
          </div>
          <div className="ex-body ex-atlas-body">
            <RepositoryAtlas
              context="explorer"
              compact={lane === "all"}
              initialSelection={explicitNode && nodeById.has(explicitNode) ? explicitNode : nodeById.has(atlasSelection) ? atlasSelection : "crates/tracedecay"}
              onSelect={selectStructure}
            />
            <div className="ex-atlas-status">
              <span>symbol and call search unavailable</span>
              <button type="button" onClick={() => navigate("code", { node: atlasSelection, path: atlasSelection })}>OPEN CODE ↗</button>
            </div>
          </div>
          <footer className="ex-foot">
            <span>Git {atlasData.revision.slice(0, 8)}</span>
            <span>stable containment</span>
          </footer>
        </Panel> : null}
        {lane === "all" || lane === "sessions" ? <Panel corners={false} className="lane lane-sess">
          <header className="ex-head">
            <div>
              <b>SESSIONS</b>
              <span className="ex-state">READY</span>
            </div>
            <LaneIco kind="SESSIONS" />
          </header>
          <div className="ex-metric">
            <span>MATCHING SESSIONS</span>
            <em>{matchingSessions.length}</em>
          </div>
          <ul className="ex-body lane-list sess-cards" onKeyDown={traverseSessions}>
            {visibleSessions.map((s, i) => {
              const spark = SPARKS.rows[s.id];
              const kind = spark?.kind ?? "empty";
              const values = spark?.v ?? [0, 0, 0, 0];
              return (
                <li key={s.id}>
                  <button
                    type="button"
                    className={s.id === selected ? "sess-card is-on" : "sess-card"}
                    onClick={() => {
                      if (s.id === selected) props.onInspect(sessionInspector(s, i, visibleSessions.length));
                      else setSelected(s.id);
                    }}
                  >
                    <span className="row-title">{s.project} · {s.provider}</span>
                    <span className="row-ago">{ago(s.endedTs ?? s.startedTs)}</span>
                    <span className="row-sub">sess:{shortId(s.id, 14)}</span>
                    <span className="row-span">{span(s.startedTs, s.endedTs)}</span>
                    <Sparkline values={values} kind={kind} />
                    <span className="row-rel">#{sessionPage * CARD_COUNT + i + 1}</span>
                  </button>
                </li>
              );
            })}
            {visibleSessions.length === 0 ? (
              <li className="sess-empty"><span>NO MATCHING SESSIONS</span><small>ready authority · served empty</small></li>
            ) : null}
          </ul>
          <footer className="ex-foot" style={{ flexWrap: "wrap", gap: 6 }}>
            <span title="Newest captured first">{matchingSessions.length ? sessionPage * CARD_COUNT + 1 : 0}–{Math.min((sessionPage + 1) * CARD_COUNT, matchingSessions.length)} of {matchingSessions.length}</span>
            <button type="button" className="ex-clear" aria-label="Previous sessions" disabled={sessionPage === 0} onClick={() => setSessionPage(page => page - 1)}>Prev</button>
            <button type="button" className="ex-clear" aria-label="Next sessions" disabled={(sessionPage + 1) * CARD_COUNT >= matchingSessions.length} onClick={() => setSessionPage(page => page + 1)}>Next</button>
            <button type="button" className="ex-clear" disabled={!selected} onClick={() => { if (selected) navigate("sessions", { session: selected }); }}>Open session ↗</button>
          </footer>
        </Panel> : null}
        {lane === "all" || lane === "knowledge" ? <Panel corners={false} className="lane lane-know">
          <header className="ex-head">
            <div>
              <b>KNOWLEDGE</b>
              <span className="ex-state">UNAVAILABLE</span>
            </div>
            <LaneIco kind="KNOWLEDGE" />
          </header>
          <div className="ex-metric">
            <span>RELEVANT CONCEPTS</span>
            <em>—</em>
          </div>
          <div className="ex-body lane-ghost-body">
            <DustField seed={0xa3be} />
            <GhostCards lane="know" />
            <LaneAbsence
              title="facts table absent"
              detail="Knowledge cannot inherit Sessions readiness. No memory is invented for this query."
            />
          </div>
          <footer className="ex-foot">
            <span>— · not served</span>
            <span>facts absent</span>
          </footer>
        </Panel> : null}
        {lane === "all" || lane === "semantic" ? <Panel corners={false} className="lane lane-sem">
          <header className="ex-head">
            <div>
              <b>SEMANTIC</b>
              <span className="ex-state">UNAVAILABLE</span>
            </div>
            <LaneIco kind="SEMANTIC" />
          </header>
          <div className="ex-metric">
            <span>SEMANTIC MATCHES</span>
            <em>—</em>
          </div>
          <div className="ex-body lane-ghost-body">
            <LaneAbsence
              title="semantic projection unavailable"
              detail="No geometry or semantic index is served. The stable repository footprint lives in Code and does not imply symbol or memory topology."
            />
          </div>
          <footer className="ex-foot">
            <span>—</span>
            <span>not served</span>
          </footer>
        </Panel> : null}
      </div>
    </div>
  );
}
