import { useMemo, useState, type CSSProperties } from "react";
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

function syncedAt(project: RecordedProject) {
  const branch = branches[project.id];
  const seconds = branch && Object.values(branch.branches)[0]?.last_synced_at;
  return seconds ? new Date(Number(seconds) * 1000) : null;
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
  const projects = useMemo(() => registered.map((project, index) => ({
    ...project, color: colors[index], synced: syncedAt(project),
  })).sort((a, b) => (b.synced?.getTime() ?? -Infinity) - (a.synced?.getTime() ?? -Infinity)), []);
  const active = projects.find(project => project.id === props.focusedId) ?? projects[0];
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
      <div className="snapshot-brain-field">
        <div className="snapshot-brain-axis"><b>RECENCY · recorded branch last_synced_at</b><span>older ← → newer</span></div>
        {projects.map((project, index) => {
          const mass = project.tracedecay_db.graph_verified_heads_v1 ?? 0;
          const width = 66 + Math.sqrt(mass) * 35;
          const left = 10 + ((project.synced?.getTime() ?? 0) - Math.min(...projects.map(item => item.synced?.getTime() ?? 0))) / Math.max(1, Math.max(...projects.map(item => item.synced?.getTime() ?? 0)) - Math.min(...projects.map(item => item.synced?.getTime() ?? 0))) * 75;
          return <button key={project.id} type="button" className={`snapshot-project ${active.id === project.id ? "is-active" : ""}`}
            style={{ "--left": `${left}%`, "--size": `${width}px`, "--color": project.color, "--row": index % 3 } as CSSProperties}
            onMouseEnter={() => choose(project)} onFocus={() => choose(project)} onClick={() => choose(project, true)}>
            <i /><b>{project.label_from_store_manifest_root}</b><span>{mass} verified head{mass === 1 ? "" : "s"} · EXACT</span>
            <small>{project.synced ? project.synced.toISOString() : "recency unavailable"}</small>
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
