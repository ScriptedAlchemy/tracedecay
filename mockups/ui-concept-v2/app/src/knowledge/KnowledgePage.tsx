import { useEffect, useState, type ReactNode } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { PACK } from "../data/pack";
import { RepositoryAtlas, atlasData, nodeById, type AtlasNode } from "../structure";
import "./knowledge.css";

const CAMERAS = ["FACTS", "GEOMETRY", "CURATION", "OPLOG"] as const;
type Cam = (typeof CAMERAS)[number];
type FactState = "canonical" | "superseded" | "contradicted";
type Source = { label: string; path: string; revision: "baseline" | "change" };
type ExampleFact = {
  id: string;
  subject: string;
  content: string;
  state: FactState;
  path: string;
  sources: [Source, Source];
  relation?: { kind: "superseded by" | "contradicted by"; target: string };
};

const ANCHORS = PACK.projects.reduce((sum, project) => sum + (project.retrievalAnchors ?? 0), 0);
const PROJECTS_ABSENT = PACK.projects.filter((project) => project.factsTable === "absent" || project.factsTable == null).length;

// Authored interaction fixture. These claims demonstrate product behavior; they are
// deliberately separate from the empty snapshot facts authority.
const EXAMPLE_FACTS: ExampleFact[] = [
  {
    id: "runtime-v1-owner",
    subject: "Project store runtime ownership",
    content: "ProjectStoreRuntimeV1 is the project store access owner.",
    state: "superseded",
    path: "crates/tracedecay/src/project_store_runtime.rs",
    sources: [
      { label: "baseline forwarding module", path: "crates/tracedecay/src/project_store_runtime.rs", revision: "baseline" },
      { label: "baseline application port", path: "crates/tracedecay-application/src/ports/project_store_runtime.rs", revision: "baseline" },
    ],
    relation: { kind: "superseded by", target: "runtime-direct-owner" },
  },
  {
    id: "runtime-direct-owner",
    subject: "Project store runtime ownership",
    content: "DaemonSessionRuntimeRegistryV1 is the canonical runtime owner after the focused refactor.",
    state: "canonical",
    path: "crates/tracedecay/src/tracedecay/lifecycle/mod.rs",
    sources: [
      { label: "current lifecycle owner", path: "crates/tracedecay/src/tracedecay/lifecycle/mod.rs", revision: "change" },
      { label: "current branch wiring", path: "crates/tracedecay/src/tracedecay/lifecycle/branches.rs", revision: "change" },
    ],
  },
  {
    id: "store-boundary-gone",
    subject: "Store runtime boundary",
    content: "The store runtime crate was removed with the forwarding layer.",
    state: "contradicted",
    path: "crates/tracedecay-store-runtime",
    sources: [
      { label: "remaining crate manifest", path: "crates/tracedecay-store-runtime/Cargo.toml", revision: "change" },
      { label: "remaining workspace member", path: "Cargo.toml", revision: "change" },
    ],
    relation: { kind: "contradicted by", target: "runtime-direct-owner" },
  },
];

const FACT_BY_ID = new Map(EXAMPLE_FACTS.map((fact) => [fact.id, fact]));

function Row(props: { label: string; value: string; unavailable?: boolean }) {
  return (
    <div className="kn-row">
      <span>{props.label}</span>
      <span className={props.unavailable ? "r abs" : "r"}>{props.value}</span>
    </div>
  );
}

function InspectFrame(props: { label: string; hint: string; foot?: ReactNode; children: ReactNode }) {
  return (
    <aside className="kn-inspect" aria-label={props.label}>
      <Corners />
      <div className="kn-inspect-body">{props.children}</div>
      {props.foot}
      <div className="kn-hint">{props.hint}</div>
    </aside>
  );
}

function SnapshotInspector(props: { cam: Cam; node: AtlasNode }) {
  return (
    <InspectFrame
      label={`${props.cam.toLowerCase()} snapshot inspector`}
      hint="The repository subject scaffold is source-backed. Knowledge cameras remain independently unavailable."
      foot={<div className="kn-foot"><div className="k">SNAPSHOT AUTHORITY</div><Row label="retrieval anchors" value={String(ANCHORS)} /><Row label="facts_table=absent" value={`${PROJECTS_ABSENT} / ${PACK.projects.length}`} /></div>}
    >
      <div className="kn-inspect-head"><span>{props.cam} CAMERA</span><b>NOT SERVED</b></div>
      <div className="kn-title"><h2>{props.node.path || "repository"}</h2></div>
      <div className="kn-block">
        <div className="k">SELECTED SOURCE SUBJECT</div>
        <Row label="kind" value={props.node.kind} />
        <Row label="contained files" value={String(props.node.files)} />
        <Row label="revision" value={atlasData.revision.slice(0, 12)} />
      </div>
      <div className="kn-block">
        <div className="k">KNOWLEDGE ATTACHMENTS</div>
        <Row label="claims" value="unavailable" unavailable />
        <Row label="contradictions" value="unavailable" unavailable />
        <Row label="supersession" value="unavailable" unavailable />
        <Row label="trust history" value="unavailable" unavailable />
      </div>
      <div className="kn-canon">No production fact is attached to this source subject. Source structure does not imply memory.</div>
    </InspectFrame>
  );
}

function FixtureInspector(props: { fact: ExampleFact; node: AtlasNode; cam: Cam; onSelect: (fact: ExampleFact) => void }) {
  const { navigate } = useDemo();
  const related = props.fact.relation ? FACT_BY_ID.get(props.fact.relation.target) : undefined;
  return (
    <InspectFrame label="Authored example claim inspector" hint="AUTHORED EXAMPLE DATA. Use Snapshot mode to inspect real availability without demonstration claims.">
      <div className="kn-inspect-head"><span>{props.cam} · CLAIM</span><b>AUTHORED EXAMPLE</b></div>
      <div className="kn-title"><h2>{props.fact.subject}</h2><span className={`kn-state is-${props.fact.state}`}>{props.fact.state}</span></div>
      <div className="kn-block">
        <div className="k">RETAINED CLAIM CONTENT</div>
        <div className="kn-canon">{props.fact.content}</div>
      </div>
      <div className="kn-block">
        <div className="k">TWO-SOURCE PROVENANCE</div>
        {props.fact.sources.map((source) => (
          <button className="kn-source" type="button" key={source.path} onClick={() => navigate("code", { node: source.path, path: source.path })}>
            <b>{source.label}</b><span>{source.path}</span><em>{source.revision === "baseline" ? atlasData.baselineRevision.slice(0, 8) : atlasData.changeRevision.slice(0, 8)} ↗</em>
          </button>
        ))}
      </div>
      <div className="kn-block">
        <div className="k">RELATIONSHIP</div>
        {related && props.fact.relation ? (
          <button className="kn-related" type="button" onClick={() => props.onSelect(related)}>
            <span>{props.fact.relation.kind}</span><b>{related.content}</b>
          </button>
        ) : <Row label="status" value="current fixture claim" />}
      </div>
      <div className="kn-block">
        <div className="k">EVIDENCE SCOPE</div>
        <Row label="identity" value={props.fact.id} />
        <Row label="selected source subject" value={props.node.path || "/"} />
        <Row label="source count" value="2" />
        <Row label="production memory" value="not claimed" unavailable />
      </div>
      <div className="kn-block">
        <div className="k">TRUST HISTORY · SOURCE VERIFICATION</div>
        <Row label="observed" value="2 retained source records · exact" />
        <Row label="verification" value="fixture revision identity checked" />
        <Row label="trust method" value="source-counted signal · not truth" />
      </div>
      <div className="kn-block">
        <div className="k">REDACTION</div>
        <Row label="content availability" value="shown fixture claim; no withheld value" />
        <Row label="policy receipt" value="unavailable — fixture has no policy authority" unavailable />
      </div>
    </InspectFrame>
  );
}

function EmptyFactsTable() {
  return (
    <section className="kn-table" aria-label="Facts ledger empty">
      <header><b>FACT LEDGER</b><span>facts table absent</span></header>
      <div className="kn-empty-ledger"><strong>0 FACTS ATTACHED</strong><span>source subjects remain navigable</span></div>
      <footer><span>showing 0 of 0 · retrieval_anchors={ANCHORS}</span><span>snapshot authority</span></footer>
    </section>
  );
}

function FixtureLedger(props: { selected: string; onSelect: (fact: ExampleFact) => void }) {
  return (
    <section className="kn-table kn-fixture-ledger" aria-label="Authored example facts ledger">
      <header><b>CLAIM LEDGER</b><span>AUTHORED EXAMPLE · {EXAMPLE_FACTS.length} claims</span></header>
      <div className="kn-table-scroll">
        <table><thead><tr><th>CLAIM</th><th>SUBJECT</th><th>STATE</th><th>SOURCES</th></tr></thead>
          <tbody>{EXAMPLE_FACTS.map((fact) => (
            <tr key={fact.id} className={fact.id === props.selected ? "is-selected" : ""}>
              <td><button type="button" onClick={() => props.onSelect(fact)}>{fact.content}</button></td>
              <td>{fact.subject}</td><td><span className={`kn-state is-${fact.state}`}>{fact.state}</span></td><td>{fact.sources.length}</td>
            </tr>
          ))}</tbody>
        </table>
      </div>
      <footer><span>demonstration data · no production facts implied</span><span>selection retained</span></footer>
    </section>
  );
}

function SubjectMap(props: { selection: string; onSelect: (node: AtlasNode) => void; fixture: boolean; fact?: ExampleFact; onSelectFact?: () => void }) {
  return (
    <section className="kn-well kn-subject-map" aria-label="Repository source subjects">
      <header><b>SOURCE SUBJECT MAP</b><span>{props.fixture ? "fixture claims attached" : "0 facts attached · structure only"}</span></header>
      <div className="kn-atlas-wrap">
        <RepositoryAtlas context="explorer" compact initialSelection={props.selection} onSelect={props.onSelect} />
        <div className="kn-map-label"><strong>{props.fixture ? "REPOSITORY STRUCTURE / CLAIM MEMBERSHIP" : "EXTRACTED REPOSITORY SUBJECTS"}</strong><span>fixed source geometry · Git {atlasData.revision.slice(0, 8)}</span>{props.fact ? <button type="button" onClick={props.onSelectFact}>MAP MEMBERSHIP · {props.fact.id}</button> : null}</div>
      </div>
    </section>
  );
}

function GeometryCamera(props: { selected: ExampleFact; onSelect: (fact: ExampleFact) => void }) {
  const positions: Record<string, { x: number; y: number }> = {
    "runtime-v1-owner": { x: 23, y: 55 },
    "runtime-direct-owner": { x: 53, y: 32 },
    "store-boundary-gone": { x: 76, y: 67 },
  };
  return (
    <section className="kn-well kn-fill" aria-label="Authored example claim geometry">
      <header><b>CLAIM GEOMETRY</b><span>AUTHORED EXAMPLE · READY · 3 memberships</span></header>
      <div className="kn-geometry">
        <svg viewBox="0 0 100 100" aria-hidden="true"><path d="M23 55 Q38 28 53 32"/><path className="is-contradiction" d="M76 67 Q68 42 53 32"/></svg>
        {EXAMPLE_FACTS.map((fact) => <button type="button" key={fact.id} className={`${fact.id === props.selected.id ? "is-selected " : ""}is-${fact.state}`} style={{ left: `${positions[fact.id].x}%`, top: `${positions[fact.id].y}%` }} onClick={() => props.onSelect(fact)}><i/><b>{fact.subject}</b><span>{fact.state}</span></button>)}
        <div className="kn-geometry-key"><span>solid = supersession</span><span>dashed = contradiction</span><span>positions are authored</span></div>
      </div>
    </section>
  );
}

function CurationCamera(props: { selected: ExampleFact; onSelect: (fact: ExampleFact) => void }) {
  const groups: FactState[] = ["superseded", "canonical", "contradicted"];
  return (
    <section className="kn-well kn-fill" aria-label="Authored example curation board">
      <header><b>CURATION BOARD</b><span>AUTHORED EXAMPLE · READY · 3 exact fixture receipts</span></header>
      <div className="kn-curation">{groups.map((state) => <section key={state}><header><b>{state.toUpperCase()}</b><span>{EXAMPLE_FACTS.filter((fact) => fact.state === state).length}</span></header>{EXAMPLE_FACTS.filter((fact) => fact.state === state).map((fact) => <button type="button" key={fact.id} className={fact.id === props.selected.id ? "is-selected" : ""} onClick={() => props.onSelect(fact)}><strong>{fact.subject}</strong><span>{fact.content}</span><em>{fact.sources.length} sources</em></button>)}</section>)}</div>
    </section>
  );
}

function OplogCamera(props: { onSelect: (fact: ExampleFact) => void }) {
  const events = [
    { revision: atlasData.baselineRevision, action: "claim observed", fact: EXAMPLE_FACTS[0] },
    { revision: atlasData.changeRevision, action: "canonical owner changed", fact: EXAMPLE_FACTS[1] },
    { revision: atlasData.changeRevision, action: "prior claim superseded", fact: EXAMPLE_FACTS[0] },
    { revision: atlasData.changeRevision, action: "boundary deletion claim contradicted", fact: EXAMPLE_FACTS[2] },
  ];
  return (
    <section className="kn-well kn-fill" aria-label="Authored example knowledge oplog">
      <header><b>CLAIM OPLOG</b><span>AUTHORED EXAMPLE · READY · {events.length} exact fixture rows</span></header>
      <ol className="kn-oplog">{events.map((event, index) => <li key={`${event.fact.id}-${index}`}><i/><button type="button" onClick={() => props.onSelect(event.fact)}><span>{event.revision.slice(0, 8)}</span><b>{event.action}</b><em>{event.fact.subject}</em></button></li>)}</ol>
    </section>
  );
}

function SnapshotAbsence(props: { cam: Cam }) {
  const copy: Record<Exclude<Cam, "FACTS">, string> = {
    GEOMETRY: "No production UMAP or semantic projection is served. Source containment cannot stand in for knowledge geometry.",
    CURATION: "No production permit, curation queue, contradiction decision, or supersession state is present.",
    OPLOG: "Observation payloads and first/last-seen records were not copied into this snapshot.",
  };
  return (
    <section className="kn-well kn-fill" aria-label={`${props.cam.toLowerCase()} camera unavailable`}>
      <header><b>{props.cam}</b><span>UNAVAILABLE · independent authority not served</span></header>
      <div className="kn-absent"><span className="kn-badge">unavailable</span><div className="kn-absent-title">{props.cam.toLowerCase()} authority unavailable</div><p>{copy[props.cam as Exclude<Cam, "FACTS">]}</p></div>
    </section>
  );
}

function cameraFromUrl(): Cam {
  const requested = new URLSearchParams(window.location.search).get("knowledge_camera");
  return CAMERAS.find((camera) => camera.toLowerCase() === requested) ?? "FACTS";
}

function factFromUrl(): ExampleFact | undefined {
  const requested = new URLSearchParams(window.location.search).get("knowledge_fact");
  return requested ? FACT_BY_ID.get(requested) : undefined;
}

export function KnowledgePage(_props: { state?: string; onState?: (id: string) => void } = {}) {
  const { mode } = useDemo();
  const explicitNode = new URLSearchParams(window.location.search).get("node")
    || new URLSearchParams(window.location.search).get("path");
  const [cam, setCam] = useWorkspaceState<Cam>("knowledge.camera", cameraFromUrl());
  const [atlasSelection, setAtlasSelection] = useWorkspaceState<string>("atlas.selection", explicitNode || "crates/tracedecay");
  const [factId, setFactId] = useWorkspaceState<string>("knowledge.fact.selection", factFromUrl()?.id ?? "runtime-direct-owner");
  const [query, setQuery] = useState(() => new URLSearchParams(window.location.search).get("knowledge_query") ?? "");
  const [filter, setFilter] = useState(() => new URLSearchParams(window.location.search).get("knowledge_filter") ?? "all");
  const [sort, setSort] = useState(() => new URLSearchParams(window.location.search).get("knowledge_sort") ?? "subject");
  const [page, setPage] = useState(() => new URLSearchParams(window.location.search).get("knowledge_page") ?? "1");
  const fact = FACT_BY_ID.get(factId) ?? EXAMPLE_FACTS[1];
  const node = nodeById.get(atlasSelection) ?? nodeById.get("crates/tracedecay") ?? atlasData.nodes[0];

  useEffect(() => { _props.onState?.(cam.toLowerCase()); }, [cam, _props.onState]);
  useEffect(() => {
    const restore = () => {
      setCam(cameraFromUrl());
      const requestedNode = new URLSearchParams(window.location.search).get("node")
        || new URLSearchParams(window.location.search).get("path");
      if (requestedNode && nodeById.has(requestedNode)) setAtlasSelection(requestedNode);
      const requestedFact = factFromUrl();
      if (requestedFact) {
        setFactId(requestedFact.id);
        setAtlasSelection(requestedFact.path);
      }
    };
    restore();
    window.addEventListener("popstate", restore);
    return () => window.removeEventListener("popstate", restore);
  }, [setAtlasSelection, setCam]);

  function selectCamera(camera: Cam) {
    if (camera === cam) return;
    setCam(camera);
    const url = new URL(window.location.href);
    url.searchParams.set("knowledge_camera", camera.toLowerCase());
    window.history.pushState(null, "", url);
  }

  function updateFactsRoute(key: string, value: string) {
    const url = new URL(window.location.href);
    url.searchParams.set(key, value);
    window.history.replaceState(null, "", url);
  }

  function selectFact(next: ExampleFact) {
    if (next.id === fact.id) return;
    const current = new URL(window.location.href);
    if (!current.searchParams.has("knowledge_fact")) {
      current.searchParams.set("knowledge_fact", fact.id);
      window.history.replaceState(null, "", current);
    }
    const url = new URL(window.location.href);
    url.searchParams.set("knowledge_fact", next.id);
    window.history.pushState(null, "", url);
    setFactId(next.id);
    setAtlasSelection(next.path);
  }

  function selectSubject(next: AtlasNode) {
    setAtlasSelection(next.id);
    const attached = mode === "fixture" ? EXAMPLE_FACTS.find((candidate) => next.id === candidate.path || next.id.startsWith(`${candidate.path}/`)) : undefined;
    if (attached) selectFact(attached);
  }

  return (
    <div className="kn-root">
      <div className="kn-main">
        <div className="kn-controls">
          <div className="kn-tabs" role="tablist" aria-label="Knowledge cameras">
            {CAMERAS.map((camera, index) => (
              <button key={camera} type="button" role="tab" aria-label={camera} id={`kn-tab-${camera}`} aria-selected={cam === camera}
                aria-controls="kn-camera-panel" tabIndex={cam === camera ? 0 : -1}
                onKeyDown={(event) => {
                  const target = event.key === "Home" ? 0 : event.key === "End" ? CAMERAS.length - 1
                    : event.key === "ArrowRight" ? (index + 1) % CAMERAS.length
                    : event.key === "ArrowLeft" ? (index + CAMERAS.length - 1) % CAMERAS.length : null;
                  if (target === null) return;
                  event.preventDefault(); selectCamera(CAMERAS[target]);
                  event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]")[target]?.focus();
                }}
                className={cam === camera ? "is-on" : ""} onClick={() => selectCamera(camera)}>{camera} <small>{mode === "snapshot" ? (camera === "FACTS" ? "ABSENT" : "UNAVAILABLE") : camera === "FACTS" ? "3" : camera === "OPLOG" ? "4" : "READY"}</small></button>
            ))}
          </div>
          <span className={`kn-mode is-${mode}`}>{mode === "fixture" ? "AUTHORED EXAMPLE DATA" : "SNAPSHOT · FACTS ABSENT"}</span>
        </div>
        <div className="kn-route-state" aria-label="Knowledge route controls">
          <label>QUERY <input value={query} onChange={(event) => { setQuery(event.target.value); updateFactsRoute("knowledge_query", event.target.value); }} /></label>
          <label>FILTER <select value={filter} onChange={(event) => { setFilter(event.target.value); updateFactsRoute("knowledge_filter", event.target.value); }}><option value="all">all claims</option><option value="canonical">canonical</option><option value="changed">changed</option></select></label>
          <label>SORT <select value={sort} onChange={(event) => { setSort(event.target.value); updateFactsRoute("knowledge_sort", event.target.value); }}><option value="subject">subject</option><option value="state">state</option></select></label>
          <label>PAGE <select value={page} onChange={(event) => { setPage(event.target.value); updateFactsRoute("knowledge_page", event.target.value); }}><option value="1">1</option></select></label>
          <span>SELECTION · {fact.id}</span>
        </div>
        <div id="kn-camera-panel" className="kn-camera-panel" role="tabpanel" aria-labelledby={`kn-tab-${cam}`}>
          {mode === "snapshot" ? (
            cam === "FACTS" ? <><SubjectMap selection={node.id} onSelect={selectSubject} fixture={false} /><EmptyFactsTable /></> : <SnapshotAbsence cam={cam} />
          ) : cam === "FACTS" ? <><SubjectMap selection={node.id} onSelect={selectSubject} fixture fact={fact} onSelectFact={() => selectFact(fact)} /><FixtureLedger selected={fact.id} onSelect={selectFact} /></>
            : cam === "GEOMETRY" ? <GeometryCamera selected={fact} onSelect={selectFact} />
            : cam === "CURATION" ? <CurationCamera selected={fact} onSelect={selectFact} />
            : <OplogCamera onSelect={selectFact} />}
        </div>
      </div>
      {mode === "snapshot" ? <SnapshotInspector cam={cam} node={node} /> : <FixtureInspector fact={fact} node={node} cam={cam} onSelect={selectFact} />}
    </div>
  );
}
