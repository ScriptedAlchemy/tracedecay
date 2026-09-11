import { useEffect, useMemo, useRef, useState } from "react";
import { PROJECTS, PROFILE_SOURCE, SIGNAL_FAMILIES, SYNAPSE_EVENT, type BrainView, type ProjectBody } from "../data/fixtures";
import { buildScopedGraph } from "./scopedGraph";

export function BrainEvidence(props: {
  view: BrainView;
  project: ProjectBody;
  onInspect: (id: string) => void;
  onSelect: (id: string) => void;
  onCheckout: (alias: string) => void;
  activityRequest: number;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { if (props.activityRequest > 0) dialog.current?.showModal(); }, [props.activityRequest]);
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<"name" | "mass">("name");
  const graph = useMemo(() => props.view === "scoped" ? buildScopedGraph(props.project) : null, [props.project, props.view]);
  const rows = [...PROJECTS].filter((p) => `${p.name} ${p.id} ${p.canonicalRoot ?? ""}`.toLowerCase().includes(search.toLowerCase()))
    .sort((a, b) => sort === "mass" ? b.indexedMass - a.indexedMass : a.name.localeCompare(b.name));
  const title = graph ? "Scoped evidence" : props.view === "repo-zoom" ? "Repository table" : props.view === "synapse" ? "Activity source" : "Project registry";
  const close = () => dialog.current?.close();
  return <>
    {graph && <div className="scoped-metrics" aria-label="Exported graph summary">
      {[["Nodes", graph.nodes.length], ["Relations", graph.edges.length], ["Files", "UNAVAILABLE"], ["Facts", "UNAVAILABLE"], ["Groups", graph.clusters.length], ["Events", "UNAVAILABLE"]].map(([label, value]) =>
        <div key={label}><span>{label}</span><strong>{value}</strong><small>{typeof value === "number" ? "exported projection" : "not in this projection"}</small></div>)}
    </div>}
    <button type="button" className="brain-evidence-button" onClick={() => dialog.current?.showModal()}>{title} ↗</button>
    <dialog ref={dialog} className="brain-evidence" onCancel={(event) => event.stopPropagation()} onKeyDown={(event) => event.stopPropagation()}>
      <header><h2>{title}</h2><button type="button" onClick={close}>Close</button></header>
      <p>STALE · static export captured {PROFILE_SOURCE.capturedAt}. These values are not a live authority.</p>
      {props.view === "synapse" ? <>
        <p>Concept activity sample · not a newly admitted live event. Sibling propagation: 0%.</p>
        <dl>{[["Touched identity", SYNAPSE_EVENT.projectId], ["Family", SYNAPSE_EVENT.family], ["Stream", SYNAPSE_EVENT.streamId], ["Timestamp", SYNAPSE_EVENT.at], ["Source event ID", "UNAVAILABLE"], ["Source transcript", "UNAVAILABLE in this export"]].map(([k, v]) => <div key={k}><dt>{k}</dt><dd>{v}</dd></div>)}</dl>
        <button type="button" onClick={() => { close(); props.onSelect(SYNAPSE_EVENT.projectId); }}>Open touched project</button>
      </> : props.view === "repo-zoom" ? <table><caption>{props.project.name} · exported checkout registry · holdings per checkout unavailable</caption><thead><tr><th>Checkout</th><th>Source path</th><th>Last seen in export</th><th>Action</th></tr></thead><tbody>{props.project.checkouts.map((c) => <tr key={c.alias}><th>{c.alias}</th><td>{c.path}</td><td>{c.lastSeen === "—" ? "UNAVAILABLE" : c.lastSeen}</td><td><button type="button" onClick={() => { close(); props.onCheckout(c.alias); }}>Inspect {c.alias}</button></td></tr>)}</tbody></table>
      : graph ? <table><caption>{graph.nodes.length} projected identities · {graph.edges.length} relations · {graph.absences.join("; ")}</caption><thead><tr><th>Identity</th><th>Group</th><th>Relation endpoints</th></tr></thead><tbody>{graph.nodes.map((node) => <tr key={node.id}><th>{node.label}<small>{node.id}</small></th><td>{node.cluster}</td><td>{graph.edges.filter((e) => e.a === node.id || e.b === node.id).map((e) => e.a === node.id ? e.b : e.a).join(", ") || "None in export"}</td></tr>)}</tbody></table>
      : <>
        <details><summary>Activity family legend · concept inset</summary><ul>{SIGNAL_FAMILIES.map((family) => <li key={family.name}>{family.name}: {family.count.toLocaleString()}</li>)}</ul><p>Accepted frames: 0. Static concept counts, not live telemetry.</p></details>
        <div className="registry-filters"><label>Find project <input type="search" value={search} onChange={(event) => setSearch(event.target.value)} /></label><label>Sort <select value={sort} onChange={(event) => setSort(event.target.value as "name" | "mass")}><option value="name">Name</option><option value="mass">Indexed mass</option></select></label></div>
        <table><caption>{rows.length} matching projects · inspection preserves scope</caption><thead><tr><th>Project / source identity</th><th>Stores</th><th>Artifacts</th><th>Indexed mass</th><th>Recency in export</th><th>Actions</th></tr></thead><tbody>{rows.map((p) => <tr key={p.id}><th>{p.name}<small>{p.id}</small></th><td>{p.storeCount}</td><td>{p.artifactCount}</td><td>{p.indexedMass}</td><td>{p.recency} · {p.age}</td><td><button type="button" onClick={() => { close(); props.onInspect(p.id); }}>Inspect {p.name}</button><button type="button" onClick={() => { close(); props.onSelect(p.id); }}>Open {p.name}</button></td></tr>)}</tbody></table>
      </>}
    </dialog>
  </>;
}
