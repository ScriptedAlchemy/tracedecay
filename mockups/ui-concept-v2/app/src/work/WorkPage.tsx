import { useEffect, useLayoutEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent, type WheelEvent } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { shortId } from "../data/pack";
import {
  DEFAULT_FAMILY, FAMILIES, FIXTURE_GRAPH, FIXTURE_PLANS, WORK_PROJECTIONS,
  applyWorkAction, blockingRelations, clock, familyById, legalActions,
  isValidWorkGraph, nodeInFamily, recentActivity, statusTone,
  type Family, type RelationKind, type ThreadNode, type WorkAction,
  type WorkGraph, type WorkProjection, type WorkStatus, type WorkTask,
} from "./model";
import "./work.css";

type Camera = { x: number; y: number; zoom: number };
type Point = { x: number; y: number };
type GraphScope = "outcome" | "root" | "plan" | "overview";
const FOCUS_CAMERA: Camera = { x: 0, y: 8, zoom: 1 };
const OVERVIEW_CAMERA: Camera = { x: 10, y: 14, zoom: 0.68 };
const WORLD = { width: 1280, height: 650 };
function isCamera(value: unknown): value is Camera {
  if (!value || typeof value !== "object") return false;
  const camera = value as Partial<Camera>;
  return Number.isFinite(camera.x) && Number.isFinite(camera.y) && Number.isFinite(camera.zoom) && camera.zoom! >= 0.5 && camera.zoom! <= 1.9;
}
function isCameraHistory(value: unknown): value is Camera[] {
  return Array.isArray(value) && value.length <= 8 && value.every(isCamera);
}
const actionLabel: Record<WorkAction, string> = {
  accept: "Accept into plan", admit: "Admit execution", replan: "Replan blocking edge",
  complete: "Record fixture success", retry: "Retry attempt",
};
const relationLabel: Record<RelationKind, string> = {
  gating: "GATING · READINESS", informational: "INFORMATIONAL", parallel: "PLANNED PARALLEL",
  observed: "OBSERVED ORDER", delegation: "DELEGATION",
};

function traverseGating(graph: WorkGraph, start: string, direction: "root" | "outcome") {
  const found = new Set([start]);
  let frontier = [start];
  while (frontier.length) {
    const next = graph.relations.filter((relation) => relation.kind === "gating" && (direction === "outcome" ? frontier.includes(relation.from) : frontier.includes(relation.to))).map((relation) => direction === "outcome" ? relation.to : relation.from).filter((id) => !found.has(id));
    next.forEach((id) => found.add(id));
    frontier = next;
  }
  return found;
}

function gatingRanks(graph: WorkGraph) {
  const ranks = new Map(graph.tasks.map((task) => [task.id, 0]));
  for (let pass = 0; pass < graph.tasks.length; pass += 1) {
    let changed = false;
    for (const relation of graph.relations) {
      if (relation.kind !== "gating") continue;
      const next = (ranks.get(relation.from) ?? 0) + 1;
      if (next > (ranks.get(relation.to) ?? 0)) { ranks.set(relation.to, next); changed = true; }
    }
    if (!changed) break;
  }
  return ranks;
}

function eventSecond(graph: WorkGraph, taskId: string) {
  const value = graph.events.slice().reverse().find((event) => event.taskId === taskId)?.at;
  if (!value) return null;
  const [hour, minute, second] = value.split(":").map(Number);
  return hour * 3600 + minute * 60 + second;
}

function estimateMinutes(estimate: string) {
  const hours = Number(estimate.match(/(\d+)h/)?.[1] ?? 0);
  const minutes = Number(estimate.match(/(\d+)m/)?.[1] ?? 0);
  return hours * 60 + minutes;
}

function visibleTasks(graph: WorkGraph, selected: WorkTask, projection: WorkProjection, scope: GraphScope) {
  if (scope === "overview") return graph.tasks;
  const plan = graph.tasks.filter((task) => task.planId === selected.planId);
  if (projection !== "DAG" || scope === "plan") return plan;
  const ids = traverseGating(graph, selected.id, scope);
  if (scope === "outcome") {
    graph.relations.filter((relation) => relation.kind === "parallel" && (ids.has(relation.from) || ids.has(relation.to))).forEach((relation) => { ids.add(relation.from); ids.add(relation.to); });
    let expanded = true;
    while (expanded) {
      expanded = false;
      graph.relations.filter((relation) => relation.kind === "gating" && ids.has(relation.from) && !ids.has(relation.to)).forEach((relation) => { ids.add(relation.to); expanded = true; });
    }
  }
  return plan.filter((task) => ids.has(task.id));
}

function projectTask(task: WorkTask, projection: WorkProjection, graph: WorkGraph, shown: WorkTask[], scope: GraphScope): Point {
  const planIndex = Math.max(0, FIXTURE_PLANS.findIndex((plan) => plan.id === task.planId));
  const siblings = graph.tasks.filter((entry) => entry.planId === task.planId).sort((a, b) => a.sequence - b.sequence);
  const index = siblings.findIndex((entry) => entry.id === task.id);
  const ranks = gatingRanks(graph);
  const rankOf = (entry: WorkTask) => ranks.get(entry.id) ?? 0;
  if (projection === "DAG" || projection === "CAUSAL") {
    if (scope === "overview") {
      const base = 20 + planIndex * 410;
      const rank = siblings.filter((entry) => rankOf(entry) === rankOf(task));
      const rankIndex = rank.findIndex((entry) => entry.id === task.id);
      return { x: base + (rank.length === 1 ? 95 : rankIndex * 190), y: 40 + rankOf(task) * 116 + (projection === "CAUSAL" ? (index % 2) * 12 : 0) };
    }
    const depths = [...new Set(shown.map(rankOf))].sort((a, b) => a - b);
    const rank = shown.filter((entry) => rankOf(entry) === rankOf(task)).sort((a, b) => a.sequence - b.sequence);
    const rankIndex = rank.findIndex((entry) => entry.id === task.id);
    const xs = rank.length === 1 ? [370] : rank.length === 2 ? [105, 635] : [25, 370, 715];
    return { x: xs[rankIndex], y: 42 + depths.indexOf(rankOf(task)) * 150 + (projection === "CAUSAL" ? rankIndex * 16 : 0) };
  }
  if (projection === "TIMELINE") {
    const timed = shown.map((entry) => eventSecond(graph, entry.id)).filter((value): value is number => value !== null);
    const min = Math.min(...timed), max = Math.max(...timed);
    const at = eventSecond(graph, task.id);
    if (at === null) {
      const missing = shown.filter((entry) => eventSecond(graph, entry.id) === null);
      const missingIndex = missing.findIndex((entry) => entry.id === task.id);
      return { x: 70 + (missingIndex % 5) * 230, y: scope === "overview" ? 475 + Math.floor(missingIndex / 5) * 120 : 365 };
    }
    return { x: 70 + (max === min ? 460 : ((at - min) / (max - min)) * 900), y: 105 + (scope === "overview" ? planIndex * 105 : 0) };
  }
  if (projection === "WORKLOAD") {
    const owners = [...new Set(shown.map((entry) => entry.owner))];
    const ownerIndex = owners.indexOf(task.owner);
    const ownerTasks = shown.filter((entry) => entry.owner === task.owner);
    const columns = scope === "overview" ? 3 : Math.min(3, owners.length);
    const regionX = 30 + (ownerIndex % columns) * 410;
    const regionY = 42 + Math.floor(ownerIndex / columns) * 300;
    const item = ownerTasks.findIndex((entry) => entry.id === task.id);
    return { x: regionX + 18 + (item % 2) * 190, y: regionY + 50 + Math.floor(item / 2) * 122 };
  }
  if (scope === "overview") return { x: 35 + index * 225, y: 72 + planIndex * 180 };
  return { x: 60 + (index % 3) * 350, y: 85 + Math.floor(index / 3) * 180 };
}

function Status({ status }: { status: WorkTask["status"] }) {
  return <span className={`wk-status is-${statusTone(status)}`}><i />{status.toUpperCase()}</span>;
}
function Row({ label, value, muted }: { label: string; value: string; muted?: boolean }) {
  return <div className="wk-row"><span>{label}</span><b className={muted ? "is-muted" : ""}>{value}</b></div>;
}
function ProjectionTabs({ mode, selected, onPick }: { mode: "snapshot" | "fixture"; selected?: WorkProjection; onPick?: (p: WorkProjection) => void }) {
  return <div className="wk-switch" role="tablist" aria-label="Work projections">
    {WORK_PROJECTIONS.map((projection) => <button key={projection} type="button" role="tab"
      aria-selected={mode === "fixture" && projection === selected}
      className={mode === "fixture" && projection === selected ? "wk-chip is-on" : "wk-chip"}
      disabled={mode === "snapshot"}
      title={mode === "snapshot" ? "Unavailable: no canonical Work graph is loaded" : `Show ${projection.toLowerCase()} projection`}
      onClick={() => onPick?.(projection)}>{projection}</button>)}
    <span className={`wk-mode-tag is-${mode}`}>{mode === "fixture" ? "AUTHORED FIXTURE" : "SNAPSHOT · NO WORK GRAPH"}</span>
  </div>;
}

function pivotToSession(navigate: ReturnType<typeof useDemo>["navigate"], surface: "agents" | "sessions", sessionId: string, source: "mac" | "design") {
  const origin = new URL(location.href);
  origin.searchParams.set("surface", "loom"); origin.searchParams.set("loom_source", source);
  navigate(surface, {
    loom_pivot: "1", loom_source: source, loom_page: source === "design" ? "full" : "",
    loom_target_session: sessionId, loom_return: origin.pathname + origin.search, work_return: "1",
  });
}
function ThreadGlyph({ node, selected, onPick, x, y }: { node: ThreadNode; family: Family; selected: boolean; onPick: () => void; x: number; y: number }) {
  const ghost = node.kind === "ghost";
  return <button type="button" className={`wk-thread-glyph${selected ? " is-selected" : ""}${ghost ? " is-ghost" : ""}`}
    style={{ left: x, top: y }} onClick={onPick} aria-pressed={selected}
    title={node.id}
    aria-label={`${ghost ? "Missing recorded parent" : "Observed session"} ${node.id}`}>
    <span className="wk-thread-ring"><i /></span>
    <b>{ghost ? "RECORDED PARENT" : node.session?.isSubagent ? "DELEGATE" : "ROOT THREAD"}</b>
    <code>{shortId(node.id, 12)}</code>
  </button>;
}

function SnapshotWork({ initialSessionId }: { initialSessionId?: string }) {
  const { navigate, setMode } = useDemo();
  const initialFamily = FAMILIES.find((family) => initialSessionId && nodeInFamily(family, initialSessionId)) ?? DEFAULT_FAMILY;
  const [familyKey, setFamilyKey] = useWorkspaceState("work.snapshot.family", initialFamily.key);
  const family = familyById(familyKey);
  const initialNode = initialSessionId && nodeInFamily(family, initialSessionId) ? initialSessionId : family.root.kind === "session" ? family.root.id : family.children[0]?.id ?? family.root.id;
  const [selectedId, setSelectedId] = useWorkspaceState("work.snapshot.thread", initialNode);
  useEffect(() => {
    if (!initialSessionId || !nodeInFamily(initialFamily, initialSessionId)) return;
    setFamilyKey(initialFamily.key);
    setSelectedId(initialSessionId);
  }, [initialFamily, initialSessionId, setFamilyKey, setSelectedId]);
  const selected = nodeInFamily(family, selectedId) ?? family.root;
  const activity = useMemo(() => recentActivity(5), []);
  const previewChildren = family.children.slice(0, 5);
  if (selected !== family.root && !previewChildren.includes(selected)) previewChildren.splice(-1, 1, selected);
  const preview = [family.root, ...previewChildren];
  const point = (index: number) => index === 0 ? { x: 76, y: 104 } : { x: 252 + ((index - 1) % 3) * 168, y: 48 + Math.floor((index - 1) / 3) * 104 };
  const routeTarget = selected.kind === "session" ? selected : family.children[0];
  const route = () => routeTarget && pivotToSession(navigate, "agents", routeTarget.id, "mac");
  return <div className="wk-root is-snapshot">
    <main className="wk-main"><ProjectionTabs mode="snapshot" />
      <section className="wk-panel wk-snapshot-board"><header className="wk-panel-head"><b>WORK CANVAS</b><span>profile snapshot · bounded export</span></header>
        <div className="wk-absence">
          <div className="wk-landmark"><span>SOURCE SCOPE</span><b>all captured</b><i>stable snapshot landmark</i></div><div className="wk-empty-mark" aria-hidden="true"><i /><i /><i /><i /></div>
          <div className="wk-absence-copy"><span>AUTHORITY STATE · UNAVAILABLE</span><h1>NO CANONICAL WORK GRAPH</h1>
            <p>This profile snapshot contains no task identities, plan membership, dependency edges, readiness, attempts, or prepared task commands.</p>
            <div className="wk-absence-actions"><button type="button" className="is-primary" onClick={() => setMode("fixture")}>Open authored Work fixture</button><button type="button" onClick={route}>Route observed thread to Agents</button></div>
          </div>
          {["TASK NODES", "READINESS EDGES", "PREPARED COMMANDS"].map((label) => <div className="wk-absence-proof" key={label}><span>0</span><b>{label}</b><i>source-backed</i></div>)}
        </div>
        <div className="wk-thread-preview"><div className="wk-preview-head"><span><b>OBSERVED DELEGATION PREVIEW</b> · session topology, not Work</span>
          <label>family <select aria-label="Observed delegation family" value={family.key} onChange={(event) => { const next = familyById(event.target.value); setFamilyKey(next.key); setSelectedId(next.root.id); }}>
            {FAMILIES.map((item) => <option value={item.key} key={item.key}>{shortId(item.key, 8)} · {item.children.length} delegates{item.edgeStyle === "dashed" ? " · parent absent" : ""}</option>)}</select></label></div>
          <div className="wk-thread-scene"><svg viewBox="0 0 760 210" preserveAspectRatio="none" aria-hidden="true">
            {preview.slice(1).map((node, index) => { const a = point(0), b = point(index + 1); return <path key={node.id} d={`M ${a.x + 35} ${a.y + 18} C ${a.x + 105} ${a.y + 18}, ${b.x - 64} ${b.y + 18}, ${b.x} ${b.y + 18}`} className={family.edgeStyle === "dashed" ? "is-dashed" : ""} />; })}
          </svg>{preview.map((node, index) => { const p = point(index); return <ThreadGlyph key={node.id} node={node} family={family} selected={selected.id === node.id} onPick={() => setSelectedId(node.id)} x={p.x} y={p.y} />; })}
          {family.children.length > 5 && <span className="wk-more-threads">+{family.children.length - 5} observed sessions</span>}</div>
          <p>Curved strands encode recorded <code>parent_session_id</code>. They do not block, unlock, accept, or complete work.</p>
        </div>
      </section>
      <section className="wk-panel wk-activity"><header className="wk-panel-head"><b>OBSERVED SESSION ACTIVITY</b><span>copied spine events · no task lifecycle implied</span></header>
        <div className="wk-feed" role="table" aria-label="Observed session activity">{activity.map((event) => <div className="wk-event" role="row" key={event.key}>
          <time>{clock(event.at)}</time><i className={`is-${event.tone}`} /><b>{event.event}</b><code>{shortId(event.sessionId, 12)}</code><span>{event.detail}</span>
        </div>)}</div>
      </section>
    </main>
    <aside className="wk-inspect"><Corners /><div className="wk-inspect-body"><h2>WORK AUTHORITY<em>SNAPSHOT / UNAVAILABLE</em></h2>
      <div className="wk-alert is-amber"><i />No task graph was copied</div><p className="wk-note">The empty Work canvas is the primary state. This subordinate thread preview remains addressable through Agents.</p>
      <div className="wk-block"><div className="k">SOURCE BOUNDARY</div><Row label="project" value={family.project} /><Row label="task identities" value="unavailable" muted /><Row label="graph revision" value="unavailable" muted /><Row label="readiness" value="unavailable" muted /><Row label="attempt records" value="unavailable" muted /><Row label="commands" value="not prepared" muted /></div>
      <div className="wk-block"><div className="k">SELECTED OBSERVED SESSION</div><div className="wk-selected-id">{shortId(selected.id, 18)}</div><Row label="identity grade" value={selected.kind === "ghost" ? "ABSENT" : "EXACT"} muted={selected.kind === "ghost"} /><Row label="relationship" value={selected === family.root ? "family root" : "delegation"} /><Row label="generation" value={selected.generation == null ? "—" : `G${selected.generation}`} /><button type="button" className="wk-wide-action" onClick={route}>{selected.kind === "ghost" ? "Open represented family in Agents" : "Open exact source in Agents"} <span>→</span></button></div>
      <div className="wk-block"><div className="k">AVAILABLE NEXT STEP</div><button type="button" className="wk-wide-action is-primary" onClick={() => setMode("fixture")}>Load explicit task fixture <span>→</span></button><p className="wk-note">Fixture records are authored examples and remain labeled. They do not claim to come from this snapshot.</p></div>
    </div></aside>
  </div>;
}

function taskSize(task: WorkTask, projection: WorkProjection, selected: boolean, scope: GraphScope) {
  if (projection === "WORKLOAD") {
    const mass = estimateMinutes(task.estimate);
    return { width: 150 + Math.min(45, mass / 4), height: 82 + Math.min(26, mass / 6) };
  }
  if (scope === "overview") return selected ? { width: 200, height: 90 } : { width: 178, height: 82 };
  return selected ? { width: 300, height: 122 } : { width: 260, height: 108 };
}

function projectedRelations(graph: WorkGraph, projection: WorkProjection) {
  return graph.relations.filter((relation) => projection === "DAG" ? ["gating", "parallel", "informational"].includes(relation.kind) : projection === "CAUSAL" ? ["gating", "informational", "observed"].includes(relation.kind) : projection === "TIMELINE" ? relation.kind === "observed" : projection === "TOPOLOGY" ? ["delegation", "gating"].includes(relation.kind) : false);
}

function relationPath(relation: WorkGraph["relations"][number], graph: WorkGraph, projection: WorkProjection, scope: GraphScope, points: Map<string, Point>, selectedId: string) {
  const a = points.get(relation.from), b = points.get(relation.to);
  const from = graph.tasks.find((task) => task.id === relation.from), to = graph.tasks.find((task) => task.id === relation.to);
  if (!a || !b || !from || !to) return null;
  const fromSize = taskSize(from, projection, from.id === selectedId, scope);
  const toSize = taskSize(to, projection, to.id === selectedId, scope);
  const vertical = projection === "DAG" || projection === "CAUSAL";
  const sameRank = vertical && Math.abs(a.y - b.y) < 35;
  const sx = sameRank ? a.x + fromSize.width : vertical ? a.x + fromSize.width / 2 : a.x + fromSize.width;
  const sy = sameRank ? a.y + fromSize.height / 2 : vertical ? a.y + fromSize.height : a.y + fromSize.height / 2;
  const ex = sameRank ? b.x - 8 : vertical ? b.x + toSize.width / 2 : b.x - 8;
  const ey = sameRank ? b.y + toSize.height / 2 : vertical ? b.y - 8 : b.y + toSize.height / 2;
  return vertical && !sameRank
    ? `M${sx} ${sy} C${sx} ${(sy + ey) / 2},${ex} ${(sy + ey) / 2},${ex} ${ey}`
    : `M${sx} ${sy} C${(sx + ex) / 2} ${sy},${(sx + ex) / 2} ${ey},${ex} ${ey}`;
}

function EdgeLayer({ graph, projection, scope, points, selectedId, focusId }: { graph: WorkGraph; projection: WorkProjection; scope: GraphScope; points: Map<string, Point>; selectedId: string; focusId: string }) {
  const shown = projectedRelations(graph, projection);
  return <svg className="wk-edge-layer" viewBox={`0 0 ${WORLD.width} ${WORLD.height}`} aria-label={`${projection.toLowerCase()} relations`}><defs>
    <marker id="wk-arrow-gating" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="4.5" markerHeight="4.5" orient="auto"><path d="M0 0 8 4 0 8Z" fill="#4dc5e5" /></marker>
    <marker id="wk-arrow-order" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="4.5" markerHeight="4.5" orient="auto"><path d="M0 1 7 4 0 7" fill="none" stroke="#d0a34e" /></marker></defs>
    {shown.map((relation) => {
      const d = relationPath(relation, graph, projection, scope, points, selectedId);
      if (!d) return null;
      const selected = relation.from === focusId || relation.to === focusId;
      return <g key={relation.id} className={`wk-relation is-${relation.kind}${selected ? " is-selected" : ""}`}><path d={d} markerEnd={relation.kind === "gating" ? "url(#wk-arrow-gating)" : relation.kind === "observed" ? "url(#wk-arrow-order)" : undefined} /><title>{relationLabel[relation.kind]} · {relation.label} · {relation.grade}</title></g>;
    })}
  </svg>;
}
function TaskNode({ task, point, projection, scope, selected, previewed, muted, onPick, onPreview, onTraverse }: { task: WorkTask; point: Point; projection: WorkProjection; scope: GraphScope; selected: boolean; previewed: boolean; muted: boolean; onPick: () => void; onPreview: (id: string | null) => void; onTraverse: (id: string, key: "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown") => void }) {
  const beacon = task.status === "failed" || task.status === "blocked" || task.status === "waiting";
  const size = taskSize(task, projection, selected, scope);
  const sized = projection === "WORKLOAD" || scope === "overview" ? { width: size.width, minHeight: size.height } : {};
  return <button type="button" className={`wk-task is-${statusTone(task.status)}${projection === "WORKLOAD" ? " is-workload" : ""}${scope === "overview" ? " is-compact" : ""}${selected ? " is-selected" : ""}${previewed ? " is-preview" : ""}${muted ? " is-muted" : ""}`} style={{ left: point.x, top: point.y, ...sized }} onClick={onPick} onMouseEnter={() => onPreview(task.id)} onMouseLeave={() => onPreview(null)} onFocus={() => onPreview(task.id)} onBlur={() => onPreview(null)} onKeyDown={(event) => { if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) { event.preventDefault(); onTraverse(task.id, event.key as "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown"); } }} aria-pressed={selected} data-task-id={task.id}>
    {beacon && <span className={`wk-beacon is-${task.status}`} aria-label={`${task.status} attention`} />}<span className="wk-task-top"><code>{task.id}</code><Status status={task.status} /></span><b>{task.title}</b><span className="wk-task-meta">{task.component}<i />{task.priority}<i />{task.estimate}</span>
  </button>;
}
function Minimap({ graph, projection, scope, tasks, points, selectedId, camera, viewport, onFit }: { graph: WorkGraph; projection: WorkProjection; scope: GraphScope; tasks: WorkTask[]; points: Map<string, Point>; selectedId: string; camera: Camera; viewport: { width: number; height: number }; onFit: () => void }) {
  return <button type="button" className="wk-minimap" onClick={onFit} aria-label="Fit current Work graph"><svg viewBox={`0 0 ${WORLD.width} ${WORLD.height}`} aria-hidden="true"><g className="wk-mini-relations">{projectedRelations(graph, projection).map((relation) => { const d = relationPath(relation, graph, projection, scope, points, selectedId); return d ? <path key={relation.id} d={d} className={`is-${relation.kind}`} /> : null; })}</g>{tasks.map((task) => { const p = points.get(task.id)!; const size = taskSize(task, projection, task.id === selectedId, scope); return <rect key={task.id} x={p.x} y={p.y} width={size.width} height={size.height} className={`${task.id === selectedId ? "is-selected" : ""} is-${statusTone(task.status)}`} />; })}<rect className="wk-mini-camera" x={-camera.x / camera.zoom} y={-camera.y / camera.zoom} width={viewport.width / camera.zoom} height={viewport.height / camera.zoom} /></svg><span>MINIMAP · FIT VIEW</span></button>;
}

function FixtureWork() {
  const { navigate, setMode } = useDemo();
  const [storedGraph, setGraph] = useWorkspaceState<WorkGraph>("work.fixture.graph", FIXTURE_GRAPH);
  const graph = isValidWorkGraph(storedGraph) ? storedGraph : FIXTURE_GRAPH;
  const [projection, setProjection] = useWorkspaceState<WorkProjection>("work.fixture.projection", "DAG");
  const [selectedId, setSelectedId] = useWorkspaceState("work.fixture.selection.v2", "T-102");
  const [anchorId, setAnchorId] = useWorkspaceState("work.fixture.path-anchor.v2", "T-102");
  const [scope, setScope] = useWorkspaceState<GraphScope>("work.fixture.scope.v2", "outcome");
  const [storedCamera, setCamera] = useWorkspaceState<Camera>("work.fixture.camera.v2", FOCUS_CAMERA);
  const [storedHistory, rememberHistory] = useWorkspaceState<Camera[]>("work.fixture.camera-history.v2", []);
  const camera = isCamera(storedCamera) ? storedCamera : FOCUS_CAMERA;
  const history = isCameraHistory(storedHistory) ? storedHistory : [];
  const setHistory = (update: Camera[] | ((items: Camera[]) => Camera[])) => rememberHistory(typeof update === "function" ? update(history) : update);
  const [ledgerView, setLedgerView] = useWorkspaceState<"activity" | "exact">("work.fixture.lower-pane", "activity");
  const [previewId, setPreviewId] = useState<string | null>(null);
  const [taskQuery, setTaskQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<WorkStatus | "all">("all");
  const [viewport, setViewport] = useState({ width: 720, height: 360 });
  const [drag, setDrag] = useState<{ id: number; x: number; y: number; camera: Camera } | null>(null);
  const fieldRef = useRef<HTMLDivElement>(null);
  const worldRef = useRef<HTMLDivElement>(null);
  const handledArrival = useRef(false);
  const handledNarrowFit = useRef(false);
  const selected = graph.tasks.find((task) => task.id === selectedId) ?? graph.tasks[0];
  const anchor = graph.tasks.find((task) => task.id === anchorId) ?? selected;
  const sceneScope: GraphScope = projection === "DAG" ? scope : scope === "overview" ? "overview" : "plan";
  const scopedTasks = visibleTasks(graph, anchor, projection, sceneScope);
  const shownTasks = scopedTasks.some((task) => task.id === selected.id) ? scopedTasks : [...scopedTasks, selected];
  const blockers = blockingRelations(graph, selected.id);
  const actions = legalActions(graph, selected.id);
  const relations = graph.relations.filter((relation) => relation.from === selected.id || relation.to === selected.id);
  const normalizedQuery = taskQuery.trim().toLowerCase();
  const filterActive = normalizedQuery.length > 0 || statusFilter !== "all";
  const matchingTasks = graph.tasks.filter((task) => (statusFilter === "all" || task.status === statusFilter) && (!normalizedQuery || task.id.toLowerCase().includes(normalizedQuery) || task.title.toLowerCase().includes(normalizedQuery)));
  const matchingIds = new Set(matchingTasks.map((task) => task.id));
  const statusOptions = [...new Set(graph.tasks.map((task) => task.status))].sort();
  const points = useMemo(() => new Map(shownTasks.map((task) => [task.id, projectTask(task, projection, graph, shownTasks, sceneScope)])), [graph, projection, sceneScope, selected.id, anchor.id]);
  const focusId = previewId ?? selected.id;
  const neighborhood = new Set([focusId, ...graph.relations.filter((relation) => relation.from === focusId || relation.to === focusId).flatMap((relation) => [relation.from, relation.to])]);
  const altitude = camera.zoom < 0.7 ? "PORTFOLIO" : camera.zoom < 1.05 ? "PLAN" : camera.zoom < 1.48 ? "TASK" : "ATTEMPT";
  const moveCamera = (next: Camera, remember = true) => { if (remember) setHistory((items) => [...items.slice(-7), camera]); setCamera(next); };
  const zoomBy = (delta: number) => moveCamera({ ...camera, zoom: Math.max(0.5, Math.min(1.9, +(camera.zoom + delta).toFixed(2))) });
  function fitCurrent() {
    const field = fieldRef.current, world = worldRef.current;
    if (!field || !world) return;
    const nodes = [...world.querySelectorAll<HTMLElement>(".wk-task,.wk-plan-context")];
    if (!nodes.length) return;
    const minX = Math.min(...nodes.map((node) => node.offsetLeft));
    const minY = Math.min(...nodes.map((node) => node.offsetTop));
    const maxX = Math.max(...nodes.map((node) => node.offsetLeft + node.offsetWidth));
    const maxY = Math.max(...nodes.map((node) => node.offsetTop + node.offsetHeight));
    const width = maxX - minX, height = maxY - minY;
    const zoom = Math.max(0.5, Math.min(1.08, Math.min((field.clientWidth - 34) / width, (field.clientHeight - 30) / height)));
    setHistory((items) => [...items.slice(-7), camera]);
    setCamera({ x: (field.clientWidth - width * zoom) / 2 - minX * zoom, y: (field.clientHeight - height * zoom) / 2 - minY * zoom, zoom: +zoom.toFixed(3) });
  }
  const refitAfterRender = () => requestAnimationFrame(() => fitCurrent());
  const chooseScope = (next: GraphScope) => { setAnchorId(selected.id); setScope(next); setCamera(next === "overview" ? OVERVIEW_CAMERA : FOCUS_CAMERA); refitAfterRender(); };
  const pickTask = (id: string) => { if (!shownTasks.some((task) => task.id === id)) setAnchorId(id); setSelectedId(id); setPreviewId(null); };
  function traverseTask(id: string, key: "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown") {
    const current = graph.tasks.find((task) => task.id === id);
    if (!current) return;
    const ranks = gatingRanks(graph);
    const sameRank = shownTasks.filter((task) => ranks.get(task.id) === ranks.get(id)).sort((a, b) => points.get(a.id)!.x - points.get(b.id)!.x);
    const gatingTargetIds = graph.relations.filter((relation) => relation.kind === "gating" && (key === "ArrowUp" ? relation.to === id : relation.from === id)).map((relation) => key === "ArrowUp" ? relation.from : relation.to);
    const vertical = shownTasks.filter((task) => gatingTargetIds.includes(task.id)).sort((a, b) => Math.abs(points.get(a.id)!.x - points.get(id)!.x) - Math.abs(points.get(b.id)!.x - points.get(id)!.x));
    const siblingIndex = sameRank.findIndex((task) => task.id === id);
    const next = key === "ArrowLeft" ? sameRank[Math.max(0, siblingIndex - 1)] : key === "ArrowRight" ? sameRank[Math.min(sameRank.length - 1, siblingIndex + 1)] : vertical[0];
    if (!next) return;
    document.querySelector<HTMLElement>(`[data-task-id="${next.id}"]`)?.focus();
  }
  function onWheel(event: WheelEvent<HTMLDivElement>) {
    event.preventDefault();
    const bounds = event.currentTarget.getBoundingClientRect();
    const nextZoom = Math.max(0.5, Math.min(1.9, camera.zoom * (event.deltaY > 0 ? 0.9 : 1.1)));
    const px = event.clientX - bounds.left, py = event.clientY - bounds.top;
    moveCamera({ x: px - ((px - camera.x) / camera.zoom) * nextZoom, y: py - ((py - camera.y) / camera.zoom) * nextZoom, zoom: +nextZoom.toFixed(3) });
  }
  function onPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if ((event.target as HTMLElement).closest("button")) return;
    event.currentTarget.setPointerCapture(event.pointerId); setDrag({ id: event.pointerId, x: event.clientX, y: event.clientY, camera });
  }
  function onPointerMove(event: ReactPointerEvent<HTMLDivElement>) {
    if (!drag || drag.id !== event.pointerId) return;
    setCamera({ ...drag.camera, x: drag.camera.x + event.clientX - drag.x, y: drag.camera.y + event.clientY - drag.y });
  }
  function onPointerUp(event: ReactPointerEvent<HTMLDivElement>) {
    if (!drag || drag.id !== event.pointerId) return;
    setHistory((items) => [...items.slice(-7), drag.camera]); setDrag(null);
  }
  useLayoutEffect(() => {
    const field = fieldRef.current;
    if (!field) return;
    const measure = () => {
      setViewport({ width: field.clientWidth, height: field.clientHeight });
      if (!handledNarrowFit.current && field.clientWidth < 760 && camera.x === FOCUS_CAMERA.x && camera.y === FOCUS_CAMERA.y && camera.zoom === FOCUS_CAMERA.zoom) {
        handledNarrowFit.current = true;
        requestAnimationFrame(fitCurrent);
      }
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(field);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!isValidWorkGraph(storedGraph)) setGraph(FIXTURE_GRAPH);
  }, [setGraph, storedGraph]);
  useEffect(() => {
    if (!isCamera(storedCamera)) setCamera(FOCUS_CAMERA);
    if (!isCameraHistory(storedHistory)) rememberHistory([]);
  }, [rememberHistory, setCamera, storedCamera, storedHistory]);
  useEffect(() => {
    if (handledArrival.current) return;
    handledArrival.current = true;
    const requested = new URLSearchParams(location.search).get("work_task");
    if (!requested) return;
    const task = graph.tasks.find((entry) => entry.id === requested);
    if (!task) return;
    setSelectedId(task.id); setAnchorId(task.id); setScope("plan"); setCamera(FOCUS_CAMERA);
  }, [graph.tasks, setAnchorId, setCamera, setScope, setSelectedId]);
  const requestedTask = new URLSearchParams(location.search).get("work_task");
  const arrivalMissing = requestedTask !== null && !graph.tasks.some((task) => task.id === requestedTask);
  const pivot = (surface: string, extra: Record<string, string> = {}) => navigate(surface, { work_task: selected.id, work_revision: String(graph.revision), work_return: "1", ...extra });
  const pivotAttempt = (sessionId: string) => pivotToSession(navigate, "sessions", sessionId, "design");
  return <div className="wk-root is-fixture"><main className="wk-main"><ProjectionTabs mode="fixture" selected={projection} onPick={(next) => { setProjection(next); refitAfterRender(); }} />
    <section className="wk-panel wk-fixture-board"><header className="wk-panel-head"><b>CANONICAL TASK GRAPH</b><span>revision {graph.revision} · explicit fixture · {graph.tasks.length} stable TaskIds</span></header>
      {arrivalMissing ? <div className="wk-arrival-error" role="alert">TASK PIVOT UNAVAILABLE · {requestedTask} is not present in fixture revision {graph.revision}. The existing selection was preserved.</div> : null}
      <div className="wk-controls"><div className="wk-path-controls" role="group" aria-label="Graph path scope"><button type="button" disabled={projection !== "DAG"} className={projection === "DAG" && scope === "outcome" ? "is-on" : ""} onClick={() => chooseScope("outcome")}>OUTCOMES</button><button type="button" disabled={projection !== "DAG"} className={projection === "DAG" && scope === "root" ? "is-on" : ""} onClick={() => chooseScope("root")}>ROOT PATH</button><button type="button" className={sceneScope === "plan" ? "is-on" : ""} onClick={() => chooseScope("plan")}>PLAN</button><button type="button" className={sceneScope === "overview" ? "is-on" : ""} onClick={() => chooseScope("overview")}>OVERVIEW · 15</button></div>
        <div className="wk-find"><input type="search" aria-label="Find TaskId or title" placeholder="FIND TASK" value={taskQuery} onChange={(event) => setTaskQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && matchingTasks.length === 1) pickTask(matchingTasks[0].id); }} /><select aria-label="Filter tasks by status" value={statusFilter} onChange={(event) => setStatusFilter(event.target.value as WorkStatus | "all")}><option value="all">ALL STATES</option>{statusOptions.map((status) => <option key={status} value={status}>{status.toUpperCase()}</option>)}</select><button type="button" aria-label="Clear task filters" disabled={!filterActive} onClick={() => { setTaskQuery(""); setStatusFilter("all"); }}>CLEAR</button>
          {filterActive ? <div className="wk-filter-results" role="listbox" aria-label="Matching tasks"><span aria-live="polite">{matchingTasks.length} OF {graph.tasks.length} TASKS</span>{matchingTasks.length ? matchingTasks.map((task) => <button type="button" role="option" aria-selected={selected.id === task.id} key={task.id} onClick={() => pickTask(task.id)}><code>{task.id}</code><b>{task.title}</b><Status status={task.status} /></button>) : <em>NO MATCHING TASKS · GRAPH CONTEXT RETAINED</em>}</div> : null}
        </div><div className="wk-relation-key">{(["gating", "informational", "parallel"] as RelationKind[]).map((kind) => <span className={`is-${kind}`} key={kind}><i />{relationLabel[kind]}</span>)}</div><span className="wk-spacer" /><span className="wk-altitude">ALTITUDE <b>{altitude}</b> · {Math.round(camera.zoom * 100)}%</span>
        <div className="wk-tools"><button type="button" onClick={fitCurrent} aria-label="Fit graph">FIT</button><button type="button" onClick={() => zoomBy(-0.15)} aria-label="Zoom out">−</button><button type="button" onClick={() => zoomBy(0.15)} aria-label="Zoom in">+</button><button type="button" disabled={!history.length} onClick={() => { const prior = history.at(-1); if (prior) { setCamera(prior); setHistory((items) => items.slice(0, -1)); } }} aria-label="Previous camera">BACK</button></div>
      </div>
      <div ref={fieldRef} className={`wk-field is-${altitude.toLowerCase()}${drag ? " is-dragging" : ""}`} tabIndex={0} aria-label={`${projection} projection; drag to pan and use wheel to zoom`} onWheel={onWheel} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerUp}
        onKeyDown={(event) => { if (event.target !== event.currentTarget) return; if (event.key === "+" || event.key === "=") zoomBy(0.15); if (event.key === "-") zoomBy(-0.15); if (event.key === "0") fitCurrent(); if (event.key.startsWith("Arrow")) { event.preventDefault(); moveCamera({ ...camera, x: camera.x + (event.key === "ArrowLeft" ? 35 : event.key === "ArrowRight" ? -35 : 0), y: camera.y + (event.key === "ArrowUp" ? 35 : event.key === "ArrowDown" ? -35 : 0) }); } }}>
        <div ref={worldRef} className={`wk-world is-${sceneScope}`} style={{ width: WORLD.width, height: WORLD.height, transform: `translate(${camera.x}px, ${camera.y}px) scale(${camera.zoom})` }}>
          {(projection === "DAG" || projection === "CAUSAL") && (sceneScope === "overview" ? FIXTURE_PLANS.map((plan, index) => <div className="wk-plan-landmark" key={plan.id} style={{ left: 20 + index * 410, top: 14 }}><code>{plan.id}</code><b>{plan.title}</b><span>{graph.tasks.filter((task) => task.planId === plan.id).length} tasks</span></div>) : <div className="wk-plan-landmark is-focus" style={{ left: 18, top: 18 }}><code>{anchor.planId}</code><b>{FIXTURE_PLANS.find((plan) => plan.id === anchor.planId)?.title}</b><span>{sceneScope.toUpperCase()} · ANCHOR {anchor.id} · {shownTasks.length} TASKS</span></div>)}
          {projection === "TOPOLOGY" && (sceneScope === "overview" ? FIXTURE_PLANS.map((plan, index) => <div className="wk-plan-landmark" key={plan.id} style={{ left: 12, top: 45 + index * 180 }}><code>{plan.id}</code><b>{plan.title}</b><span>{graph.tasks.filter((task) => task.planId === plan.id).length} tasks · membership band</span></div>) : <div className="wk-plan-landmark is-focus" style={{ left: 18, top: 18 }}><code>{anchor.planId}</code><b>{FIXTURE_PLANS.find((plan) => plan.id === anchor.planId)?.title}</b><span>PLAN MEMBERSHIP · {shownTasks.length} TASKS</span></div>)}
          {projection === "TIMELINE" ? <><div className="wk-timeline-axis"><span>{graph.events.filter((event) => shownTasks.some((task) => task.id === event.taskId)).map((event) => event.at).sort()[0] ?? "NO TIMED EVENTS"}</span><b>AUTHORED EVENT TIME</b><span>{graph.events.filter((event) => shownTasks.some((task) => task.id === event.taskId)).map((event) => event.at).sort().at(-1) ?? "—"}</span></div><div className="wk-unavailable-band">UNSCHEDULED / EVENT TIME UNAVAILABLE · absence retained</div></> : null}
          {projection === "WORKLOAD" ? [...new Set(shownTasks.map((task) => task.owner))].map((owner, index, owners) => { const owned = shownTasks.filter((task) => task.owner === owner); const mass = owned.reduce((total, task) => total + estimateMinutes(task.estimate), 0); const columns = sceneScope === "overview" ? 3 : Math.min(3, owners.length); return <div className="wk-owner-region" key={owner} style={{ left: 30 + (index % columns) * 410, top: 42 + Math.floor(index / columns) * 300, height: 165 + Math.min(105, mass / 2) }}><b>{owner}</b><span>{owned.length} tasks · {mass} authored estimate-minutes</span><i>region area ∝ task mass</i></div>; }) : null}
          <EdgeLayer graph={graph} projection={projection} scope={sceneScope} points={points} selectedId={selected.id} focusId={focusId} />
          {shownTasks.map((task) => <TaskNode key={task.id} task={task} point={points.get(task.id)!} projection={projection} scope={sceneScope} selected={selected.id === task.id} previewed={previewId === task.id && selected.id !== task.id} muted={(previewId !== null && !neighborhood.has(task.id)) || (filterActive && !matchingIds.has(task.id))} onPick={() => pickTask(task.id)} onPreview={setPreviewId} onTraverse={traverseTask} />)}
          {sceneScope !== "overview" && FIXTURE_PLANS.filter((plan) => plan.id !== anchor.planId).map((plan, index) => { const tasks = graph.tasks.filter((task) => task.planId === plan.id); const alerts = tasks.filter((task) => task.status === "blocked" || task.status === "failed" || task.status === "waiting").length; return <button type="button" className="wk-plan-context" key={plan.id} style={{ left: 24, top: 330 + index * 63 }} onClick={() => { setSelectedId(tasks[0].id); setAnchorId(tasks[0].id); setScope("outcome"); moveCamera(FOCUS_CAMERA); }}><code>{plan.id}</code><b>{plan.title}</b><span>{tasks.length} tasks · {alerts} attention</span></button>; })}
          {(projection === "TOPOLOGY" || altitude === "ATTEMPT") && shownTasks.flatMap((task) => task.attempts.map((attempt, index) => { const p = points.get(task.id)!; return <button key={attempt.id} type="button" className={`wk-attempt is-${attempt.status}`} style={{ left: p.x + 75 + index * 42, top: p.y + 112 }} onClick={() => { pickTask(task.id); pivotAttempt(attempt.sessionId); }} title={`Open ${attempt.id} in Sessions`}><i /><code>{attempt.id}</code></button>; }))}
        </div><Minimap graph={graph} projection={projection} scope={sceneScope} tasks={shownTasks} points={points} selectedId={selected.id} camera={camera} viewport={viewport} onFit={fitCurrent} /><div className="wk-camera-help">DRAG PAN · WHEEL ZOOM · 0 FIT</div>
      </div><div className="wk-canvas-foot"><span><b>{selected.id}</b> · {sceneScope === "overview" ? "portfolio overview" : `${sceneScope} focus from ${anchor.id}`} · {shownTasks.length} of {graph.tasks.length} tasks</span><span>Only solid cyan arrowheads gate readiness.</span></div>
    </section>
    <section className="wk-panel wk-exact"><header className="wk-panel-head"><b>{ledgerView === "activity" ? "TASK ACTIVITY LEDGER" : "EXACT TASK INDEX"}</b><span className="wk-lower-switch"><button type="button" className={ledgerView === "activity" ? "is-on" : ""} onClick={() => setLedgerView("activity")}>ACTIVITY</button><button type="button" className={ledgerView === "exact" ? "is-on" : ""} onClick={() => setLedgerView("exact")}>EXACT TASK TABLE · {graph.tasks.length}</button></span></header>{ledgerView === "activity" ? <div className="wk-fixture-activity" role="table" aria-label="Authored fixture task activity">{graph.events.slice(-5).map((event) => <button type="button" role="row" key={event.id} onClick={() => pickTask(event.taskId)}><time>{event.at}</time><i /><span>{event.event}</span><code>{event.taskId}</code><b>{graph.tasks.find((task) => task.id === event.taskId)?.title}</b><em>{event.detail}</em></button>)}</div> : <div className="wk-task-table" role="table" aria-label="Exact fixture tasks">{graph.tasks.map((task) => <button type="button" role="row" key={task.id} className={task.id === selected.id ? "is-selected" : ""} onClick={() => pickTask(task.id)}><code>{task.id}</code><b>{task.title}</b><span>{task.planId}</span><Status status={task.status} /><span>{task.owner}</span></button>)}</div>}</section>
  </main>
  <aside className="wk-inspect"><Corners /><div className="wk-inspect-body"><h2>SELECTED TASK<em>FIXTURE / EXPLICIT · REV {graph.revision}</em></h2><div className="wk-selected-id">{selected.id}</div><div className="wk-selected-title">{selected.title}</div><Status status={selected.status} />
    {(selected.status === "failed" || selected.status === "blocked" || selected.status === "waiting") && <div className={`wk-alert is-${selected.status === "failed" ? "red" : "amber"}`}><i />{selected.status === "failed" ? "Attempt failed · provenance retained" : selected.status === "waiting" ? "Waiting on an external review decision" : `${blockers.length} gating blocker${blockers.length === 1 ? "" : "s"}`}</div>}
    <div className="wk-block"><div className="k">CANONICAL IDENTITY</div><Row label="TaskId" value={selected.id} /><Row label="plan" value={selected.planId} /><Row label="component" value={selected.component} /><Row label="owner" value={selected.owner} /><Row label="priority / estimate" value={`${selected.priority} · ${selected.estimate}`} /><Row label="source" value={selected.source} /></div>
    <div className="wk-block"><div className="k">GRAPH REVISION</div><Row label="revision" value={`revision ${graph.revision}`} /><Row label="topology" value={`${graph.tasks.length} tasks · ${graph.relations.length} relations`} /><p className="wk-note">This selection is projected from one immutable fixture graph revision; task state does not prove an attempt or delivery outcome.</p></div>
    <div className="wk-block"><div className="k">READINESS</div>{blockers.length ? blockers.map((relation) => <button type="button" className="wk-evidence-row" key={relation.id} onClick={() => pickTask(relation.from)}><span className="is-gating"><i />GATING</span><b>{relation.from}</b><em>{relation.label}</em></button>) : <p className="wk-note">No incomplete gating predecessor. Informational, parallel, observed-order, and delegation relations never block this task.</p>}</div>
    <div className="wk-block"><div className="k">NEXT LEGAL ACTIONS</div>{actions.length ? actions.map((action) => <button type="button" className="wk-wide-action is-primary" key={action} onClick={() => setGraph((current) => applyWorkAction(current, selected.id, action))}>{actionLabel[action]} <span>→</span></button>) : <p className="wk-note">No fixture command is legal from <b>{selected.status}</b>.</p>}<p className="wk-note">Actions update this local fixture graph and append a revision event. They never invoke a daemon.</p></div>
    <div className="wk-block"><div className="k">RELATIONS</div>{relations.map((relation) => <button type="button" className="wk-evidence-row" key={relation.id} onClick={() => pickTask(relation.from === selected.id ? relation.to : relation.from)}><span className={`is-${relation.kind}`}><i />{relation.kind}</span><b>{relation.from === selected.id ? "→" : "←"} {relation.from === selected.id ? relation.to : relation.from}</b><em>{relation.grade} · {relation.label}</em></button>)}</div>
    <div className="wk-block"><div className="k">ATTEMPTS & PIVOTS</div>{selected.attempts.length ? selected.attempts.map((attempt) => <button type="button" className="wk-evidence-row" key={attempt.id} onClick={() => pivotAttempt(attempt.sessionId)}><span className={`is-${attempt.status}`}><i />{attempt.status}</span><b>{attempt.id}</b><em>{attempt.executor} · EXACT session evidence</em></button>) : <p className="wk-note">No attempt has been admitted.</p>}<div className="wk-pivots"><button type="button" onClick={() => pivot("agents")}>Agents</button><button type="button" onClick={() => pivot("code")}>Code</button><button type="button" onClick={() => pivot("delivery")}>Delivery</button><button type="button" onClick={() => setMode("snapshot")}>Snapshot</button></div></div>
    <div className="wk-block"><div className="k">OUTCOME EVIDENCE</div><p className="wk-note">No delivery outcome has been recorded. The Delivery pivot preserves {selected.id} and revision {graph.revision}; it does not infer success from task lifecycle or an attempt.</p></div>
  </div></aside></div>;
}

export function WorkPage(props: { initialSessionId?: string; onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { mode } = useDemo();
  return mode === "fixture" ? <FixtureWork /> : <SnapshotWork initialSessionId={props.initialSessionId} />;
}
