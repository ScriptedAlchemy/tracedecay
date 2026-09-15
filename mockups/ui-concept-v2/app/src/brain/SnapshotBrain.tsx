import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import manifest from "../../profile-pack/manifest.json";
import tracedecayBranch from "../../profile-pack/proj_ae394425f7837d4f/branch-meta.json";
import zeroFsBranch from "../../profile-pack/proj_e19f6f383c982ea8/branch-meta.json";
import iosBranch from "../../profile-pack/proj_8007f36f3654e9be/branch-meta.json";
import movieBranch from "../../profile-pack/proj_a812b7edf6fab331/branch-meta.json";
import mqvpnBranch from "../../profile-pack/proj_2f28a204e57c569f/branch-meta.json";
import { RepositoryAtlas } from "../structure";
import type { BrainView } from "../data/fixtures";

type RecordedProject = {
  id: string;
  label_from_store_manifest_root: string;
  project_root: string;
  row_counts: { sessions: number; observations_slim: number };
  tracedecay_db: { graph_verified_heads_v1?: number; note?: string };
};
type BranchMeta = { branches: Record<string, { last_synced_at: string }> };

const colors = ["#5ee7ff", "#67e8f9", "#f0b429", "#9be15d", "#c084fc"];
const branches: Record<string, BranchMeta> = {
  proj_ae394425f7837d4f: tracedecayBranch,
  proj_e19f6f383c982ea8: zeroFsBranch,
  proj_8007f36f3654e9be: iosBranch,
  proj_a812b7edf6fab331: movieBranch,
  proj_2f28a204e57c569f: mqvpnBranch,
};
const all = manifest.projects as RecordedProject[];
const registered = all.filter(project => typeof project.tracedecay_db.graph_verified_heads_v1 === "number");

type SnapshotProject = RecordedProject & { color: string; synced: Date | null };
type SnapshotLayout = SnapshotProject & { x: number; y: number; size: number; labelDx: number; labelDy: number };

function syncedAt(project: RecordedProject) {
  const branch = branches[project.id];
  const seconds = branch && Object.values(branch.branches)[0]?.last_synced_at;
  return seconds ? new Date(Number(seconds) * 1000) : null;
}

function circleHitsBox(circle: SnapshotLayout, box: { x: number; y: number; w: number; h: number }) {
  const x = Math.max(box.x, Math.min(circle.x, box.x + box.w));
  const y = Math.max(box.y, Math.min(circle.y, box.y + box.h));
  return Math.hypot(circle.x - x, circle.y - y) < circle.size / 2 + 3;
}

function layoutProjects(projects: SnapshotProject[], width: number, height: number) {
  const times = projects.map(project => project.synced?.getTime() ?? 0);
  const oldest = Math.min(...times), newest = Math.max(...times);
  const bodies: SnapshotLayout[] = projects.map((project, index) => {
    const mass = project.tracedecay_db.graph_verified_heads_v1 ?? 0;
    return {
      ...project,
      size: 66 + Math.sqrt(mass) * 35,
      x: 80 + ((project.synced?.getTime() ?? 0) - oldest) / Math.max(1, newest - oldest) * Math.max(1, width - 180),
      y: 110 + index * Math.max(72, (height - 190) / Math.max(1, projects.length - 1)),
      labelDx: 14,
      labelDy: 14,
    };
  });
  const ordered = [...bodies].sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  for (let pass = 0; pass < 64; pass++) {
    let moved = false;
    for (let i = 0; i < ordered.length; i++) for (let j = i + 1; j < ordered.length; j++) {
      const a = ordered[i], b = ordered[j], dy = b.y - a.y;
      const separation = Math.sqrt(Math.max(0, ((a.size + b.size) / 2 + 4) ** 2 - (b.x - a.x) ** 2));
      if (Math.abs(dy) >= separation) continue;
      const push = (separation - Math.abs(dy)) / 2 + 0.1;
      if (dy < 0 || (dy === 0 && a.id < b.id)) { a.y += push; b.y -= push; } else { a.y -= push; b.y += push; }
      moved = true;
    }
    for (const body of bodies) body.y = Math.max(body.size / 2 + 26, Math.min(height - body.size / 2 - 88, body.y));
    if (!moved) break;
  }
  const occupied: { x: number; y: number; w: number; h: number }[] = [];
  for (const body of [...bodies].sort((a, b) => a.label_from_store_manifest_root.length - b.label_from_store_manifest_root.length)) {
    const w = Math.min(205, Math.max(100, body.label_from_store_manifest_root.length * 7 + 8)), h = 62;
    const candidates = [[14, 14], [-w - 14, 14], [14, -h - 14], [-w - 14, -h - 14]].map(([dx, dy]) => ({
      x: Math.max(8, Math.min(width - w - 8, body.x + dx)), y: Math.max(24, Math.min(height - h - 8, body.y + dy)), w, h,
    }));
    const best = candidates.reduce((a, b) => {
      const cost = (box: typeof a) => occupied.reduce((sum, other) => sum + Math.max(0, Math.min(box.x + box.w, other.x + other.w) - Math.max(box.x, other.x)) * Math.max(0, Math.min(box.y + box.h, other.y + other.h) - Math.max(box.y, other.y)), 0)
        + bodies.filter(other => other !== body).reduce((sum, other) => sum + (circleHitsBox(other, box) ? 1e9 : 0), 0);
      return cost(a) <= cost(b) ? a : b;
    });
    body.labelDx = best.x - body.x;
    body.labelDy = best.y - body.y;
    occupied.push(best);
  }
  return bodies;
}

function SnapshotAbsence({ label, reason }: { label: string; reason: string }) {
  return <div className="snapshot-brain-absence"><b>{label} · UNAVAILABLE</b><span>{reason}</span></div>;
}

export function SnapshotBrain(props: {
  view: BrainView;
  setView: (view: BrainView) => void;
  focusedId: string;
  setFocusedId: (id: string) => void;
  setScoped: (scoped: boolean) => void;
}) {
  const [atlas, setAtlas] = useState(new URLSearchParams(location.search).get("atlas") === "1");
  const fieldRef = useRef<HTMLDivElement>(null);
  const [fieldSize, setFieldSize] = useState({ width: 1000, height: 600 });
  const projects = useMemo(() => registered.map((project, index) => ({
    ...project, color: colors[index], synced: syncedAt(project),
  })).sort((a, b) => (b.synced?.getTime() ?? -Infinity) - (a.synced?.getTime() ?? -Infinity)), []);
  const active = projects.find(project => project.id === props.focusedId) ?? projects[0];
  const layout = useMemo(() => layoutProjects(projects, fieldSize.width, fieldSize.height), [projects, fieldSize]);
  useEffect(() => {
    const field = fieldRef.current;
    if (!field) return;
    const measure = () => setFieldSize({ width: field.clientWidth, height: field.clientHeight });
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(field);
    return () => observer.disconnect();
  }, []);
  const activityAbsent = "No admitted dashboard activity event was copied into this profile snapshot.";

  function choose(project: typeof active, scoped = false) {
    props.setFocusedId(project.id);
    props.setScoped(scoped);
    if (scoped) props.setView("scoped");
  }
  function chooseAtlas() {
    const url = new URL(location.href);
    url.searchParams.set("atlas", "1");
    history.replaceState(null, "", url);
    setAtlas(true);
  }
  function leaveAtlas() {
    const url = new URL(location.href);
    url.searchParams.delete("atlas");
    history.replaceState(null, "", url);
    setAtlas(false);
  }
  if (atlas) return <div className="snapshot-atlas"><button type="button" onClick={leaveAtlas}>← Registry field</button><RepositoryAtlas context="brain" /></div>;

  const unavailableVisual = props.view === "firing-tree" || props.view === "neuron-lab" || props.view === "synapse" || props.view === "repo-zoom" || props.view === "scoped";
  return <section className="snapshot-brain" aria-label="Recorded Brain snapshot">
    <header>
      <div><b>RECORDED PROJECT REGISTRY</b><span>profile-pack/manifest.json · captured 2026-08-30 · read only</span></div>
      <div className="snapshot-brain-actions">
        <button type="button" onClick={() => { props.setScoped(false); props.setView("overview"); }}>Overview</button>
        <button type="button" onClick={() => props.setView("hover")}>Inspect</button>
        <button type="button" onClick={chooseAtlas}>Atlas / repository structure</button>
      </div>
    </header>
    <div className="snapshot-brain-authorities" aria-label="Independent source states">
      <span><b>REGISTRY · READY</b>{registered.length} registered projects with recorded indexed heads</span>
      <span><b>SCOPED GRAPH · UNAVAILABLE</b>graph nodes and edges were not copied</span>
      <span><b>RENDERER · READY</b>recorded registry field only</span>
      <span><b>ACTIVITY · EMPTY</b>admitted event feed copied 0 events</span>
    </div>
    {unavailableVisual ? (
      <div className="snapshot-brain-unavailable">
        <h2>{props.view === "synapse" ? "NO ADMITTED ACTIVITY TO RENDER" : props.view === "repo-zoom" ? "REPOSITORY RELATION UNAVAILABLE" : props.view === "scoped" ? "SCOPED KNOWLEDGE GRAPH UNAVAILABLE" : "NEURAL VIEW HAS NO SNAPSHOT AUTHORITY"}</h2>
        <p>{props.view === "synapse" ? activityAbsent : props.view === "repo-zoom" ? "The registry records project roots, but no project-to-repository or checkout relation was exported for this Brain view." : props.view === "scoped" ? "Indexed-head counts were copied; scoped graph identities, relations, and renderer input were not." : "This visual is a fixture renderer. The recorded snapshot has no neural scene projection, so it remains unmounted rather than inventing activity."}</p>
        <div className="snapshot-brain-unavailable-actions">
          <button type="button" onClick={() => props.setView("overview")}>Return to recorded registry</button>
          <button type="button" onClick={chooseAtlas}>Open atlas / repository structure</button>
        </div>
      </div>
    ) : (
      <div className="snapshot-brain-field" ref={fieldRef}>
        <div className="snapshot-brain-axis"><b>RECENCY · recorded branch last_synced_at</b><span>older ← → newer</span></div>
        <svg className="snapshot-label-leaders" aria-hidden="true">{layout.filter(project => Math.hypot(project.labelDx, project.labelDy) > project.size / 2 + 28).map(project => <line key={project.id} x1={project.x} y1={project.y} x2={project.x + project.labelDx} y2={project.y + project.labelDy + 8} stroke={project.color} />)}</svg>
        {layout.map((project) => {
          const mass = project.tracedecay_db.graph_verified_heads_v1 ?? 0;
          return <button key={project.id} type="button" className={`snapshot-project ${active.id === project.id ? "is-active" : ""}`}
            style={{ "--left": `${project.x}px`, "--top": `${project.y}px`, "--size": `${project.size}px`, "--label-x": `${project.labelDx}px`, "--label-y": `${project.labelDy}px`, "--color": project.color } as CSSProperties}
            onMouseEnter={() => choose(project)} onFocus={() => choose(project)} onClick={() => choose(project, true)}>
            <i /><span className="snapshot-project-label"><b>{project.label_from_store_manifest_root}</b><span>{mass} verified head{mass === 1 ? "" : "s"} · EXACT</span>
            <small>{project.synced ? project.synced.toISOString() : "recency unavailable"}</small></span>
          </button>;
        })}
        <div className="snapshot-brain-key">BODY AREA · recorded indexed mass (graph_verified_heads_v1)<br />POSITION · recorded branch recency<br />Excluded: 1 registered project has an unsealed index, not an empty healthy body.</div>
      </div>
    )}
    <aside className="snapshot-brain-inset">
      <b>SELECTED PROJECT</b>
      <strong>{active.label_from_store_manifest_root}</strong>
      <span>{active.id}</span>
      <dl><div><dt>Indexed mass</dt><dd>{active.tracedecay_db.graph_verified_heads_v1} verified heads · EXACT</dd></div><div><dt>Recency</dt><dd>{active.synced?.toISOString() ?? "UNAVAILABLE"}</dd></div><div><dt>Session rows</dt><dd>{active.row_counts.sessions} · recorded export</dd></div><div><dt>Observation rows</dt><dd>{active.row_counts.observations_slim} · recorded export</dd></div></dl>
      <SnapshotAbsence label="Activity" reason={activityAbsent} />
    </aside>
  </section>;
}
