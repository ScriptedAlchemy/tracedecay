import { HUB, PROJECTS, PROFILE_SOURCE, RECENCY_AXIS, SYNAPSE_EVENT, type BrainView, type ProjectBody, type Surface } from "../data/fixtures";
import { Corners } from "../app/shell/Corners";
import type { SurfaceInspect } from "../surfaces/inspect";

function fmt(n: number) {
  return n.toLocaleString("en-US");
}

function recencyLabel(id: string) {
  return RECENCY_AXIS.find((t) => t.id === id)?.label ?? id;
}

export function Inspector(props: {
  view: BrainView;
  project: ProjectBody | null;
  labScope?: string | null;
  surface?: Surface;
  inspect?: SurfaceInspect | null;
}) {
  if (props.surface && props.surface !== "brain") {
    const i = props.inspect;
    if (!i) return <aside className="inspector"><Corners /></aside>;
    return (
      <aside className="inspector">
        <Corners />
        <h2>{i.title}</h2>
        <div className="kind">{i.kind}</div>
        {i.id ? <div className="mono-id">ID: {i.id}</div> : null}
        {i.sections.map((s) => (
          <div className="kv" key={s.k}>
            <div className="k">{s.k}</div>
            {s.rows.map((r, n) => (
              <div className="v" key={`${s.k}-${n}`}>
                <span>{r.l}</span>
                {r.r != null ? <span>{r.r}</span> : null}
              </div>
            ))}
          </div>
        ))}
        <div className="hint">
          <span className="info" aria-hidden="true">i</span>
          <p>{i.hint.split("\n").map((line, idx) => (
            <span key={idx}>{idx ? <br /> : null}{line}</span>
          ))}</p>
        </div>
      </aside>
    );
  }
  const p = props.project ?? PROJECTS[0];
  const labScoped = props.view === "neuron-lab" && Boolean(props.labScope);
  if (props.view === "synapse") {
    return (
      <aside className="inspector">
        <Corners />
        <h2>ACCEPTED ACTIVITY</h2>
        <div className="kind">CONCEPT SAMPLE · NOT LIVE</div>
        <div className="mono-id">ID: {SYNAPSE_EVENT.projectId}</div>
        <div className="kv">
          <div className="v"><span>family</span><span>{SYNAPSE_EVENT.family}</span></div>
          <div className="v"><span>streamId</span><span>{SYNAPSE_EVENT.streamId}</span></div>
          <div className="v"><span>at</span><span>{SYNAPSE_EVENT.at}</span></div>
        </div>
        <BodyFacts p={p} extra />
        <Hint />
      </aside>
    );
  }
  if (props.view === "scoped" || labScoped) {
    return (
      <aside className="inspector">
        <Corners />
        <h2>{p.name}</h2>
        <div className="kind">PROJECT IDENTITY</div>
        <SourceStatus />
        <div className="mono-id">ID: {p.id}</div>
        <div className="kv">
          <div className="k">CANONICAL ROOT</div>
          <div className="v">{p.canonicalRoot ?? "UNAVAILABLE"}</div>
        </div>
        <div className="kv">
          <div className="k">DEFAULT BRANCH</div>
          <div className="v">{p.defaultBranch ?? "UNAVAILABLE"}</div>
        </div>
        <Checkouts p={p} />
        <Hint labScoped={labScoped} />
      </aside>
    );
  }
  if (props.view === "repo-zoom") {
    return (
      <aside className="inspector">
        <Corners />
        <h2>REPOSITORY</h2>
        <div className="kind">SHARED CHECKOUT STRUCTURE</div>
        <div className="kv">
          <div className="k">HUB</div>
          <div className="v">{HUB.label}</div>
          <div className="v">massless · does not scope</div>
        </div>
        <div className="hint"><p>Select a checkout mesh or label to inspect its exported source. Open project changes scope.</p></div>
      </aside>
    );
  }
  return (
    <aside className={props.view === "hover" ? "inspector is-inspecting" : "inspector"}>
      <Corners />
      <PlateBody p={p} hover={props.view === "hover"} />
    </aside>
  );
}

function SourceStatus() {
  return <p className="brain-source-status">STALE · static export<br />{PROFILE_SOURCE.capturedAt}</p>;
}

function PlateBody({ p, hover }: { p: ProjectBody; hover: boolean }) {
  return (
    <>
      <h2>{p.name}</h2>
      <div className="kind">PROJECT BODY</div>
      <SourceStatus />
      <div className="mono-id">ID: {p.id}</div>
      {hover && <div className="kv"><div className="k">CANONICAL ROOT</div><div className="v">{p.canonicalRoot ?? "UNAVAILABLE"}</div></div>}
      <Metrics p={p} />
      <div className="kv">
        <div className="k">RECENCY</div>
        <div className="v"><span>bucket</span><span>{recencyLabel(p.recency)}</span></div>
        <div className="v"><span>age</span><span>{p.age}</span></div>
      </div>
      <Checkouts p={p} />
      <Hint />
    </>
  );
}

function Metrics({ p }: { p: ProjectBody }) {
  return (
    <div className="kv">
      <div className="k">METRICS</div>
      <div className="v"><span>store_count</span><span>{fmt(p.storeCount)}</span></div>
      <div className="v"><span>artifact_count</span><span>{fmt(p.artifactCount)}</span></div>
      <div className="v"><span>indexed mass</span><span>{fmt(p.indexedMass)}</span></div>
    </div>
  );
}

function Checkouts({ p }: { p: ProjectBody }) {
  if (!p.checkouts.length) return null;
  return (
    <div className="kv">
      <div className="k">CHECKOUTS ({p.checkouts.length})</div>
      {p.checkouts.map((c) => (
        <div className="v checkout" key={c.alias}>
          <span className="co-left">
            <b>{c.alias}</b>
            <span className="path">{c.path}</span>
          </span>
          <span className="co-age">{c.lastSeen === "—" ? "UNAVAILABLE" : c.lastSeen}</span>
        </div>
      ))}
    </div>
  );
}

function BodyFacts({ p, extra }: { p: ProjectBody; extra?: boolean }) {
  return (
    <>
      <div className="kv">
        <div className="k">PROJECT BODY</div>
        <div className="v">{p.name}</div>
        <div className="v">{p.id}</div>
      </div>
      <Metrics p={p} />
      <div className="kv">
        <div className="k">RECENCY</div>
        <div className="v"><span>bucket</span><span>{recencyLabel(p.recency)}</span></div>
        <div className="v"><span>age</span><span>{p.age}</span></div>
      </div>
      {extra ? <Checkouts p={p} /> : null}
    </>
  );
}

function Hint({ labScoped }: { labScoped?: boolean } = {}) {
  return (
    <div className="hint">
      <span className="info" aria-hidden="true">i</span>
      <p>
        hover to inspect
        <br />
        {labScoped ? (
          <>
            esc / empty air returns
            <br />
            click does not fire
          </>
        ) : (
          <>
            click project to scope
            <br />
            hub does not scope
          </>
        )}
      </p>
    </div>
  );
}
