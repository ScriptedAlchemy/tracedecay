import { useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import {
  FIXTURE_AGENTS,
  FIXTURE_BRANCHES,
  FIXTURE_RELATIONS,
  FIXTURE_TOTALS,
  fixtureAgent,
  fixtureRelation,
  pathToRoot,
  type FixtureAgent,
  type FixtureBranch,
  type FixtureRelation,
  type RelationKind,
} from "./dense";

type Altitude = "outcome" | "branch" | "agent" | "event";
type NavigatorFocus = "all" | "pinned" | "issues";
type SceneNode = {
  id: string;
  kind: "agent" | "bundle";
  label: string;
  sub: string;
  branch: string;
  x: number;
  y: number;
  r: number;
  status?: FixtureAgent["status"];
};
type SceneEdge = {
  id: string;
  from: string;
  to: string;
  kind: RelationKind | "aggregate";
  label: string;
};
type Scene = { nodes: SceneNode[]; edges: SceneEdge[]; height: number };
type MiniViewport = { x: number; y: number; width: number; height: number };

function hasFixtureIssue(agent: FixtureAgent) {
  return agent.status === "failed" || agent.status === "ambiguous" || agent.parentId?.startsWith("missing:") === true;
}

function nodeHasFixtureIssue(node: SceneNode) {
  if (node.kind === "agent") {
    const agent = fixtureAgent(node.id);
    return agent ? hasFixtureIssue(agent) : false;
  }
  const id = node.id.slice("bundle:".length);
  const workstream = FIXTURE_BRANCHES.find((item) => item.id === id);
  if (workstream) return workstream.agents.some(hasFixtureIssue);
  return FIXTURE_AGENTS.some((agent) => (agent.id === id || agent.parentId === id) && hasFixtureIssue(agent));
}

function curve(a: SceneNode, b: SceneNode) {
  const mx = (a.x + b.x) / 2;
  return `M ${a.x} ${a.y} C ${mx} ${a.y}, ${mx} ${b.y}, ${b.x} ${b.y}`;
}

function agentNode(agent: FixtureAgent, x: number, y: number): SceneNode {
  return {
    id: agent.id,
    kind: "agent",
    label: agent.label,
    sub: `${agent.events.toLocaleString()} events`,
    branch: agent.branch,
    x,
    y,
    r: agent.depth === 0 ? 15 : agent.depth === 1 ? 11 : 7,
    status: agent.status,
  };
}

function sceneFor(altitude: Altitude, branch: FixtureBranch, width: number, height: number): Scene {
  const w = Math.max(560, width);
  const root = FIXTURE_AGENTS[0];
  const nodes: SceneNode[] = [];
  const edges: SceneEdge[] = [];

  if (altitude === "outcome") {
    const h = Math.max(260, height);
    nodes.push(agentNode(root, 74, h / 2));
    FIXTURE_BRANCHES.forEach((item, index) => {
      const y = 54 + index * ((h - 108) / (FIXTURE_BRANCHES.length - 1));
      nodes.push({
        id: `bundle:${item.id}`,
        kind: "bundle",
        label: item.label,
        sub: `${item.agents.length} agents · ${item.events.toLocaleString()} events · ${item.failures} failed · ${item.gaps} gap`,
        branch: item.id,
        x: w * 0.5,
        y,
        r: 13,
      });
      edges.push({ id: `aggregate:${item.id}`, from: root.id, to: `bundle:${item.id}`, kind: "aggregate", label: `${item.agents.length} agents` });
    });
    return { nodes, edges, height: h };
  }

  const h = Math.max(410, height);

  const leaders = FIXTURE_BRANCHES.map((item) => item.agents[0]);
  nodes.push(agentNode(root, 64, h / 2));
  leaders.forEach((leader, index) => {
    const y = 50 + index * ((h - 100) / (leaders.length - 1));
    nodes.push(agentNode(leader, w * 0.35, y));
    edges.push({ id: `parent:${leader.id}`, from: root.id, to: leader.id, kind: "parentage", label: "parent_session_id" });
  });

  if (altitude === "branch") {
    branch.agents.slice(1, 5).forEach((agent, index) => {
      const leader = nodes.find((node) => node.id === branch.agents[0].id)!;
      const y = Math.max(42, Math.min(h - 42, leader.y + (index - 1.5) * 42));
      nodes.push({
        id: `bundle:${agent.id}`,
        kind: "bundle",
        label: agent.label,
        sub: `${branch.agents.filter((member) => member.parentId === agent.id).length + 1} agents · exact branch`,
        branch: branch.id,
        x: w * 0.72,
        y,
        r: 9,
      });
      edges.push({ id: `aggregate:${agent.id}`, from: branch.agents[0].id, to: `bundle:${agent.id}`, kind: "aggregate", label: "branch bundle" });
    });
    return { nodes, edges, height: h };
  }

  const selectedLeader = nodes.find((node) => node.id === branch.agents[0].id)!;
  selectedLeader.x = w * 0.2;
  selectedLeader.y = h / 2;
  nodes.filter((node) => node.id !== root.id && node.id !== selectedLeader.id).forEach((node, index) => {
    node.kind = "bundle";
    node.label = FIXTURE_BRANCHES.find((item) => item.id === node.branch)?.label ?? node.label;
    node.sub = "compressed context";
    node.x = w * 0.08;
    node.y = 42 + index * 35;
    node.r = 5;
  });

  const groups = branch.agents.slice(1, 5);
  groups.forEach((agent, index) => nodes.push(agentNode(agent, w * 0.46, 70 + index * ((h - 140) / 3))));
  const leaves = branch.agents.slice(5);
  leaves.forEach((agent, index) => nodes.push(agentNode(agent, w * 0.77, 38 + index * 25)));

  const visibleIds = new Set(nodes.map((node) => node.id));
  const relevant = FIXTURE_RELATIONS.filter((relation) => visibleIds.has(relation.from) && visibleIds.has(relation.to));
  relevant.forEach((relation) => {
    if (!edges.some((edge) => edge.id === relation.id)) edges.push({ ...relation, label: relation.kind });
  });

  if (altitude === "event") {
    FIXTURE_RELATIONS.filter((relation) => relation.kind !== "parentage" && (visibleIds.has(relation.from) || visibleIds.has(relation.to))).forEach((relation) => {
      const remoteId = visibleIds.has(relation.from) ? relation.to : relation.from;
      if (!visibleIds.has(remoteId)) {
        const remote = fixtureAgent(remoteId);
        if (remote) {
          nodes.push(agentNode(remote, w * 0.93, 44 + (nodes.length % 10) * 34));
          visibleIds.add(remoteId);
        }
      }
      if (visibleIds.has(relation.from) && visibleIds.has(relation.to) && !edges.some((edge) => edge.id === relation.id)) {
        edges.push({ ...relation, label: relation.kind });
      }
    });
  }

  return { nodes, edges, height: Math.max(h, 430) };
}

function DenseInspector(props: { selectedId: string; relationId: string | null }) {
  const selected = fixtureAgent(props.selectedId);
  const relation = fixtureRelation(props.relationId);
  if (relation) {
    const title = relation.kind === "parentage" ? "RECORDED PARENT EDGE" : relation.kind === "handoff" ? "SELECTED HANDOFF" : "SELECTED REJOIN";
    return (
      <aside className="ag-inspect ag-dense-inspect" aria-label="Selected relation">
        <Corners />
        <div className={`ag-kick is-${relation.kind}`}>{title}<em>{relation.grade}</em></div>
        <h2 className="ag-pair"><span>{relation.from}</span><i>→</i><span className="to">{relation.to}</span></h2>
        <div className="ag-block"><div className="k">RELATION AUTHORITY</div>
          <div className="ag-row"><span>kind</span><span className="r">{relation.kind}</span></div>
          <div className="ag-row"><span>direction</span><span className="r">source → destination</span></div>
          <div className="ag-row"><span>source</span><span className="r">{relation.source}</span></div>
          <div className="ag-row"><span>grade</span><span className="r">{relation.grade}</span></div>
        </div>
        <div className="ag-block"><div className="k">WORK READINESS</div><p className="ag-copy">This fixture relation is evidence only. It does not create or unlock a Work dependency.</p></div>
        <div className="ag-hint">DENSE FIXTURE · synthetic identities and evidence records</div>
      </aside>
    );
  }
  return (
    <aside className="ag-inspect ag-dense-inspect" aria-label="Selected relation">
      <Corners />
      <div className="ag-kick">SELECTED AGENT <em>FIXTURE</em></div>
      <h2 className="ag-pair"><span className="to">{selected?.label ?? props.selectedId}</span></h2>
      {selected ? <>
        <div className="ag-block"><div className="k">IDENTITY</div>
          <div className="ag-row"><span>agent</span><span className="r">{selected.id}</span></div>
          <div className="ag-row"><span>workstream</span><span className="r">{selected.branch}</span></div>
          <div className="ag-row"><span>generation</span><span className="r">GEN {selected.depth}</span></div>
          <div className="ag-row"><span>events</span><span className="r">{selected.events.toLocaleString()}</span></div>
          <div className="ag-row"><span>status</span><span className="r">{selected.status}</span></div>
        </div>
        <div className="ag-block"><div className="k">PARENT REFERENCE</div>
          <div className={selected.parentId?.startsWith("missing:") ? "ag-row is-abs" : "ag-row"}><span>parent</span><span className={selected.parentId?.startsWith("missing:") ? "r abs" : "r"}>{selected.parentId ?? "none"}</span></div>
        </div>
      </> : null}
      <div className="ag-hint">DENSE FIXTURE · no snapshot identity or downstream receipt is implied</div>
    </aside>
  );
}

function DenseField(props: {
  altitude: Altitude;
  branch: FixtureBranch;
  selectedId: string;
  relationId: string | null;
  pathFocus: boolean;
  navigatorFocus: NavigatorFocus;
  pinnedBranches: string[];
  onNode: (id: string, branch: string) => void;
  onRelation: (id: string) => void;
}) {
  const wrap = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ width: 900, height: 520 });
  useEffect(() => {
    const element = wrap.current;
    if (!element) return;
    const observer = new ResizeObserver(() => setBox({ width: element.clientWidth, height: element.clientHeight }));
    observer.observe(element);
    setBox({ width: element.clientWidth, height: element.clientHeight });
    return () => observer.disconnect();
  }, []);
  const scene = useMemo(() => sceneFor(props.altitude, props.branch, box.width, box.height), [props.altitude, props.branch, box]);
  const byId = useMemo(() => new Map(scene.nodes.map((node) => [node.id, node])), [scene]);
  const path = useMemo(() => pathToRoot(props.selectedId), [props.selectedId]);
  const worldWidth = Math.max(560, box.width);
  const [viewport, setViewport] = useState<MiniViewport>({ x: 0, y: 0, width: worldWidth, height: scene.height });

  useEffect(() => {
    const element = wrap.current;
    if (!element) return;
    const update = () => {
      setViewport({
        x: element.scrollLeft / Math.max(1, element.scrollWidth) * worldWidth,
        y: element.scrollTop / Math.max(1, element.scrollHeight) * scene.height,
        width: Math.min(worldWidth, element.clientWidth / Math.max(1, element.scrollWidth) * worldWidth),
        height: Math.min(scene.height, element.clientHeight / Math.max(1, element.scrollHeight) * scene.height),
      });
    };
    const frame = requestAnimationFrame(update);
    element.addEventListener("scroll", update, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      element.removeEventListener("scroll", update);
    };
  }, [scene, worldWidth]);

  function panFromMinimap(event: MouseEvent<HTMLButtonElement>) {
    const element = wrap.current;
    const map = event.currentTarget.querySelector("svg");
    if (!element || !map) return;
    const selected = byId.get(props.selectedId);
    const bounds = map.getBoundingClientRect();
    const x = event.detail === 0 && selected ? selected.x : (event.clientX - bounds.left) / Math.max(1, bounds.width) * worldWidth;
    const y = event.detail === 0 && selected ? selected.y : (event.clientY - bounds.top) / Math.max(1, bounds.height) * scene.height;
    element.scrollTo({
      left: Math.max(0, Math.min(element.scrollWidth - element.clientWidth, x / worldWidth * element.scrollWidth - element.clientWidth / 2)),
      top: Math.max(0, Math.min(element.scrollHeight - element.clientHeight, y / scene.height * element.scrollHeight - element.clientHeight / 2)),
      behavior: "auto",
    });
  }

  return <>
    <div className="ag-dense-field" ref={wrap}>
    <svg viewBox={`0 0 ${worldWidth} ${scene.height}`} aria-label="Dense delegation topology">
      <defs><radialGradient id="ag-dense-core"><stop offset="0" stopColor="#40dfff" stopOpacity=".8"/><stop offset="1" stopColor="#06111a"/></radialGradient></defs>
      {scene.edges.map((edge) => {
        const from = byId.get(edge.from), to = byId.get(edge.to);
        if (!from || !to) return null;
        const inNavigatorFocus = props.navigatorFocus === "all" ||
          (props.navigatorFocus === "pinned" && (props.pinnedBranches.includes(from.branch) || props.pinnedBranches.includes(to.branch))) ||
          (props.navigatorFocus === "issues" && (nodeHasFixtureIssue(from) || nodeHasFixtureIssue(to)));
        const selectedContext = edge.id === props.relationId || (path.has(edge.from) && path.has(edge.to));
        const dim = (props.pathFocus && !path.has(edge.from) && !path.has(edge.to)) || (!inNavigatorFocus && !selectedContext);
        return <g key={edge.id} className={`ag-dense-edge is-${edge.kind}${edge.id === props.relationId ? " is-on" : ""}${dim ? " is-dim" : ""}`} role="button" tabIndex={0} aria-label={`Select ${edge.kind} relation ${edge.from} to ${edge.to}`} onClick={() => props.onRelation(edge.id)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onRelation(edge.id); } }}>
          <path d={curve(from, to)} />
          <path className="hit" d={curve(from, to)} />
          {props.altitude === "event" && edge.kind !== "aggregate" ? <text x={(from.x + to.x) / 2} y={(from.y + to.y) / 2 - 4}>{edge.label}</text> : null}
        </g>;
      })}
      {scene.nodes.map((node) => {
        const inNavigatorFocus = props.navigatorFocus === "all" ||
          (props.navigatorFocus === "pinned" && props.pinnedBranches.includes(node.branch)) ||
          (props.navigatorFocus === "issues" && nodeHasFixtureIssue(node));
        const selectedContext = node.id === props.selectedId || path.has(node.id);
        const dim = (props.pathFocus && node.kind === "agent" && !path.has(node.id)) || (!inNavigatorFocus && !selectedContext);
        const pinned = props.pinnedBranches.includes(node.branch);
        return <g key={node.id} className={`ag-dense-node is-${node.kind} is-${node.status ?? "context"}${node.id === props.selectedId ? " is-on" : ""}${pinned ? " is-pinned" : ""}${dim ? " is-dim" : ""}`} transform={`translate(${node.x} ${node.y})`} role="button" tabIndex={0} aria-label={`Select ${node.label}`} onClick={() => props.onNode(node.id, node.branch)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onNode(node.id, node.branch); } }}>
          <circle r={node.r} />
          <text className="name" x={node.r + 7} y="-2">{node.label}</text>
          <text className="sub" x={node.r + 7} y="10">{node.sub}</text>
        </g>;
      })}
    </svg>
    </div>
    <button type="button" className="ag-spatial-minimap" aria-label="Topology minimap; click to pan, press Enter to center selection" onClick={panFromMinimap}>
      <span>TOPOLOGY MAP</span>
      <svg viewBox={`0 0 ${worldWidth} ${scene.height}`} aria-hidden="true">
        {scene.edges.map((edge) => {
          const from = byId.get(edge.from), to = byId.get(edge.to);
          return from && to ? <path key={edge.id} className={`ag-mini-edge is-${edge.kind}`} d={curve(from, to)} /> : null;
        })}
        {scene.nodes.map((node) => <circle key={node.id} className={`ag-mini-node${node.id === props.selectedId ? " is-selected" : ""}${props.pinnedBranches.includes(node.branch) ? " is-pinned" : ""}`} cx={node.x} cy={node.y} r={Math.max(5, node.r * .55)} />)}
        <rect className="ag-mini-viewport" x={viewport.x} y={viewport.y} width={viewport.width} height={viewport.height} />
      </svg>
    </button>
  </>;
}

export function DenseAgents() {
  const { setMode } = useDemo();
  const [altitude, setAltitude] = useWorkspaceState<Altitude>("agents:altitude", "outcome");
  const [branchId, setBranchId] = useWorkspaceState("agents:branch", FIXTURE_BRANCHES[0].id);
  const [selectedId, setSelectedId] = useWorkspaceState("agents:selected", "fixture-root");
  const [relationId, setRelationId] = useWorkspaceState<string | null>("agents:relation", null);
  const [pathFocus, setPathFocus] = useWorkspaceState("agents:path", false);
  const [pinState, setPinState] = useWorkspaceState<string[]>("agents:pins", []);
  const [navigatorFocusState, setNavigatorFocus] = useWorkspaceState<NavigatorFocus>("agents:navigator-focus", "all");
  const [exact, setExact] = useWorkspaceState("agents:exact", false);
  const [query, setQuery] = useWorkspaceState("agents:query", "");
  const [pane, setPane] = useState<"topology" | "details">("topology");
  const branch = FIXTURE_BRANCHES.find((item) => item.id === branchId) ?? FIXTURE_BRANCHES[0];
  const pinnedBranches = Array.isArray(pinState) ? pinState.filter((id) => typeof id === "string" && FIXTURE_BRANCHES.some((item) => item.id === id)) : [];
  const navigatorFocus: NavigatorFocus = navigatorFocusState === "pinned" || navigatorFocusState === "issues" ? navigatorFocusState : "all";
  const issueCount = FIXTURE_AGENTS.filter(hasFixtureIssue).length;
  const filtered = FIXTURE_AGENTS.filter((agent) => {
    if (!`${agent.id} ${agent.label} ${agent.branch} ${agent.status} ${agent.parentId ?? ""}`.toLowerCase().includes(query.toLowerCase())) return false;
    if (navigatorFocus === "pinned" && !pinnedBranches.includes(agent.branch)) return false;
    if (navigatorFocus === "issues" && !hasFixtureIssue(agent)) return false;
    return true;
  });

  function togglePin(id: string) {
    const next = pinnedBranches.includes(id) ? pinnedBranches.filter((item) => item !== id) : [...pinnedBranches, id];
    setPinState(next);
    if (!next.length && navigatorFocus === "pinned") setNavigatorFocus("all");
  }

  function selectNode(id: string, nextBranch: string) {
    if (id.startsWith("bundle:")) {
      const bundleId = id.slice(7);
      const workstream = FIXTURE_BRANCHES.find((item) => item.id === bundleId);
      const agent = fixtureAgent(bundleId);
      if (workstream) {
        setBranchId(workstream.id);
        setSelectedId(workstream.agents[0].id);
        setAltitude("branch");
      } else if (agent) {
        setBranchId(agent.branch);
        setSelectedId(agent.id);
        setAltitude("agent");
      }
    } else {
      setSelectedId(id);
      if (nextBranch !== "control") setBranchId(nextBranch);
    }
    setRelationId(null);
    if (!id.startsWith("bundle:") && matchMedia("(max-width: 900px)").matches) setPane("details");
  }

  return <div className={`ag-root ag-dense-root is-${pane}`}>
    <div className="ag-mobile-tabs" role="tablist" aria-label="Agents view">
      <button type="button" role="tab" aria-selected={pane === "topology"} onClick={() => setPane("topology")}>Topology</button>
      <button type="button" role="tab" aria-selected={pane === "details"} onClick={() => setPane("details")}>Selected relation</button>
    </div>
    <div className="ag-band">
      <div className="ag-chip"><div className="k">DATA MODE</div><b>DENSE FIXTURE</b><small>synthetic · isolated from snapshot</small><button className="ag-mode" type="button" onClick={() => setMode("snapshot")}>USE SNAPSHOT</button></div>
      <div className="ag-chip"><div className="k">UNIQUE AGENTS</div><b>{FIXTURE_TOTALS.agents}</b><small>{FIXTURE_TOTALS.subagents} subagents · exact</small></div>
      <div className="ag-chip"><div className="k">DETERMINISTIC BUNDLES</div><b>{FIXTURE_BRANCHES.length}</b><small>workstream → branch → agent</small></div>
      <div className="ag-chip"><div className="k">RELATIONS</div><b>{FIXTURE_TOTALS.relations}</b><small>120 parent · 6 handoff · 6 rejoin</small></div>
      <div className="ag-chip"><div className="k">EVIDENCE GAPS</div><b className="ag-amber">{FIXTURE_TOTALS.gaps}</b><small>missing parent records</small></div>
      <div className="ag-chip"><div className="k">EVENT VOLUME</div><b>{FIXTURE_TOTALS.events.toLocaleString()}</b><small>fixture measured field</small></div>
    </div>
    <main className="ag-main">
      <section className="ag-topo ag-dense-topo" aria-label="Dense delegation topology">
        <div className="ag-topo-head"><b>DELEGATION TOPOLOGY <i>(fixture)</i></b><span className="meta">{branch.label} · {filtered.length}/{FIXTURE_TOTALS.agents} exact rows{navigatorFocus === "issues" ? " · ISSUES = failed / ambiguous / missing parent" : navigatorFocus === "pinned" ? ` · ${pinnedBranches.length} pinned branches` : ""}</span></div>
        <div className="ag-dense-controls">
          <div className="ag-altitude" role="group" aria-label="Semantic zoom">
            {(["outcome", "branch", "agent", "event"] as const).map((level) => <button type="button" key={level} aria-pressed={altitude === level} onClick={() => setAltitude(level)}>{level}</button>)}
          </div>
          <button type="button" aria-pressed={pathFocus} onClick={() => setPathFocus((value) => !value)}>PATH FOCUS</button>
          <button type="button" disabled={!pinnedBranches.length} aria-pressed={navigatorFocus === "pinned"} onClick={() => setNavigatorFocus((value) => value === "pinned" ? "all" : "pinned")}>PINNED {pinnedBranches.length}</button>
          <button type="button" title="Exact fixture issues: failed or ambiguous status, or an explicit missing parent reference. No downstream risk is inferred." aria-pressed={navigatorFocus === "issues"} onClick={() => setNavigatorFocus((value) => value === "issues" ? "all" : "issues")}>ISSUES {issueCount}</button>
          <button type="button" onClick={() => { setAltitude("outcome"); setSelectedId("fixture-root"); setRelationId(null); setPathFocus(false); }}>FIT</button>
          <button type="button" aria-pressed={exact} onClick={() => setExact((value) => !value)}>EXACT ROWS</button>
          <input type="search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search branch or agent" aria-label="Search branches" />
        </div>
        <div className="ag-relation-legend"><i className="parentage" /> parentage · EXACT parent record <i className="handoff" /> handoff · exact ledger <i className="rejoin" /> rejoin · explicit result <strong>FRONTIER:</strong> click a bundle to expand its deterministic branch</div>
        <DenseField altitude={altitude} branch={branch} selectedId={selectedId} relationId={relationId} pathFocus={pathFocus} navigatorFocus={navigatorFocus} pinnedBranches={pinnedBranches} onNode={selectNode} onRelation={(id) => { setRelationId(id); const relation = fixtureRelation(id); if (relation) setSelectedId(relation.to); if (matchMedia("(max-width: 900px)").matches) setPane("details"); }} />
        <nav className="ag-minimap" aria-label="Branch navigator">
          <span>BRANCH NAVIGATOR</span>
          {FIXTURE_BRANCHES.map((item) => <div className="ag-minimap-row" key={item.id}><button type="button" className={item.id === branch.id ? "is-on" : ""} aria-pressed={item.id === branch.id} onClick={() => { setBranchId(item.id); setSelectedId(item.agents[0].id); setRelationId(null); setAltitude("branch"); }}>{item.label}</button><button type="button" className="ag-pin" aria-label={`${pinnedBranches.includes(item.id) ? "Unpin" : "Pin"} ${item.label}`} aria-pressed={pinnedBranches.includes(item.id)} onClick={() => togglePin(item.id)}>◆</button></div>)}
        </nav>
        {exact || query.trim() ? <div className="ag-exact"><div className="ag-exact-head"><b>EXACT AGENT FALLBACK</b><span>{filtered.length} / {FIXTURE_TOTALS.agents}</span></div><div className="ag-exact-table" role="table" aria-label="Exact fixture agents">
          {filtered.map((agent) => <button type="button" role="row" key={agent.id} className={agent.id === selectedId ? "is-on" : ""} onClick={() => selectNode(agent.id, agent.branch)}><span role="cell">{agent.id}</span><span role="cell">{agent.branch}</span><span role="cell">GEN {agent.depth}</span><span role="cell">{agent.events.toLocaleString()}</span><span role="cell">{agent.status}</span><span role="cell">{agent.parentId ?? "root"}</span></button>)}
        </div></div> : null}
      </section>
    </main>
    <DenseInspector selectedId={selectedId} relationId={relationId} />
  </div>;
}
