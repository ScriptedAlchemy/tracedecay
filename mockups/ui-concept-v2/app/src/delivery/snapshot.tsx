import { useEffect, useRef } from "react";
import workload, { trackingCoverage, type TrackedBranch } from "../data/tracked-workload";
import { useDemo, useWorkspaceState } from "../app/workspace";

const branches = workload.branches;
const repositories = [...new Set(branches.map((branch) => branch.repository))].sort();
const contextFor = (branch: TrackedBranch) => workload.prs.find((pr) => pr.id === branch.prContext?.id);
const purpose = (branch: TrackedBranch) => contextFor(branch)?.title ?? branch.trackedRef.replace(/^refs\/heads\//, "").replaceAll(/[-_]/g, " ");
const linkedBranch = (prId: string) => branches.find((branch) => branch.prContext?.id === prId);
// A source-body relation is visible only between two admitted branch contexts.
const relations = workload.relations.filter((relation) => linkedBranch(relation.from) && linkedBranch(relation.to));

export function DeliverySnapshot() {
  const { navigate, setMode } = useDemo();
  const params = new URLSearchParams(location.search);
  const incoming = branches.find((branch) => branch.id === params.get("branch") || branch.prContext?.id === params.get("pr"));
  const arrival = useRef(incoming);
  const [selectedId, setSelectedId] = useWorkspaceState("delivery.branches.selected", incoming?.id ?? branches[0]?.id ?? "");
  const [project, setProject] = useWorkspaceState("delivery.branches.project", "");
  const [query, setQuery] = useWorkspaceState("delivery.branches.query", "");
  const [relationId, setRelationId] = useWorkspaceState("delivery.branches.relation", "");
  const effectiveProject = repositories.includes(project) ? project : "";
  const visible = branches.filter((branch) => (!effectiveProject || branch.repository === effectiveProject) && `${branch.repository} ${purpose(branch)} ${branch.trackedRef} ${branch.prContext?.number ?? ""}`.toLowerCase().includes(query.toLowerCase()));
  const selected = visible.find((branch) => branch.id === selectedId) ?? visible[0];
  const selectionScope = `${selected?.id ?? "unavailable"}:${selected?.graphSource.source_oid ?? "unavailable"}`;
  const [directory, setDirectory] = useWorkspaceState(`delivery.branches.directory:${selectionScope}`, "");
  const [filePath, setFilePath] = useWorkspaceState(`delivery.branches.file:${selectionScope}`, "");
  const footprint = selected?.footprint?.headOid === selected?.graphSource.source_oid ? selected?.footprint : undefined;
  const selectedFile = footprint?.files.find((file) => file.path === filePath);
  const directories = [...new Set(footprint?.files.map((file) => file.path.split("/").slice(0, -1).join("/") || ".") ?? [])].sort();
  const relation = relations.find((item) => item.id === relationId && visible.some((branch) => branch.prContext?.id === item.from) && visible.some((branch) => branch.prContext?.id === item.to));
  const visibleRelations = relations.filter((item) => visible.some((branch) => branch.prContext?.id === item.from) && visible.some((branch) => branch.prContext?.id === item.to));
  useEffect(() => {
    if (!arrival.current) return;
    if (arrival.current.id !== selectedId) { setSelectedId(arrival.current.id); setRelationId(""); }
    if (!visible.some((branch) => branch.id === arrival.current?.id)) { setProject(""); setQuery(""); }
  }, []);
  const pick = (branch: TrackedBranch) => {
    setSelectedId(branch.id); setRelationId(""); setFilePath(""); setDirectory("");
    const url = new URL(location.href); url.searchParams.set("branch", branch.id); url.searchParams.delete("pr"); history.replaceState(null, "", url);
  };

  if (trackingCoverage.state === "unavailable") return <div className="dl-snapshot dl-tracking-unavailable" role="status" aria-label="Tracked branch scope unavailable">
    <div className="dl-tracking-state"><span className="dl-tracking-symbol" aria-hidden="true">◇</span><span>TRACKING EVIDENCE UNAVAILABLE</span></div>
    <h2>Delivery needs tracked, indexed branches</h2>
    <p>{trackingCoverage.reason}</p>
    <div className="dl-tracking-requirements"><span>TraceDecay project</span><b>→</b><span>Tracked branch</span><b>→</b><span>Indexed head</span></div>
    <p>This is a missing source, not an empty backlog. A matching GitHub PR can supply context once its branch head is indexed.</p>
    <div className="dl-tracking-actions"><button onClick={() => setMode("fixture")}>Explore authored Delivery design</button><button onClick={() => navigate("explorer")}>Open repository explorer</button></div>
    <details><summary>What the recorded export establishes</summary>{trackingCoverage.evidence.map((item) => <p key={item.source}><strong>{item.source}</strong><br/>{item.finding}</p>)}</details>
  </div>;

  return <div className={`dl-snapshot dl-branches${visible.length === 1 ? " is-single" : ""}`}>
    <div className="dl-branch-intro"><div><h2>Indexed branches</h2><p>{workload.trackedBranchCount ?? "Unknown"} indexed {workload.trackedBranchCount === 1 ? "branch" : "branches"} · {workload.prContextCount ?? "Unknown"} linked PR {workload.prContextCount === 1 ? "context" : "contexts"}</p></div><button onClick={() => setMode("fixture")}>Explore authored Delivery design</button></div>
    <div className="dl-branch-workspace">
      <aside className="dl-pane dl-branch-list" aria-label="Tracked branches">
        <label>Project<select aria-label="Filter tracked branches by project" value={effectiveProject} onChange={(event) => {setProject(event.target.value);setRelationId("");setFilePath("");setDirectory("");}}><option value="">All indexed projects</option>{repositories.map((repository) => <option key={repository}>{repository}</option>)}</select></label>
        <label>Find a branch<input className="dl-search" aria-label="Search tracked branches" value={query} onChange={(event) => {setQuery(event.target.value);setRelationId("");setFilePath("");setDirectory("");}} placeholder="Purpose or PR number"/></label>
        <p>{visible.length} matching {visible.length === 1 ? "branch" : "branches"}</p>
        {visible.map((branch) => <button key={branch.id} className={`dl-branch-row${selected?.id === branch.id ? " is-selected" : ""}`} aria-pressed={selected?.id === branch.id} onClick={() => pick(branch)}><span>{branch.repository}</span><strong>{purpose(branch)}</strong><small>{branch.prContext ? `PR #${branch.prContext.number} context` : "No linked PR context"}</small></button>)}
        {!visible.length && <><p>No branch matches these filters.</p><button onClick={() => {setProject("");setQuery("");}}>Clear filters</button></>}
      </aside>
      <section className="dl-pane dl-branch-map" aria-label="Indexed branch relationships">
        <div className="dl-branch-map-heading"><strong>{footprint ? "Changed-file footprint" : "Tracked work"}</strong><span>{footprint ? `${footprint.files.length} files · ${directories.length} directories · +${footprint.files.reduce((sum,file) => sum + (file.additions ?? 0),0)} / −${footprint.files.reduce((sum,file) => sum + (file.deletions ?? 0),0)} text lines` : "Branch index evidence · PRs are linked context"}</span></div>
        {footprint ? <div className="dl-change-footprint">
          <p className="dl-branch-boundary">Directories contain changed files. Green: added lines. Copper: removed lines. Shared logarithmic scale.</p>
          <button className="dl-directory-back" hidden={!directory || !directories.includes(directory)} onClick={() => {setDirectory("");setFilePath("");}}>Back to changed files</button>
          <div className="dl-change-directories">{directories.filter((item) => !directory || !directories.includes(directory) || item === directory).map((directory) => {
            const files = footprint.files.filter((file) => (file.path.split("/").slice(0,-1).join("/") || ".") === directory).sort((a,b) => a.path.localeCompare(b.path));
            return <section key={directory} className="dl-change-directory"><h3><button onClick={() => {setDirectory(directory);setFilePath("");}} aria-label={`Focus directory ${directory}`}>{directory}</button><small>{files.length} {files.length === 1 ? "file" : "files"}</small></h3><div>{files.map((file) => <button key={file.path} className="dl-change-file" aria-label={`Inspect changed file ${file.path}`} aria-pressed={selectedFile?.path === file.path} onClick={() => {setFilePath(file.path);setRelationId("");}} title={file.path}>
              <span>{file.path.split("/").at(-1)}</span><small>{file.status} · +{file.additions ?? "?"} / −{file.deletions ?? "?"}</small>
              <i aria-hidden="true"><b style={{width:`${Math.min(100,Math.log2((file.additions ?? 0)+1)*10)}%`}}/><em style={{width:`${Math.min(100,Math.log2((file.deletions ?? 0)+1)*10)}%`}}/></i>
            </button>)}</div></section>;
          })}</div>
        </div> : <>
        <div className={`dl-branch-cards${visible.length === 2 ? " is-pair" : ""}`}>
          {visible.map((branch, index) => <div key={branch.id} className="dl-branch-position">
            <button className={`dl-branch-card${selected?.id === branch.id ? " is-selected" : ""}`} aria-label={`Inspect indexed branch: ${purpose(branch)}`} aria-pressed={selected?.id === branch.id} onClick={() => pick(branch)}>
              <span className="dl-branch-repository">{branch.repository}</span><span className="dl-branch-kind">◈ Indexed branch</span><strong>{purpose(branch)}</strong>
              <span className="dl-branch-pr">{branch.prContext ? `Linked PR #${branch.prContext.number} · exact head match` : "No PR context captured"}</span>
              <span className="dl-branch-head">Indexed head <code>{branch.graphSource.source_oid.slice(0,12)}</code></span>
            </button>
            {visible.length === 2 && index === 0 && visibleRelations.length > 0 && <div className="dl-branch-connector" aria-hidden="true"><span>{visibleRelations.every((edge) => edge.type === "companion") ? "companion" : "related"}</span></div>}
          </div>)}
        </div>
        </>}
        {!visible.length && <div className="dl-branch-no-match">Clear the filters to return to indexed work.</div>}
        <div className="dl-branch-relations">{visibleRelations.length > 0 && <h3>Why these branches are related</h3>}{visibleRelations.length ? visibleRelations.map((edge) => <button key={edge.id} aria-pressed={relation?.id === edge.id} onClick={() => {setRelationId(edge.id);setFilePath("");}}><strong>{edge.type}</strong><span>PR #{linkedBranch(edge.from)?.prContext?.number} {edge.type === "companion" ? "—" : "→"} PR #{linkedBranch(edge.to)?.prContext?.number}</span><small>Read source statement ↗</small></button>) : <p>Relationship evidence is not supplied for this sample.</p>}</div>
        {visibleRelations.length > 0 && <p className="dl-branch-boundary">A companion link establishes related work, not merge order or a shared delivery outcome. CI and review readiness are separate sources.</p>}
      </section>
      <aside className="dl-pane dl-branch-inspector" aria-label="Selected branch evidence">
        {selectedFile && footprint && selected ? <><button onClick={() => setFilePath("")}>Back to indexed branch</button><h3>SELECTED CHANGED FILE</h3><h2>{selectedFile.path.split("/").at(-1)}</h2><p>{selectedFile.path}</p><dl><dt>Recorded change</dt><dd>{selectedFile.status} · +{selectedFile.additions ?? "unavailable"} / −{selectedFile.deletions ?? "unavailable"} lines</dd>{selectedFile.previousPath && <><dt>Previous path</dt><dd>{selectedFile.previousPath}</dd></>}<dt>Comparison base</dt><dd>{footprint.baseOid}</dd><dt>Indexed head</dt><dd>{footprint.headOid}</dd><dt>Git observation</dt><dd>{footprint.observedAt}</dd></dl><a href={`https://github.com/${selected.repository}/blob/${selectedFile.status === "D" ? footprint.baseOid : footprint.headOid}/${selectedFile.path.split("/").map(encodeURIComponent).join("/")}`} target="_blank" rel="noreferrer">Open exact file revision ↗</a><p>Direct retained base-to-head diff. This does not establish call relationships, review coverage or affected tests.</p><details><summary>Git comparison source</summary><p>Each tile represents one file. Independent bars share a log₂ scale; full width is 1,023 lines. This measures changed text, not complexity or causal impact. Binary line counts are unavailable.</p>{footprint.sourceEvidence.map((source) => <p key={source}>{source}</p>)}</details></> : relation ? <><h3>RELATIONSHIP SOURCE</h3><h2>{relation.type}</h2><p>{workload.relationDirections[relation.type as keyof typeof workload.relationDirections]}</p><blockquote>{relation.evidence.excerpt}</blockquote><a href={relation.evidence.url} target="_blank" rel="noreferrer">Open exact relationship source ↗</a><p>Recorded {relation.evidence.observedAt ?? "time unavailable"}</p><button onClick={() => setRelationId("")}>Return to selected branch</button></> : selected ? <>
          <h3>SELECTED INDEXED BRANCH</h3><h2>{purpose(selected)}</h2><p>{selected.repository}</p>
          <dl><dt>Indexed head</dt><dd><code>{selected.graphSource.source_oid}</code></dd><dt>Observed</dt><dd>{selected.observedAt}</dd></dl>
          {selected.prContext ? <div className="dl-branch-pr-context"><h3>LINKED PR CONTEXT</h3><a href={selected.prContext.url} target="_blank" rel="noreferrer">PR #{selected.prContext.number} · {contextFor(selected)?.title ?? "Open GitHub source"} ↗</a><p>GitHub head matches the indexed branch head. This does not establish PR-autotrack enrollment.</p><p>Provider context observed {selected.prContext.observedAt}</p></div> : <p>No exact-head PR context is attached.</p>}
          <details><summary>Exact tracking and index proof</summary><dl>{Object.entries(selected.graphSource).map(([key,value]) => <div key={key}><dt>{key}</dt><dd>{value}</dd></div>)}<dt>Tracked ref</dt><dd>{selected.trackedRef}</dd><dt>Branch record</dt><dd>{selected.id}</dd></dl>{selected.sourceEvidence.map((source) => <p key={source}>{source}</p>)}</details>
          <details><summary>Review, checks and chronology</summary><p>No complete revision-bound review or CI coverage is attached to this branch sample. Open its exact PR source to inspect current provider evidence.</p></details>
        </> : <p>Select an indexed branch to inspect its source.</p>}
      </aside>
    </div>
    <details className="dl-branch-coverage"><summary>Indexing sample coverage</summary><p>{trackingCoverage.reason}</p>{workload.source.limitations.map((limitation) => <p key={limitation}>{limitation}</p>)}</details>
  </div>;
}
