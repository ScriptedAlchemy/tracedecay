import { useEffect, useState, type KeyboardEvent, type ReactNode } from "react";
import { useDemo, useWorkspaceState } from "../app/workspace";
import {
  DEFINITIONS, PINNED_REFS, REGISTRY_DIGESTS, RUN_ROWS, RUN_STEPS, STEPS, VERSION_GHOSTS,
  VERSION_TRACK, WORKFLOW_EDGES, WORKFLOW_NODES, WORKFLOW_PINS, WORKFLOW_RUNS,
  type WorkflowNodeState, type WorkflowRun,
} from "./data";
import "./workflows.css";

type PageProps = { onInspect?: unknown; state?: string; onState?: (id: string) => void };

function SectionK({ children }: { children: ReactNode }) {
  return <div className="wf-k">{children}</div>;
}

function GlyphCircle({ kind }: { kind: "up" | "down" | "x" }) {
  return <svg viewBox="0 0 20 20" className="wf-glyph" aria-hidden="true"><circle cx="10" cy="10" r="8.4" fill="none" strokeWidth="1.3" />{kind === "up" ? <path d="M10 14V6.6M6.8 9.6 10 6.4l3.2 3.2" fill="none" strokeWidth="1.4" /> : kind === "down" ? <path d="M10 6v7.4M6.8 10.4 10 13.6l3.2-3.2" fill="none" strokeWidth="1.4" /> : <path d="M7 7l6 6M13 7l-6 6" fill="none" strokeWidth="1.4" />}</svg>;
}

function stateForNode(run: WorkflowRun, id: string): WorkflowNodeState {
  if (run.state === "COMPLETED") return id === "s8" ? "unexercised" : "succeeded";
  if (run.state === "WAITING") return id === "s6" ? "waiting" : ["s1", "s2", "s3", "s4", "s5"].includes(id) ? "succeeded" : "unexercised";
  return id === "s3" ? "failed-attempt" : ["s1", "s2"].includes(id) ? "succeeded" : "unexercised";
}

function RegistryColumn({ selected, onSelect }: { selected: string; onSelect: (name: string) => void }) {
  const keySelect = (event: KeyboardEvent<HTMLTableRowElement>, index: number) => {
    const next = event.key === "ArrowDown" ? index + 1 : event.key === "ArrowUp" ? index - 1 : -1;
    if (next >= 0 && next < DEFINITIONS.length) {
      event.preventDefault();
      onSelect(DEFINITIONS[next].name);
      (event.currentTarget.parentElement?.children[next] as HTMLElement | undefined)?.focus();
    }
    if (event.key === "Enter" || event.key === " ") onSelect(DEFINITIONS[index].name);
  };
  return <div className="wf-col wf-col-registry">
    <section className="wf-well wf-registry" aria-label="Authored example definition registry">
      <h3 className="wf-title">DEFINITION REGISTRY <span className="dim">(AUTHORED EXAMPLES)</span></h3>
      <div className="wf-table-scroll"><table className="wf-reg-table"><thead><tr><th className="w-name">NAME</th><th className="w-ver">VERSION</th><th className="w-status">STATE</th><th className="w-ts">UPDATED (UTC)</th></tr></thead><tbody>{DEFINITIONS.map((definition, index) => <tr key={definition.name} className={selected === definition.name ? "selected" : ""} aria-selected={selected === definition.name} tabIndex={selected === definition.name ? 0 : -1} onClick={() => onSelect(definition.name)} onKeyDown={(event) => keySelect(event, index)}><td className="name" title={definition.name}>{definition.name}</td><td>{definition.version}</td><td className="active">{definition.status}</td><td className="ts">{definition.updated}</td></tr>)}</tbody></table></div>
      <footer>14 fixture definitions · selection is local view state</footer>
    </section>
    <section className="wf-well wf-digests"><h3 className="wf-title">IMMUTABLE REGISTRY PINS</h3>{REGISTRY_DIGESTS.map((digest) => <div key={digest.set} className="wf-digest-row"><span className="set">{digest.set}</span><span>{digest.id}</span><span className="sha">{digest.sha}</span></div>)}<div className="wf-pinned-at"><span className="set">observed</span><span>authored fixture · 2025-05-09 19:15:22 UTC</span></div></section>
  </div>;
}

function TruthTracks({ run, onDelivery }: { run: WorkflowRun; onDelivery: () => void }) {
  return <section className="wf-tracks" aria-label="Independent truth tracks">
    <div className="wf-track definition"><span>DEFINITION ELIGIBILITY</span><b>ACTIVE · pins matched</b><small>validation receipt · rev 4</small></div>
    <div className={`wf-track run ${run.state.toLowerCase()}`}><span>RUN TRUTH</span><b>{run.state} · {run.result}</b><small>{run.id} · {run.updated} UTC</small></div>
    <button type="button" className={`wf-track delivery ${run.delivery.state}`} onClick={onDelivery}><span>OPEN DELIVERY WITH RUN CONTEXT ↗</span><b>{run.delivery.state === "blocked" ? "BLOCKED" : "NOT ACCEPTED"}</b><small>{run.delivery.task} · authored relation, no proved deliverable join · checks {run.delivery.checks}</small></button>
  </section>;
}

function Topology({ run, compare, selectedNode, onNode }: { run: WorkflowRun; compare: boolean; selectedNode: string; onNode: (id: string) => void }) {
  const nodes = new Map(WORKFLOW_NODES.map((node) => [node.id, node]));
  const selected = nodes.get(selectedNode) ?? WORKFLOW_NODES[0];
  return <>
    <figure className="wf-topology" aria-labelledby="wf-topology-title">
      <figcaption id="wf-topology-title"><span>TYPED OPERATION DAG · {run.id}</span><em>stable recipe coordinates · run overlay</em></figcaption>
      <svg className="wf-topology-lines" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
        {WORKFLOW_EDGES.map(([from, to]) => { const a = nodes.get(from)!; const b = nodes.get(to)!; const deferred = stateForNode(run, to) !== "succeeded"; return <line key={`${from}-${to}`} className={deferred ? "deferred" : ""} x1={a.x} y1={a.y} x2={b.x} y2={b.y} />; })}
        {WORKFLOW_PINS.map((pin) => { const node = nodes.get(pin.target)!; return <line key={pin.id} className="pin-line" x1={pin.x} y1={pin.y + 6} x2={node.x} y2={node.y - 7} />; })}
      </svg>
      {compare && <div className="wf-ghost-layer" aria-label="Version 3 comparison ghost">{VERSION_GHOSTS.map((ghost) => <span key={ghost.id} className="wf-ghost" style={{ left: `${ghost.x}%`, top: `${ghost.y}%` }}>{ghost.label}<i>v3</i></span>)}</div>}
      {WORKFLOW_PINS.map((pin) => <span key={pin.id} className="wf-pin" style={{ left: `${pin.x}%`, top: `${pin.y}%` }}><b>{pin.label}</b><small>{pin.value}</small></span>)}
      {WORKFLOW_NODES.map((node) => { const nodeState = stateForNode(run, node.id); return <button key={node.id} type="button" className={`wf-node ${nodeState} ${selectedNode === node.id ? "selected" : ""}`} style={{ left: `${node.x}%`, top: `${node.y}%` }} onClick={() => onNode(node.id)} aria-pressed={selectedNode === node.id}><span>{node.id} · {nodeState.replace("-", " ")}</span><b>{node.label}</b><small>{node.operation}</small>{node.changed ? <i>{node.changed}</i> : null}</button>; })}
    </figure>
    <div className="wf-node-inspector" role="status"><span>SELECTED OPERATION · {selected.id}</span><b>{selected.operation}</b><em>{selected.input} → {selected.output}</em><small>{selected.attempt ?? (stateForNode(run, selected.id) === "unexercised" ? "No admission released this node for the selected run." : "Exact typed step projection for the fixture run.")}</small></div>
  </>;
}

function DefinitionColumn({ name, run, compare, setCompare, selectedNode, onNode, onDelivery }: { name: string; run: WorkflowRun; compare: boolean; setCompare: (value: boolean) => void; selectedNode: string; onNode: (id: string) => void; onDelivery: () => void }) {
  const definition = DEFINITIONS.find((row) => row.name === name) ?? DEFINITIONS[3];
  const detailed = definition.name === "enrich-graph";
  return <section className="wf-col wf-well wf-definition" aria-label={`Selected definition ${definition.name}`}>
    <div className="wf-definition-head"><div><h3 className="wf-title">SELECTED DEFINITION</h3><b>{definition.name}</b><span className="wf-chip">{definition.version}</span><span className="wf-active">ACTIVE</span></div><button type="button" className="wf-compare" aria-pressed={compare} onClick={() => setCompare(!compare)}>v3 GHOST {compare ? "ON" : "OFF"}</button></div>
    {detailed ? <><TruthTracks run={run} onDelivery={onDelivery} /><Topology run={run} compare={compare} selectedNode={selectedNode} onNode={onNode} /><div className="wf-definition-fallbacks"><section><SectionK>PINNED REFERENCES (IMMUTABLE)</SectionK><div className="wf-pinned-refs">{PINNED_REFS.map((ref) => <div key={ref.set}><span>{ref.set}</span><b>{ref.id}</b><small>{ref.sha}</small></div>)}</div></section><section><SectionK>VERSION HISTORY · EXACT FALLBACK</SectionK><div className="wf-version-scroll"><table className="wf-version-history"><thead><tr><th>VERSION</th><th>STATUS</th><th>CREATED (UTC)</th><th>ACTIVATED (UTC)</th><th>RETIRED (UTC)</th><th>REASON</th></tr></thead><tbody>{VERSION_TRACK.map((row) => <tr key={row[0]}>{row.map((cell, index) => <td key={index} className={cell === "ACTIVE" ? "active" : cell === "RETIRED" ? "retired" : ""}>{cell}</td>)}</tr>)}</tbody></table></div></section></div><SectionK>EXACT FALLBACK · DECODED STEP TABLE ({definition.version})</SectionK><div className="wf-table-scroll"><table className="wf-steps"><thead><tr><th>STEP</th><th>ID</th><th>OPERATION</th><th>TYPE</th><th>INPUTS</th><th>OUTPUTS</th><th>TIMEOUT</th><th>RETRIES</th></tr></thead><tbody>{STEPS.map((row) => <tr key={row[1]}><td>{row[0]}</td>{row.slice(1).map((cell, index) => <td key={index}>{cell}</td>)}</tr>)}</tbody></table></div></> : <div className="wf-unserved-definition"><b>Topology detail has not been authored for this fixture registry row.</b><span>The registry identity remains distinct from the selected enrich-graph v4 example; no graph is inferred from the name.</span></div>}
  </section>;
}

function RunColumn({ selected, onSelect, onExact }: { selected: WorkflowRun; onSelect: (run: WorkflowRun) => void; onExact: () => void }) {
  const [draft, setDraft] = useState(selected.id);
  const [lookup, setLookup] = useState(selected.id);
  const loaded = WORKFLOW_RUNS.find((run) => run.id === lookup);
  const load = () => {
    setLookup(draft);
    const run = WORKFLOW_RUNS.find((item) => item.id === draft);
    if (run) onSelect(run);
  };
  useEffect(() => { setDraft(selected.id); setLookup(selected.id); }, [selected.id]);
  return <div className="wf-col wf-col-run">
    <section className="wf-well wf-run" aria-label="Exact fixture run lookup">
      <h3 className="wf-title">RUN LOOKUP <span className="dim">(EXACT RUN-ID)</span></h3><label className="wf-runid"><span>run</span><span className="wf-input-wrap"><input aria-label="Run ID" value={draft} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => event.key === "Enter" && load()} /><button type="button" onClick={load}>LOOK UP</button></span></label>
      {loaded ? <><div className="wf-run-rows">{RUN_ROWS(loaded).map(([key, value]) => <div key={key} className="wf-run-row"><span>{key}</span><em className={value === "COMPLETED" ? "active" : value === "FAILED" || value === "failed terminal receipt" ? "failed" : value === "WAITING" || value === "no terminal receipt" ? "waiting" : ""}>{value}</em></div>)}</div><button type="button" className="wf-exact-route" onClick={onExact}>OPEN EXACT RUN ROUTE ↗</button><SectionK>DECODED STEP SEQUENCE</SectionK><div className="wf-table-scroll"><table className="wf-runsteps"><thead><tr><th>ID</th><th>OPERATION</th><th>RUN STATE</th><th>STARTED</th><th>DURATION</th></tr></thead><tbody>{RUN_STEPS(loaded).map((row) => <tr key={row[0]}><td>{row[0]}</td>{row.slice(1).map((cell, index) => <td key={index} className={cell === "success" ? "active" : cell === "retry sealed" ? "retry" : cell === "condition false" || cell === "unreleased" ? "unexercised" : cell === "waiting capacity" ? "waiting" : cell === "failed receipt" ? "failed" : ""}>{cell}</td>)}</tr>)}</tbody></table></div></> : <div className="wf-run-missing" role="status">No authored fixture matches this exact run ID. Snapshot and fixture identities never mix.</div>}
    </section>
    <section className="wf-well wf-run-ledger"><h3 className="wf-title">RUN LEDGER <span className="dim">(FIXTURE)</span></h3>{WORKFLOW_RUNS.map((run) => <button key={run.id} type="button" className={`wf-run-choice ${run.id === selected.id ? "selected" : ""}`} onClick={() => onSelect(run)}><span>{run.id}</span><b className={run.state.toLowerCase()}>{run.state}</b><small>{run.summary}</small></button>)}</section>
    <section className="wf-well wf-controls"><h3 className="wf-title">LIFECYCLE CONTROLS <span className="dim">(DAEMON-VALIDATED CAS)</span></h3>{[{ label: "Activate v5", tone: "activate", kind: "up" }, { label: "Retire v4", tone: "retire", kind: "down" }, { label: "Reject v4", tone: "reject", kind: "x" }].map((control) => <div key={control.label} className={`wf-ctl ${control.tone}`}><button type="button" disabled title="Fixture: no daemon lifecycle authority"><GlyphCircle kind={control.kind as "up" | "down" | "x"} /><b>{control.label}</b></button><span>expected_rev = 3 · unavailable</span></div>)}</section>
  </div>;
}

function Snapshot() {
  return <div className="wf-snapshot"><section className="wf-well"><h3 className="wf-title">WORKFLOW AUTHORITIES UNAVAILABLE</h3><b>No definition registry, immutable pins, lifecycle receipt, or exact run projection is served in this snapshot.</b><p>The stable surface remains available for navigation, but there is no workflow graph or run truth to render.</p></section><section className="wf-well"><h3 className="wf-title">EXACT FALLBACK</h3><table><thead><tr><th>DEFINITION</th><th>VERSION</th><th>RUN</th><th>STATE</th><th>AUTHORITY</th></tr></thead><tbody><tr><td>unavailable</td><td>unavailable</td><td>unavailable</td><td>no projection</td><td>workflow registry not served</td></tr></tbody></table></section></div>;
}

export function WorkflowsPage({ state, onState }: PageProps = {}) {
  const { mode, navigate } = useDemo();
  const fixture = mode === "fixture";
  const requestedRun = new URLSearchParams(location.search).get("run");
  const initialDefinition = DEFINITIONS.some((row) => row.name === state) ? state! : "enrich-graph";
  const [selectedDefinition, setSelectedDefinition] = useWorkspaceState("workflows:definition", initialDefinition);
  const [selectedRunId, setSelectedRunId] = useWorkspaceState("workflows:run", WORKFLOW_RUNS.some((run) => run.id === requestedRun) ? requestedRun! : "run_7f3c9a");
  const [compare, setCompare] = useWorkspaceState("workflows:compare-v3", true);
  const [selectedNode, setSelectedNode] = useWorkspaceState("workflows:node", "s5");
  const selectedRun = WORKFLOW_RUNS.find((run) => run.id === selectedRunId) ?? WORKFLOW_RUNS[0];
  useEffect(() => {
    const definition = DEFINITIONS.find((row) => row.name === state);
    const run = WORKFLOW_RUNS.find((item) => item.id === requestedRun || item.id === state);
    if (definition) setSelectedDefinition(definition.name);
    if (run) { setSelectedRunId(run.id); setSelectedDefinition(run.definition); }
  }, [requestedRun, setSelectedDefinition, setSelectedRunId, state]);
  if (!fixture) return <div className="wf-root"><div className="wf-truth">SNAPSHOT · WORKFLOW AUTHORITY UNAVAILABLE</div><Snapshot /></div>;
  const selectDefinition = (name: string) => { setSelectedDefinition(name); onState?.(name); };
  const selectRun = (run: WorkflowRun) => { setSelectedRunId(run.id); setSelectedDefinition(run.definition); onState?.(run.definition); };
  const exactRunRoute = () => navigate("workflows", { state: selectedDefinition, run: selectedRun.id });
  const deliveryRoute = () => navigate("delivery", { state: "04", workflow: selectedDefinition, run: selectedRun.id, task: selectedRun.delivery.task });
  return <div className="wf-root"><div className="wf-truth">AUTHORED FIXTURE · SYNTHETIC DATA · LIFECYCLE COMMANDS UNAVAILABLE</div><div className="wf-main"><RegistryColumn selected={selectedDefinition} onSelect={selectDefinition} /><DefinitionColumn name={selectedDefinition} run={selectedRun} compare={compare} setCompare={setCompare} selectedNode={selectedNode} onNode={setSelectedNode} onDelivery={deliveryRoute} /><RunColumn selected={selectedRun} onSelect={selectRun} onExact={exactRunRoute} /></div></div>;
}
