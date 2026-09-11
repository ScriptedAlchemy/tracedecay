import { useId, useRef, useState } from "react";
import { useWorkspaceState } from "../app/workspace";
import {
  AGENT_FILTERS,
  CHECK_MATRIX,
  DELIVERY_FIXTURE_PRS,
  GLOBAL_PRS,
  LOCAL_REPOS,
  PIPELINE,
  PROVIDER_OUTCOMES,
  REVIEW_FINDINGS,
  STATUS_FILTERS,
  UNKNOWN_DIRS,
  type DeliveryFixturePr,
  type HonestInbox,
  type StatusTone,
} from "./data";
import { ProjectConstellation } from "./constellation";
import { HonestMark } from "./marks";

const REPO_COLOR: Record<string, string> = {
  rspack: "#38cfe8",
  rslib: "#f0b429",
  rsbuild: "#f0b429",
  rspress: "#c084fc",
  lynx: "#60a5fa",
  "module-federation": "#9be15d",
  "trace-decay": "#5ee7ff",
  tracedecay: "#5ee7ff",
};

function repoColor(repo: string): string {
  const key = repo.split("/")[0].trim();
  return REPO_COLOR[key] ?? "#8b99a8";
}

function toneDot(tone: StatusTone): string {
  return tone === "danger"
    ? "var(--state-danger)"
    : tone === "amber"
      ? "var(--activity-amber)"
      : tone === "violet"
        ? "var(--state-violet)"
        : tone === "ready"
          ? "var(--state-ready)"
          : tone === "live"
            ? "var(--signal-cyan)"
            : "#8b99a8";
}

function CiMark(props: { ci: string }) {
  if (props.ci === "passed") return <span style={{ color: "var(--state-ready)" }}>✓</span>;
  if (props.ci === "failed") return <span style={{ color: "var(--state-danger)" }}>●</span>;
  if (props.ci === "partial") return <span style={{ color: "var(--activity-amber)" }}>●</span>;
  return <span style={{ color: "var(--state-violet)" }}>▲</span>;
}

function FilterBox(props: { k: string; onSelect?: (label: string) => void; rows: { label: string; count: string; tone?: StatusTone; pct?: number }[] }) {
  return (
    <div className="dl-fbox">
      <div className="fk">
        {props.k} <span>⌃</span>
      </div>
      {props.rows.map((r) => (
        <div className="fr" key={r.label} role={props.onSelect ? "button" : undefined} tabIndex={props.onSelect ? 0 : undefined} onClick={() => props.onSelect?.(r.label)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onSelect?.(r.label); } }}>
          <span className="fl">
            <i className="mark" style={{ background: toneDot(r.tone ?? "quiet") }} />
            {r.label}
          </span>
          <b>{r.count}</b>
          {r.pct != null ? (
            <span className="fbar">
              <i style={{ width: `${r.pct}%`, background: toneDot(r.tone ?? "quiet") }} />
            </span>
          ) : null}
        </div>
      ))}
    </div>
  );
}

function PrTable(props: { rows: typeof GLOBAL_PRS; total: string; page?: string; compact?: boolean }) {
  const detailPrefix = useId();
  return (
    <div className="dl-fbox" style={{ marginTop: 8 }}>
      <div className="fk">
        PR LIST (LOADED SNAPSHOT) <span>Sort: Recent activity ⌄</span>
      </div>
      <div className={props.compact ? "dl-prtable is-compact" : "dl-prtable"}>
        <div className="row head">
          <span>PR / Title</span>
          <span>Author</span>
          <span>Chg</span>
          <span>Cov</span>
          <span>CI</span>
          <span>Evid</span>
          <span>Fresh</span>
        </div>
        {props.rows.map((p) => (
          <div className="row" key={p.id}>
            <span className="prcell">
              <em style={{ color: repoColor(p.repo) }}>{p.repo}</em>
              <button className="t dl-textbutton" popoverTarget={`${detailPrefix}-pr-detail-${p.id}`}>{p.id} {p.title}</button>
              <div popover="auto" id={`${detailPrefix}-pr-detail-${p.id}`} className="dl-evidence-popover">
                <button popoverTarget={`${detailPrefix}-pr-detail-${p.id}`} popoverTargetAction="hide" aria-label="Close PR details">×</button>
                <h4>{p.id} {p.title}</h4><p>{p.repo} · {p.author}</p>
                <p>{p.chg} files · review {p.cov} · evidence {p.evid} · CI {p.ci} · {p.fresh}</p>
                <p>Attention sources: {p.attention.join(", ") || "none in loaded snapshot"}. Provider: {p.provider}. Correlation source: not attached to this example.</p>
                {p.id === "#707" ? <a className="dl-btn" href="?data=fixture&surface=delivery&state=04&pr=707&umbrella=v2">Open #707 journey example →</a> : <p>Full journey source is not loaded for this PR. The example journey for #707 is separately available.</p>}
              </div>
            </span>
            <span className="mono">{p.author}</span>
            <span className="mono">{p.chg}</span>
            <span className="mono">{p.cov}</span>
            <span className="mono">
              <CiMark ci={p.ci} />
            </span>
            <span className="mono">{p.evid}</span>
            <span className="mono">{p.fresh}</span>
          </div>
        ))}
      </div>
      <div className="dl-pager">
        <span>{props.rows.length} of {props.total} loaded PRs</span>
        <span className="pages">Local snapshot</span>
      </div>
    </div>
  );
}

function InboxFilters(props: { scoped?: boolean; query: string; onQuery: (query: string) => void; onFilter: (label: string) => void }) {
  if (props.scoped) {
    return (
      <>
        <input className="dl-search" placeholder="Search tracedecay PRs…" value={props.query} onChange={(event) => props.onQuery(event.target.value)} />
        <div className="dl-fbox">
          <div className="fk">FILTERS (5)</div>
          {[
            { label: "Unresolved", count: "612", tone: "danger" as StatusTone, pct: 100, on: false },
            { label: "High risk", count: "78", tone: "amber" as StatusTone, pct: 22, on: true },
            { label: "Weak evidence", count: "96", tone: "violet" as StatusTone, pct: 27 },
            { label: "Unreviewed", count: "312", tone: "live" as StatusTone, pct: 55 },
            { label: "Stale", count: "143", tone: "quiet" as StatusTone, pct: 24 },
          ].map((r) => (
            <div className={r.on ? "fr is-on" : "fr"} key={r.label} role="button" tabIndex={0} onClick={() => props.onFilter(r.label)} onKeyDown={(event) => { if (event.key === "Enter") props.onFilter(r.label); }}>
              <span className="fl">
                <i className="mark" style={{ background: toneDot(r.tone) }} />
                {r.label}
              </span>
              <b>{r.count}</b>
              <span className="fbar">
                <i style={{ width: `${r.pct}%`, background: toneDot(r.tone) }} />
              </span>
            </div>
          ))}
        </div>
        <FilterBox onSelect={props.onFilter}
          k="AGENTS (PROJECT)"
          rows={[
            { label: "claude-code", count: "324" },
            { label: "gemini-cli", count: "178" },
            { label: "codegpt", count: "64" },
            { label: "human", count: "46" },
            { label: "other", count: "0" },
          ]}
        />
        <FilterBox onSelect={props.onFilter}
          k="WORKTREES"
          rows={[
            { label: "main", count: "324" },
            { label: "core", count: "178" },
            { label: "scheduler", count: "96" },
            { label: "remote-retry", count: "64" },
            { label: "docs", count: "38" },
            { label: "other", count: "109" },
          ]}
        />
      </>
    );
  }
  return (
    <>
      <input className="dl-search" placeholder="Search PRs across all projects…" value={props.query} onChange={(event) => props.onQuery(event.target.value)} />
      <div className="dl-fcols">
        <FilterBox onSelect={props.onFilter} k="STATUS" rows={STATUS_FILTERS} />
        <FilterBox onSelect={props.onFilter}
          k="AGENT / AUTHOR (ALL)"
          rows={AGENT_FILTERS.map((a, i) => ({ ...a, tone: (["live", "amber", "violet", "ready", "quiet"] as StatusTone[])[i] }))}
        />
      </div>
      <FilterBox onSelect={props.onFilter} k="PROVIDER OUTCOME (ALL)" rows={PROVIDER_OUTCOMES} />
    </>
  );
}

function InspectorBoxPair(props: { children: React.ReactNode }) {
  return <div className="dl-fcols">{props.children}</div>;
}

const DELIVERY_BEACONS = [
  { id: "ci-failed", label: "CI failed" },
  { id: "diagnostics", label: "diagnostics" },
  { id: "review", label: "review requested / changes" },
  { id: "stale", label: "stale > 30m" },
  { id: "conflicting-edits", label: "conflicting edits" },
] as const;

function DeliveryEvidenceGraph() {
  const [selectedId, setSelectedId] = useState("#12977");
  const [beacon, setBeacon] = useState<string | null>(null);
  const [camera, setCamera] = useState({ zoom: 1, x: 0, y: 0 });
  const drag = useRef<{ x: number; y: number; camera: typeof camera } | null>(null);
  const admitted = DELIVERY_FIXTURE_PRS.filter((pr) => pr.admission === "joined");
  const unadmitted = DELIVERY_FIXTURE_PRS.filter((pr) => pr.admission === "not-joined");
  const repositories = [...new Set(admitted.map((pr) => pr.repository))].sort();
  const edges = admitted.flatMap((pr) => (pr.edges ?? []).map((edge) => ({ from: pr.id, ...edge })));
  const visible = DELIVERY_FIXTURE_PRS.filter((pr) => !beacon || pr.attention.includes(beacon as DeliveryFixturePr["attention"][number]));
  const selected = DELIVERY_FIXTURE_PRS.find((pr) => pr.id === selectedId) ?? DELIVERY_FIXTURE_PRS[0];
  const select = (pr: DeliveryFixturePr) => setSelectedId(pr.id);
  const nodeX = (pr: DeliveryFixturePr) => 180 + ((pr.lastActivity - 8) / 14) * 980;
  const laneY = (repository: string) => 190 + repositories.indexOf(repository) * 122;
  const resetCamera = () => setCamera({ zoom: 1, x: 0, y: 0 });
  const onWheel = (event: React.WheelEvent<SVGSVGElement>) => {
    event.preventDefault();
    setCamera((current) => ({ ...current, zoom: Math.max(0.65, Math.min(2.4, current.zoom * (event.deltaY < 0 ? 1.12 : 0.89))) }));
  };
  return <div className="dl-stage is-delivery-evidence">
    <aside className="dl-pane">
      <h3>ADMITTED PRS <span>{admitted.length} JOINED · {unadmitted.length} UNADMITTED</span></h3>
      <div className="dl-scroll">
        <div className="dl-delivery-beacons" aria-label="Attention signal legend">
          {DELIVERY_BEACONS.map((item) => <button key={item.id} className={beacon === item.id ? "is-on" : ""} aria-pressed={beacon === item.id} onClick={() => setBeacon(beacon === item.id ? null : item.id)}>{item.label}</button>)}
        </div>
        <p className="dl-microbody">Named attention dims unrelated nodes. It does not establish a relationship.</p>
        {visible.map((pr) => <button key={pr.id} className={`dl-delivery-pr-row${selected.id === pr.id ? " is-selected" : ""}${pr.admission === "not-joined" ? " is-gap" : ""}`} aria-pressed={selected.id === pr.id} onClick={() => select(pr)}>
          <strong>{pr.id} · {pr.title}</strong><span>{pr.repository} · {pr.lastActivity.toFixed(1)} UTC</span><em>{pr.admission === "joined" ? `${pr.changedFiles} files · indexed ${pr.trackedHead}` : pr.gap}</em>
        </button>)}
        {!visible.length && <p className="dl-local-notice">No admitted PR has this named signal in the authored fixture.</p>}
      </div>
    </aside>
    <section className="dl-pane">
      <h3>DELIVERY EVIDENCE GRAPH <span>{admitted.length} ADMITTED · {unadmitted.length} UNADMITTED · {repositories.length} REPOSITORIES · {edges.length} EVIDENCE EDGE</span></h3>
      <div className="dl-delivery-graph" aria-label="Repository lanes, indexed heads, admitted pull requests and evidence relationships">
        <div className="dl-graph-legend"><b>NODE & ATTENTION LEGEND</b><span>◇ indexed head / fresh</span><span>ring = changed-file count</span><span>✕ CI failed · ◉ review · ! diagnostics</span><span>◌ stale · ⚡ conflicting edits</span><span>solid edge = exact evidence</span></div>
        <div className="dl-graph-controls"><button onClick={() => setCamera((value) => ({ ...value, zoom: Math.max(.65, value.zoom - .2) }))}>−</button><span>{Math.round(camera.zoom * 100)}%</span><button onClick={() => setCamera((value) => ({ ...value, zoom: Math.min(2.4, value.zoom + .2) }))}>+</button><button onClick={resetCamera}>Fit</button></div>
        <svg className="dl-delivery-svg" viewBox="0 0 1280 700" role="group" aria-label="Delivery time axis graph. Scroll to zoom and drag to pan."
          onWheel={onWheel}
          onPointerDown={(event) => { drag.current = { x: event.clientX, y: event.clientY, camera }; event.currentTarget.setPointerCapture(event.pointerId); }}
          onPointerMove={(event) => { if (drag.current) setCamera({ ...drag.current.camera, x: drag.current.camera.x + event.clientX - drag.current.x, y: drag.current.camera.y + event.clientY - drag.current.y }); }}
          onPointerUp={() => { drag.current = null; }} onPointerCancel={() => { drag.current = null; }}>
          <g transform={`translate(${camera.x} ${camera.y}) scale(${camera.zoom})`}>
            <text x="22" y="66" className="dl-delivery-axis-label">LAST ACTIVITY · UTC</text>
            <line x1="180" y1="72" x2="1160" y2="72" className="dl-delivery-axis" />
            {[8, 10, 12, 14, 16, 18, 20, 22].map((hour) => <g key={hour}><line x1={180 + ((hour - 8) / 14) * 980} x2={180 + ((hour - 8) / 14) * 980} y1="68" y2="610" className="dl-delivery-tick" /><text x={180 + ((hour - 8) / 14) * 980} y="57" textAnchor="middle" className="dl-delivery-tick-label">{String(hour).padStart(2, "0")}:00</text></g>)}
            {repositories.map((repository) => { const y = laneY(repository); const head = admitted.find((pr) => pr.repository === repository)!; return <g key={repository}><text x="22" y={y - 16} className="dl-delivery-repo">{repository}</text><text x="22" y={y + 1} className="dl-delivery-fresh">INDEX FRESH · {head.trackedHead}</text><line x1="180" x2="1160" y1={y} y2={y} className="dl-delivery-lane" /><rect x="158" y={y - 9} width="18" height="18" className="dl-delivery-head" transform={`rotate(45 167 ${y})`} /><text x="188" y={y + 4} className="dl-delivery-head-label">HEAD</text></g>; })}
            {edges.map((edge) => { const from = admitted.find((pr) => pr.id === edge.from)!; const to = admitted.find((pr) => pr.id === edge.to)!; const x1 = nodeX(from), y1 = laneY(from.repository), x2 = nodeX(to), y2 = laneY(to.repository); return <g key={`${edge.from}-${edge.to}`} className={selected.id === edge.from || selected.id === edge.to ? "is-selected" : ""}><path d={`M ${x1} ${y1} C ${(x1 + x2) / 2} ${y1 - 70}, ${(x1 + x2) / 2} ${y2 - 70}, ${x2} ${y2}`} className="dl-delivery-edge-path" /><text x={(x1 + x2) / 2} y={Math.min(y1, y2) - 42} textAnchor="middle" className="dl-delivery-edge-label">{edge.kind}</text></g>; })}
            {admitted.map((pr) => { const highlighted = !beacon || pr.attention.includes(beacon as DeliveryFixturePr["attention"][number]); const selectedNode = selected.id === pr.id; const radius = 12 + Math.min(16, Math.sqrt(pr.changedFiles ?? 0)); return <g key={pr.id} className={`dl-delivery-pr-node${selectedNode ? " is-selected" : ""}${highlighted ? "" : " is-dim"}`} role="button" tabIndex={0} aria-label={`Select ${pr.id}, ${pr.changedFiles} changed files`} onPointerDown={(event) => event.stopPropagation()} onClick={() => select(pr)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(pr); } }}><circle cx={nodeX(pr)} cy={laneY(pr.repository)} r={radius} /><circle cx={nodeX(pr)} cy={laneY(pr.repository)} r="5" /><text x={nodeX(pr)} y={laneY(pr.repository) + radius + 16} textAnchor="middle">{pr.id}</text><text x={nodeX(pr)} y={laneY(pr.repository) + radius + 29} textAnchor="middle" className="dl-delivery-file-count">{pr.changedFiles} files</text>{pr.attention.map((signal, index) => <text key={signal} x={nodeX(pr) + radius + 6} y={laneY(pr.repository) - radius + index * 13} className="dl-delivery-beacon">{signal === "ci-failed" ? "✕" : signal === "review" ? "◉" : signal === "diagnostics" ? "!" : signal === "stale" ? "◌" : "⚡"}</text>)}</g>; })}
            <rect x="22" y="600" width="1138" height="76" className="dl-unadmitted-band" /><text x="34" y="621" className="dl-delivery-axis-label">UNADMITTED · NOT JOINED TO INDEXED HEAD</text>
            {unadmitted.map((pr) => <g key={pr.id} className={`dl-delivery-pr-node is-unadmitted${selected.id === pr.id ? " is-selected" : ""}`} role="button" tabIndex={0} aria-label={`Select unadmitted ${pr.id}`} onPointerDown={(event) => event.stopPropagation()} onClick={() => select(pr)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(pr); } }}><circle cx={nodeX(pr)} cy="647" r="18" /><text x={nodeX(pr)} y="652" textAnchor="middle">{pr.id}</text><text x={nodeX(pr)} y="672" textAnchor="middle" className="dl-delivery-file-count">{pr.gap}</text><text x={nodeX(pr) + 23} y="639" className="dl-delivery-beacon">⚡</text></g>)}
          </g>
        </svg>
        <div className="dl-delivery-minimap" aria-label="Delivery graph minimap"><svg viewBox="0 0 1280 700" preserveAspectRatio="none" aria-hidden="true">{repositories.map((repository) => <line key={repository} x1="180" x2="1160" y1={laneY(repository)} y2={laneY(repository)} />)}{admitted.map((pr) => <circle key={pr.id} cx={nodeX(pr)} cy={laneY(pr.repository)} r="13" />)}<rect x={Math.max(0, -camera.x / camera.zoom)} y={Math.max(0, -camera.y / camera.zoom)} width={1280 / camera.zoom} height={700 / camera.zoom} /></svg></div>
      </div>
    </section>
    <aside className="dl-pane">
      <h3>SELECTED PR CAUSAL CHAIN <span>{selected.id}</span></h3>
      <div className="dl-scroll dl-delivery-inspector">
        <h2>{selected.title}</h2><p>{selected.repository}</p>
        <dl><dt>Admission</dt><dd>{selected.admission === "joined" ? `registered repository + tracked indexed head ${selected.trackedHead}` : selected.gap}</dd></dl>
        <ol className="dl-causal-chain"><li><b>Agent session</b><span>{selected.agent}</span></li><li><b>Code change</b><span>{selected.code ?? "UNAVAILABLE · not joined to indexed head"}</span></li><li><b>CI / review</b><span>{selected.ci ?? "UNAVAILABLE"} · {selected.review ?? "review UNAVAILABLE"}</span></li><li><b>Next action</b><span>{selected.nextAction}</span></li></ol>
        <p className="dl-hint">Fixture-only causal projection. Gaps remain typed; the graph does not infer membership, outcome, or dependency from shared display.</p>
      </div>
    </aside>
  </div>;
}

function SnapshotQueue(props: { scoped?: boolean }) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<string | null>(null);
  const source = props.scoped ? GLOBAL_PRS.filter((p) => p.repo.startsWith("trace-decay /")) : GLOBAL_PRS;
  const tokens: Record<string, string[]> = { "High risk": ["test_risk", "unsafe_patterns"], "Weak evidence": ["weak_evidence"], Unreviewed: ["unreviewed"], Unresolved: ["unresolved"], Stale: ["stale"], Passed: ["passed"], Failed: ["failed"], Partial: ["partial"], Unknown: ["unknown"] };
  const rows = source.filter((p) => {
    const values = `${p.repo} ${p.id} ${p.title} ${p.author} ${p.attention.join(" ")} ${p.ci} ${p.freshness}`.toLowerCase();
    return values.includes(query.toLowerCase()) && (!filter || (tokens[filter] ?? [filter]).some((token) => values.includes(token.toLowerCase())));
  });
  return <><InboxFilters scoped={props.scoped} query={query} onQuery={setQuery} onFilter={(label) => setFilter(filter === label ? null : label)} />
    {filter ? <button className="dl-btn" onClick={() => setFilter(null)}>Clear filter: {filter}</button> : null}
    <PrTable rows={rows} total={String(source.length)} compact={props.scoped} />
    {!rows.length ? <p className="dl-local-notice">No loaded snapshot PRs match. This is a local filter result, not a provider empty-state receipt.</p> : null}
    <a className="dl-btn" href="?data=fixture&surface=delivery&state=11">Local repositories / provider state →</a>
  </>;
}

export function GlobalInbox() {
  return <DeliveryEvidenceGraph />;
}

export function ProjectInbox() {
  return (
    <div className="dl-stage is-3">
      <aside className="dl-pane">
        <h3>
          PROJECT PR INBOX <span>tracedecay · {GLOBAL_PRS.filter((pr)=>pr.repo.startsWith("trace-decay /")).length} loaded examples</span>
        </h3>
        <div className="dl-scroll">
          <SnapshotQueue scoped />
          <div className="dl-fbox">
            <div className="fk">RELATED ACROSS PROJECTS</div>
            <p className="dl-microbody">Separate authored repository examples; no agent or outcome correlation source is attached.</p>
            {[
              { label: "Rspack", count: "13 PRs", c: "#38cfe8" },
              { label: "Rsbuild / Rslib", count: "9 PRs", c: "#f0b429" },
              { label: "Rspress", count: "7 PRs", c: "#c084fc" },
              { label: "Lynx", count: "6 PRs", c: "#60a5fa" },
              { label: "Module Federation", count: "5 PRs", c: "#9be15d" },
              { label: "TraceDecay (other)", count: "0 PRs", c: "#5ee7ff" },
              { label: "Other", count: "3 PRs", c: "#8b99a8" },
            ].map((r) => (
              <div className="fr" key={r.label}>
                <span className="fl">
                  <i className="mark" style={{ background: r.c }} />
                  {r.label}
                </span>
                <b>{r.count}</b>
              </div>
            ))}
          </div>
          <div className="dl-pager">
            <span>All loaded project detail records are shown</span>
            <span className="pages">

            </span>
          </div>
        </div>
      </aside>
      <section className="dl-pane">
        <h3>
          PROJECT DELIVERY CONSTELLATION <span>ⓘ How to read · SCOPED TO PROJECT tracedecay</span>
        </h3>
        <ProjectConstellation />
      </section>
      <aside className="dl-pane">
        <h3>
          PROJECT PR INSPECTOR <span>Open #707 journey example →</span>
        </h3>
        <div className="dl-scroll">
          <div className="dl-umbtitle">
            <b style={{ color: "var(--signal-cyan)" }}>tracedecay</b>
          </div>
          <div className="dl-kicker">OUTCOME OBJECTIVE</div>
          <p className="dl-body">
            Improve delivery platform stability through remote retry, scheduler fairness, and cache correctness.
          </p>
          <div className="dl-fbox">
            <div className="fk">PROJECT PR COVERAGE</div>
            <div className="fr">
              <span className="fl">{GLOBAL_PRS.filter((pr)=>pr.repo.startsWith("trace-decay /")).length} loaded example records</span>
              <b>Authored sample</b>
            </div>
            <div className="dl-bar">
              <i style={{ width: "100%" }} />
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">SELECTED AGENTS (TOP)</div>
            {[
              { label: "claude-code", n: "324 PRs", pct: 53 },
              { label: "gemini-cli", n: "178 PRs", pct: 29 },
              { label: "codegpt", n: "64 PRs", pct: 10 },
              { label: "human", n: "46 PRs", pct: 8 },
            ].map((a) => (
              <div className="fr" key={a.label}>
                <span className="fl">
                  <i className="mark" style={{ background: "var(--signal-cyan)" }} />
                  {a.label}
                </span>
                <b>
                  {a.n} · {a.pct}%
                </b>
                <span className="fbar">
                  <i style={{ width: `${a.pct}%` }} />
                </span>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">WORK TASK COVERAGE</div>
            <div className="fr">
              <span className="fl amber">238 / 276 tasks</span>
              <b className="amber">86%</b>
            </div>
            <div className="dl-bar">
              <i style={{ width: "86%", background: "var(--activity-amber)" }} />
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">CHANGED SURFACE (TOTAL)</div>
            <div className="fr">
              <span className="fl">78,341 files · 1.84M LOC</span>
            </div>
          </div>
          <InspectorBoxPair>
            <FilterBox
              k="REVIEW EVIDENCE · 87% AVG"
              rows={[
                { label: "High risk", count: "78", tone: "danger" },
                { label: "Weak evidence", count: "96", tone: "amber" },
                { label: "Low risk", count: "—", tone: "live" },
                { label: "Informational", count: "212", tone: "quiet" },
              ]}
            />
            <FilterBox
              k="CI / CHECK COVERAGE · 94%"
              rows={[
                { label: "passed", count: "2,187", tone: "ready" },
                { label: "partial", count: "412", tone: "amber" },
                { label: "failed", count: "196", tone: "danger" },
                { label: "Unavailable", count: "—", tone: "quiet" },
              ]}
            />
          </InspectorBoxPair>
          <div className="dl-fbox">
            <div className="fk">RELEASE OUTCOME (EXPECTED)</div>
            <div className="fr">
              <span className="fl mono">V2.0.0 · Target: 2025-05-23</span>
            </div>
            <div className="fr">
              <span className="fl">Confidence: High · Milestone: V2 Release</span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">MISSING / PRIVATE EVIDENCE</div>
            <div className="fr">
              <span className="fl">Private evidence 17% · Unscoped data 33%</span>
            </div>
            <div className="fr">
              <span className="fl">Unavailable sources 3 · Not indexed 2</span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">
              PROJECT UMBRELLA <span>Open umbrella →</span>
            </div>
            <div className="fr">
              <span className="fl amber">V2 code-intelligence release</span>
            </div>
          </div>
        </div>
      </aside>
    </div>
  );
}

const NODE_LEGEND = [
  { icon: "◔", label: "Review coverage" },
  { icon: "◇", label: "Changed surface" },
  { icon: "✓", label: "CI / Status" },
  { icon: "◎", label: "Responsible agents" },
  { icon: "☀", label: "Freshness" },
  { icon: "▲", label: "Release contribution" },
];

export function UmbrellaGraph() {
  return <DeliveryEvidenceGraph />;
}

function pipeTone(state: string): HonestInbox | null {
  if (state === "NOT_PUBLISHED") return "not_published";
  if (state === "UNAVAILABLE") return "unavailable";
  if (state === "NOT_CONFIGURED") return "not_published";
  return null;
}

export function LocalFirst() {
  const detailId=useId();
  const [selected,setSelected]=useWorkspaceState<{kind:"repository"|"directory"|"projection";id:string}|null>("delivery.fixture.local.selected",null);
  const record=selected?.kind==="repository"?LOCAL_REPOS.find((item)=>item.id===selected.id):selected?.kind==="directory"?UNKNOWN_DIRS.find((item)=>item.id===selected.id):PIPELINE.find((item)=>String(item.n)===selected?.id);
  return (
    <div className="dl-stage is-local">
      <div popover="auto" id={detailId} className="dl-evidence-popover" aria-label="Local record detail"><button popoverTarget={detailId} popoverTargetAction="hide">Back to local evidence</button><h2>{selected?.kind} · {selected?.id}</h2>{record&&<dl>{Object.entries(record).map(([key,value])=><div key={key}><dt>{key}</dt><dd>{String(value)}</dd></div>)}</dl>}<p>{selected?.kind==="projection"?"This is the exact authored projection record and its independent availability state. It is not a repository-specific fetch; other projections do not inherit its readiness.":selected?.kind==="directory"?"Non-Git directory: branch, commit and provider identity are unknown.":"Exact authored repository record. Last-indexed freshness is separate from commit time; local readiness does not establish provider readiness."}</p><p>Local fixture record only. No provider call or enrollment is performed.</p></div>
      <section className="dl-pane">
        <h3>
          <span className="dl-tabchips">
            <span className="chip is-on">🗄 REPOSITORIES</span>
            <span className="chip">PULL REQUESTS</span>
            <span className="chip chip-violet">NOT PUBLISHED</span>
          </span>
          <span>last indexed — not commit time</span>
        </h3>
        <div className="dl-metrics six">
          <div className="dl-metric">
            <b>2m ago</b>
            <span>LAST INDEXED</span>
          </div>
          <div className="dl-metric">
            <b>9</b>
            <span>ACTIVE REPOS · GIT</span>
          </div>
          <div className="dl-metric">
            <b>2 – 38</b>
            <span>BRANCHES (MIN–MAX)</span>
          </div>
          <div className="dl-metric">
            <b>4 – 27</b>
            <span>CHECKOUTS (MIN–MAX)</span>
          </div>
          <div className="dl-metric">
            <b>5 – 143</b>
            <span>WORKING-TREE CHANGES</span>
          </div>
          <div className="dl-metric">
            <b>8.2 MB – 312 MB</b>
            <span>BODY SIZE (MIN–MAX)</span>
          </div>
        </div>
        <div className="dl-scroll">
          <div className="dl-repo head">
            <span>REPOSITORY</span>
            <span>WORKTREE</span>
            <span>AHEAD / BEHIND</span>
            <span>DIRTY</span>
            <span>LATEST LOCAL COMMIT</span>
            <span>INDEX</span>
            <span>STATE</span>
          </div>
          {LOCAL_REPOS.map((r) => (
            <button className="dl-repo dl-local-record" key={r.id} popoverTarget={detailId} aria-label={`Inspect local repository ${r.id}`} aria-pressed={selected?.kind==="repository"&&selected.id===r.id} onClick={()=>setSelected({kind:"repository",id:r.id})}>
              <span>{r.id}</span>
              <span>{r.branch}</span>
              <span>{r.ahead}</span>
              <span>{r.dirty}</span>
              <span>{r.commit}</span>
              <span>{r.fresh}</span>
              <span>
                {r.state === "STALE" ? <HonestMark id="stale" /> : <span className="dl-att tone-ready">FRESH</span>}
              </span>
            </button>
          ))}
          <div className="dl-kicker" style={{ marginTop: 12 }}>
            UNKNOWN / NON-GIT (BRANCHES UNKNOWN)
          </div>
          <div className="dl-unknownstrip mono">
            <span>
              🗀 <b>5</b> directories
            </span>
            <span>
              ⑂ branches <b>unknown</b>
            </span>
            <span>
              🗄 <b>974 MB</b> content on disk
            </span>
            <span>
              ◔ <b>3m ago</b> last scanned
            </span>
          </div>
          {UNKNOWN_DIRS.map((d) => (
            <button className="dl-repo dl-local-record" key={d.id} popoverTarget={detailId} aria-label={`Inspect local directory ${d.id}`} aria-pressed={selected?.kind==="directory"&&selected.id===d.id} onClick={()=>setSelected({kind:"directory",id:d.id})}>
              <span>{d.id}</span>
              <span>{d.path}</span>
              <span>—</span>
              <span>—</span>
              <span>no VCS detected</span>
              <span>3m ago</span>
              <span className="dl-att tone-ready">FRESH</span>
            </button>
          ))}
        </div>
      </section>
      <section className="dl-pane">
        <h3>
          PULL REQUEST INBOX <span className="chip-violet">NOT PUBLISHED</span>
        </h3>
        <div className="dl-empty">
          <div className="glyph">🗳✕</div>
          <h4>
            PULL REQUEST INBOX
            <br />
            NOT PUBLISHED
          </h4>
          <p>
            GitHub read authority is not configured for this profile. Local repositories and delivery history remain
            available.
          </p>
          <a className="dl-btn violet block" href="?data=fixture&surface=settings">⚙ OPEN SETTINGS · PROVIDER AUTHORITY</a>
          <button type="button" className="dl-btn ghost block" onClick={() => document.querySelector<HTMLElement>(".dl-stage.is-local aside")?.focus()}>CONTINUE WITH LOCAL EVIDENCE</button>
        </div>
        <div className="dl-scroll" style={{ flex: "0 0 auto" }}>
          <div className="dl-kicker">INBOX STATE (WHY PRS ARE UNAVAILABLE)</div>
          <div className="dl-chiprow" style={{ gap: 6 }}>
            {(["served-empty", "rate-limited", "denied", "stale", "not_published", "unavailable"] as const).map((id) => (
              <span key={id} style={id === "not_published" ? { outline: "1px solid var(--state-violet)", outlineOffset: 2 } : undefined}>
                <HonestMark id={id} />
              </span>
            ))}
          </div>
          <div className="dl-stat" style={{ marginTop: 8 }}>
            <span>Active state</span>
            <b>
              <HonestMark id="not_published" />
            </b>
          </div>
        </div>
      </section>
      <aside className="dl-pane" tabIndex={-1} aria-label="Local repository evidence">
        <h3>
          DELIVERY PIPELINE <span>8 PROJECTIONS</span>
        </h3>
        <div className="dl-scroll">
          <div className="dl-pipe">
            {PIPELINE.map((s) => {
              const honest = pipeTone(s.state);
              return (
                <button className="step dl-local-record" key={s.n} popoverTarget={detailId} aria-label={`Inspect projection ${s.name}`} aria-pressed={selected?.kind==="projection"&&selected.id===String(s.n)} onClick={()=>setSelected({kind:"projection",id:String(s.n)})}>
                  <span
                    className="n"
                    style={{
                      color:
                        s.state === "READY"
                          ? "var(--state-ready)"
                          : s.state === "MIXED"
                            ? "var(--activity-amber)"
                            : "var(--state-violet)",
                    }}
                  >
                    {s.n}
                  </span>
                  <div>
                    <div className="name">{s.name}</div>
                    <div className="src">{s.source}</div>
                  </div>
                  <span className="st">
                    {honest ? <HonestMark id={honest} /> : <span className="dl-att tone-ready">{s.state}</span>}
                  </span>
                </button>
              );
            })}
          </div>
          <div className="dl-kicker" style={{ marginTop: 16 }}>
            SOURCE LEGEND (EVIDENCE LADDER)
          </div>
          <div className="dl-ladder">
            {[
              { g: "EXACT", c: "var(--signal-cyan)", note: "Direct observation of the artifact" },
              { g: "EXPLICIT", c: "var(--state-ready)", note: "Declared by tool/config/metadata" },
              { g: "INFERRED", c: "#7dd3fc", note: "Derived from multiple signals" },
              { g: "AMBIGUOUS", c: "var(--activity-amber)", note: "Conflicting or unclear signals" },
              { g: "STALE", c: "var(--activity-amber)", note: "Older than freshness threshold" },
              { g: "UNAVAILABLE", c: "var(--state-violet)", note: "Provider or data not accessible" },
            ].map((r) => (
              <div className="rung" key={r.g}>
                <i style={{ background: r.c, boxShadow: `0 0 6px ${r.c}` }} />
                <b style={{ color: r.c }}>{r.g}</b>
                <span>{r.note}</span>
              </div>
            ))}
          </div>
          <p className="dl-hint">
            Local authority remains useful. Provider absence is not empty-PR success — CI identity is unavailable (
            {CHECK_MATRIX.filter((c) => c.provider !== "COMPLETE").length} projections not published).
          </p>
        </div>
      </aside>
    </div>
  );
}
