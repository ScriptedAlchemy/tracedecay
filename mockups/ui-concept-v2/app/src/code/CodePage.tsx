import { useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { RepositoryAtlas, atlasData, nodeById, type AtlasNode, type AtlasViewState } from "../structure";
import { SemanticField } from "./SemanticField";
import { buildSnapshotDataset, fixtureDataset, type CodeLens, type SemanticDataset, type SemanticNode } from "./semanticData";
import "./code.css";

type CodeView = CodeLens | "files";
type ResolvedNode = SemanticNode & { relationAvailable: boolean };
const LENSES: CodeLens[] = ["cortex", "trace", "core"];

function viewFromParams(params: URLSearchParams): CodeView {
  if (params.has("path") || params.get("lens") === "files") return "files";
  const lens = params.get("lens");
  return lens === "trace" || lens === "core" ? lens : "cortex";
}

function relationCounts(dataset: SemanticDataset, id: string) {
  const edges = dataset.edges;
  return {
    incoming: edges.filter((edge) => edge.target === id).reduce((sum, edge) => sum + edge.weight, 0),
    outgoing: edges.filter((edge) => edge.source === id).reduce((sum, edge) => sum + edge.weight, 0),
  };
}

function resolveNode(dataset: SemanticDataset, id: string): ResolvedNode | undefined {
  const semantic = dataset.nodes.find((node) => node.id === id);
  if (semantic) return { ...semantic, relationAvailable: true };
  for (const file of dataset.files) {
    const symbol = file.symbols.find((candidate) => candidate.id === id);
    if (symbol) return { id: symbol.id, name: symbol.name, kind: symbol.kind, module: file.path.split("/src/")[0] ?? file.path, file: file.path, startLine: symbol.start, endLine: symbol.end, degree: 0, x: 0, y: 0, ring: 0, relationAvailable: false };
  }
  return undefined;
}

function Row({ label, value, absent = false }: { label: string; value: string; absent?: boolean }) {
  return <div className="cd-row"><span>{label}</span><span className={absent ? "r abs" : "r"}>{value}</span></div>;
}

function SemanticInspector({ dataset, lens, pinned, preview }: { dataset: SemanticDataset; lens: CodeLens; pinned: ResolvedNode; preview: ResolvedNode | null }) {
  const { navigate } = useDemo();
  const shown = preview ?? pinned;
  const counts = relationCounts(dataset, shown.id);
  const file = shown.file ? dataset.files.find((candidate) => candidate.path === shown.file) : undefined;
  return <aside className="cd-inspect" aria-label="Semantic selection inspector">
    <Corners />
    <div className="cd-inspect-body">
      <div className="cd-block first">
        <div className="k">{preview ? "HOVER PREVIEW" : "PINNED SELECTION"}</div>
        <div className="cd-sel-box"><i /><b>{shown.name}</b><em>{dataset.sourceLabel}</em></div>
        <Row label="identity" value={shown.id} />
        <Row label="kind" value={shown.kind} />
        <Row label="module" value={shown.module} />
        <Row label="lens" value={lens.toUpperCase()} />
      </div>
      <div className="cd-block">
        <div className="k">RELATIONSHIP EVIDENCE</div>
        <Row label={dataset.source === "authored-fixture" ? "caller sites" : "incoming manifests"} value={shown.relationAvailable ? String(counts.incoming) : "not in Trace sample"} absent={!shown.relationAvailable} />
        <Row label={dataset.source === "authored-fixture" ? "callee sites" : "outgoing manifests"} value={shown.relationAvailable ? String(counts.outgoing) : "not in Trace sample"} absent={!shown.relationAvailable} />
        <Row label="degree" value={shown.relationAvailable ? String(shown.degree) : "not in Trace sample"} absent={!shown.relationAvailable} />
        {shown.unresolved ? <p className="cd-copy cd-unknown">{shown.unresolved}</p> : null}
        <p className="cd-copy">{dataset.source === "authored-fixture" ? "Authored interaction data from the reviewed 26-symbol design fixture. Counts are examples, not an index read." : "Measured repository containment and declared Cargo manifest relations. No symbol calls or runtime reachability are implied."}</p>
      </div>
      <div className="cd-block">
        <div className="k">SOURCE POSITION</div>
        <Row label="path" value={shown.file ?? "not attached"} absent={!shown.file} />
        <Row label="range" value={shown.startLine === null ? "not served" : `${shown.startLine}–${shown.endLine}`} absent={shown.startLine === null} />
        <Row label="provenance" value={dataset.source === "authored-fixture" ? "authored example" : atlasData.revision.slice(0, 12)} />
        {lens === "core" && file && shown.startLine !== null && shown.endLine !== null ? <div className="cd-range-sample" aria-label="Authored source range"><div><b>{shown.startLine}</b><span style={{ top: `${shown.startLine / file.lines * 100}%`, height: `${Math.max(2, (shown.endLine - shown.startLine) / file.lines * 100)}%` }} /><b>{shown.endLine}</b></div><p>{shown.name} · lines {shown.startLine}–{shown.endLine} of {file.lines}</p><small>Range metadata only · source text is not included in this authored fixture</small></div> : null}
      </div>
      <div className="cd-block">
        <div className="k">INDEPENDENT AUTHORITIES</div>
        <Row label="diagnostics" value="not attached" absent />
        <Row label="test mapping" value="not attached" absent />
        <Row label="live activity" value="not attached" absent />
      </div>
      {dataset.source === "measured-snapshot" ? <div className="cd-pivots"><button type="button" onClick={() => navigate("brain", { node: shown.id, path: shown.id })}>BRAIN ↗</button><button type="button" onClick={() => navigate("explorer", { node: shown.id, path: shown.id })}>EXPLORER ↗</button></div> : null}
    </div>
    <div className="cd-hint">Hover previews · click pins · Enter or double-click drills · Escape returns</div>
  </aside>;
}

function CoreInspector({ dataset, pinned, preview, coreView, onCoreView, onSelect }: { dataset: SemanticDataset; pinned: ResolvedNode; preview: ResolvedNode | null; coreView: "overview" | "range"; onCoreView: (view: "overview" | "range") => void; onSelect: (id: string) => void }) {
  const shown = preview ?? pinned;
  const sourceById = new Map(dataset.files.flatMap((file) => file.symbols.map((symbol) => [symbol.id, { file, symbol }] as const)));
  const source = sourceById.get(shown.id);
  const incoming = shown.relationAvailable ? dataset.edges.filter((edge) => edge.target === shown.id) : [];
  const outgoing = shown.relationAvailable ? dataset.edges.filter((edge) => edge.source === shown.id) : [];
  const relationSummary = (edges: typeof dataset.edges, direction: "incoming" | "outgoing") => {
    if (!shown.relationAvailable) return "—";
    const endpoints = new Set(edges.map((edge) => direction === "incoming" ? edge.source : edge.target)).size;
    const sites = edges.reduce((sum, edge) => sum + edge.weight, 0);
    const noun = direction === "incoming" ? "caller" : "callee";
    return `${endpoints} ${noun}${endpoints === 1 ? "" : "s"} · ${sites} ${sites === 1 ? "site" : "sites"}`;
  };
  const relation = (id: string) => resolveNode(dataset, id);
  const relationList = (edges: typeof dataset.edges, direction: "incoming" | "outgoing") => shown.relationAvailable ? edges.length ? <ul>{edges.map((edge, index) => {
    const other = relation(direction === "incoming" ? edge.source : edge.target);
    return <li key={`${edge.source}:${edge.target}:${index}`}><button type="button" onClick={() => onSelect(other?.id ?? "")} disabled={!other}><b>{other?.name ?? (direction === "incoming" ? edge.source : edge.target)}</b><span>{edge.weight} {edge.weight === 1 ? "site" : "sites"}</span><small>{other?.file && other.startLine !== null ? `${other.file.split("/").at(-1)}:${other.startLine}` : "source range unavailable"}</small></button></li>;
  })}</ul> : <p className="cd-core-empty">0 in the authored Trace sample</p> : <p className="cd-core-empty absent">Not in the 26-symbol Trace sample</p>;
  return <aside className="cd-inspect cd-core-inspect" aria-label="Core source inspector">
    <Corners />
    <div className="cd-inspect-body">
      <div className="cd-core-legend" aria-label="Core band legend"><span><i className="code" />CODE</span><span><i className="test" />TEST</span><span><i className="gap" />SOURCE COVERAGE UNAVAILABLE</span></div>
      <section className="cd-core-identity">
        <div className="k">{preview ? "HOVER PREVIEW" : "SELECTED SOURCE BAND"}</div>
        <h2>{shown.name}</h2>
        <p>{shown.file ?? "Source position not attached"}</p>
        <div className="cd-core-range"><strong>{shown.startLine === null ? "RANGE UNAVAILABLE" : `${shown.startLine}–${shown.endLine}`}</strong><span>{shown.startLine === null ? "No source span in this example" : `${(shown.endLine ?? shown.startLine) - shown.startLine + 1} lines · ${shown.kind}`}</span></div>
        <label>Exact symbol<select aria-label="Select Core symbol" value={source ? shown.id : ""} onChange={(event) => onSelect(event.target.value)}><option value="" disabled>Select a source-positioned band</option>{dataset.files.map((file) => <optgroup key={file.path} label={file.path}>{file.symbols.map((symbol) => <option key={symbol.id} value={symbol.id}>{symbol.name} · {symbol.start}–{symbol.end}</option>)}</optgroup>)}</select></label>
      </section>
      <section className="cd-core-relations">
        <div><header><span>INCOMING</span><b>{relationSummary(incoming, "incoming")}</b></header>{relationList(incoming, "incoming")}</div>
        <div><header><span>OUTGOING</span><b>{relationSummary(outgoing, "outgoing")}</b></header>{relationList(outgoing, "outgoing")}</div>
      </section>
      <section className="cd-core-source-state">
        <div className="k">SOURCE TEXT</div><h3>Source text not included</h3>
        <p>{source ? `The authored example supplies the exact ${source.symbol.start}–${source.symbol.end} range in ${source.file.lines}-line ${source.file.path.split("/").at(-1)}.` : "This Trace identity has no source range in the authored Core sample."} No repository bytes are attached.</p>
      </section>
      <section className="cd-core-view" aria-label="Core semantic zoom"><span>VIEW</span><button type="button" aria-pressed={coreView === "overview"} onClick={() => onCoreView("overview")}>OVERVIEW</button><button type="button" aria-pressed={coreView === "range"} disabled={!source} onClick={() => onCoreView("range")}>SOURCE RANGE</button></section>
    </div>
    <div className="cd-hint">Canvas keeps file order and one line scale · hover previews · click pins</div>
  </aside>;
}

function SourceRibbon({ dataset, lens }: { dataset: SemanticDataset; lens: CodeLens }) {
  const statement = dataset.source === "authored-fixture"
    ? lens === "cortex" ? "area = authored symbol mass · contours = internal edges/symbol · heat shown only where supplied" : lens === "trace" ? "tributaries = callers · branches = callees · membranes = contains" : "one line scale · arcs = Trace edges with represented Core spans"
    : lens === "cortex" ? "area = tracked files · strata = Cargo depth · rivers = selected incident manifests · heat = Git touches" : "symbol and call authority is absent from this snapshot";
  return <div className="cd-semantic-ribbon"><strong>{dataset.sourceLabel}</strong><span>{statement}</span><em>{dataset.revision}</em></div>;
}

function MissingSemantic({ lens, onCortex, onFiles }: { lens: CodeLens; onCortex: () => void; onFiles: () => void }) {
  return <div className="cd-semantic-missing" role="status">
    <span>{lens.toUpperCase()} AUTHORITY ABSENT</span>
    <h2>{lens === "trace" ? "No indexed symbols or call edges are attached" : "No source-positioned symbol subgraph is attached"}</h2>
    <p>The measured snapshot can show repository containment, Git health, and declared Cargo dependencies. It cannot turn those crate relations into symbol callers, membranes, source bands, diagnostics, or tests.</p>
    <div><button type="button" onClick={onCortex}>RETURN TO CORTEX</button><button type="button" onClick={onFiles}>OPEN EXACT FILES</button></div>
  </div>;
}

function AuthorityBoard({ dataset }: { dataset: SemanticDataset }) {
  const snapshot = dataset.source === "measured-snapshot";
  return <section className="cd-authority-board" aria-label="Code authority states">
    <div><span>SNAPSHOT</span><b>{snapshot ? "PINNED · FRESHNESS UNKNOWN" : "AUTHORED FIXTURE"}</b><small>{dataset.revision}</small></div>
    <div><span>IMPACT</span><b className="is-unavailable">UNAVAILABLE</b><small>No snapshot-bound dependent projection</small></div>
    <div><span>TEST MAP</span><b className="is-unavailable">UNAVAILABLE</b><small>No mapped-test authority attached</small></div>
    <div><span>DIAGNOSTICS</span><b className="is-unavailable">UNAVAILABLE</b><small>No index diagnostic authority attached</small></div>
  </section>;
}

function ExactFallback({ dataset, selectedId, onSelect, onClose }: { dataset: SemanticDataset; selectedId: string; onSelect: (id: string) => void; onClose: () => void }) {
  const rows = [...dataset.nodes.map((node) => resolveNode(dataset, node.id)), ...dataset.files.flatMap((file) => file.symbols.map((node) => resolveNode(dataset, node.id)))].filter((node): node is ResolvedNode => !!node).filter((node, index, all) => all.findIndex((candidate) => candidate.id === node.id) === index);
  return <div className="cd-exact-fallback" role="dialog" aria-label="Exact Code source table">
    <header><b>EXACT SOURCE TABLE</b><button type="button" onClick={onClose}>CLOSE</button></header>
    <p>{dataset.sourceLabel} · selection, path, range, and snapshot provenance. Calls, impact, diagnostics, and mapped tests retain their independent unavailable states.</p>
    <div role="table" aria-label="Exact Code symbols">
      {rows.map((node) => <button key={node.id} type="button" role="row" className={node.id === selectedId ? "is-on" : ""} onClick={() => onSelect(node.id)}>
        <span role="cell">{node.name}</span><span role="cell">{node.file ?? "source unavailable"}</span><span role="cell">{node.startLine === null ? "range unavailable" : `${node.startLine}–${node.endLine}`}</span>
      </button>)}
    </div>
  </div>;
}

export function CodePage(props: { onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { mode } = useDemo();
  const params = new URLSearchParams(location.search);
  const explicitPath = params.get("path") || params.get("node");
  const dataset = useMemo(() => mode === "fixture" ? fixtureDataset : buildSnapshotDataset(), [mode]);
  const [view, setView] = useState<CodeView>(() => viewFromParams(params));
  const [query, setQuery] = useState(() => params.get("q") ?? "");
  const [submitted, setSubmitted] = useState(() => params.get("q")?.trim() ?? "");
  const [semanticSelection, setSemanticSelection] = useWorkspaceState<string>("code.semantic-selection", params.get("symbol") || dataset.defaultSelection);
  const [atlasSelection, setAtlasSelection] = useWorkspaceState<string>("atlas.selection", explicitPath && nodeById.has(explicitPath) ? explicitPath : "crates/tracedecay");
  const [, setAtlasView] = useWorkspaceState<AtlasViewState>("repository-atlas", { selected: explicitPath && nodeById.has(explicitPath) ? explicitPath : "crates/tracedecay", camera: null, layer: "structure", comparison: "after" });
  const [previewId, setPreviewId] = useState<string | null>(null);
  const [exactFallback, setExactFallback] = useState(false);
  const [coreView, setCoreView] = useWorkspaceState<"overview" | "range">("code.core-view", params.get("core_view") === "range" ? "range" : "overview");
  const searchRef = useRef<HTMLInputElement>(null);
  const pinned = resolveNode(dataset, semanticSelection) ?? resolveNode(dataset, dataset.defaultSelection) ?? (dataset.nodes[0] ? { ...dataset.nodes[0], relationAvailable: true } : undefined);
  const preview = previewId ? resolveNode(dataset, previewId) ?? null : null;
  const lens: CodeLens = view === "files" ? "cortex" : view;
  const results = useMemo(() => {
    if (!submitted) return [];
    const needle = submitted.toLowerCase();
    const complete = [...dataset.nodes.map((node) => ({ ...node, relationAvailable: true })), ...dataset.files.flatMap((file) => file.symbols.filter((symbol) => !dataset.nodes.some((node) => node.id === symbol.id)).map((symbol) => resolveNode(dataset, symbol.id)!))];
    return complete.filter((node) => `${node.name} ${node.kind} ${node.module} ${node.file ?? ""}`.toLowerCase().includes(needle)).slice(0, 10);
  }, [dataset, submitted]);

  useEffect(() => { props.onState?.(view); }, [props.onState, view]);
  useEffect(() => {
    function restore() {
      const next = new URLSearchParams(location.search);
      const nextView = viewFromParams(next); setView(nextView); props.onState?.(nextView);
      const nextPath = next.get("path") || next.get("node");
      if (nextPath && nodeById.has(nextPath)) { setAtlasSelection(nextPath); setAtlasView((state) => ({ ...state, selected: nextPath })); }
      const symbol = next.get("symbol"); if (symbol && resolveNode(dataset, symbol)) setSemanticSelection(symbol);
      const q = next.get("q") ?? ""; setQuery(q); setSubmitted(q.trim());
      setCoreView(next.get("core_view") === "range" ? "range" : "overview");
    }
    function shortcut(event: globalThis.KeyboardEvent) { if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") { searchRef.current?.focus(); event.preventDefault(); } }
    restore(); window.addEventListener("popstate", restore); window.addEventListener("keydown", shortcut);
    return () => { window.removeEventListener("popstate", restore); window.removeEventListener("keydown", shortcut); };
  }, [dataset, props.onState, setAtlasSelection, setAtlasView, setCoreView, setSemanticSelection]);

  useEffect(() => { if (view === "files") setAtlasView((state) => state.selected === atlasSelection ? state : { ...state, selected: atlasSelection }); }, [atlasSelection, setAtlasView, view]);

  function writeUrl(nextView: CodeView, symbol = semanticSelection, path = atlasSelection) {
    const url = new URL(location.href); url.searchParams.set("lens", nextView);
    if (nextView === "files") { url.searchParams.set("path", path); url.searchParams.set("node", path); url.searchParams.delete("symbol"); }
    else { url.searchParams.delete("path"); url.searchParams.set("symbol", symbol); url.searchParams.delete("node"); }
    if (nextView === "core" && coreView === "range") url.searchParams.set("core_view", "range"); else url.searchParams.delete("core_view");
    if (submitted) url.searchParams.set("q", submitted); else url.searchParams.delete("q");
    history.pushState(null, "", url);
  }
  function chooseView(next: CodeView) { setView(next); setPreviewId(null); writeUrl(next); props.onState?.(next); }
  function selectSemantic(id: string) { if (!resolveNode(dataset, id)) return; setSemanticSelection(id); const url = new URL(location.href); url.searchParams.set("symbol", id); url.searchParams.delete("path"); url.searchParams.delete("node"); history.replaceState(null, "", url); }
  function selectAtlas(node: AtlasNode) { setAtlasSelection(node.id); const url = new URL(location.href); url.searchParams.set("lens", "files"); url.searchParams.set("node", node.id); url.searchParams.set("path", node.id); url.searchParams.delete("symbol"); history.replaceState(null, "", url); }
  function drill() { if (!dataset.semanticAvailable) return; chooseView(lens === "cortex" ? "trace" : lens === "trace" ? "core" : "core"); }
  function back() { chooseView(lens === "core" ? "trace" : "cortex"); }
  function submitSearch(event: FormEvent) { event.preventDefault(); const next = query.trim(); setSubmitted(next); const candidates = [...dataset.nodes.map((node) => ({ ...node, relationAvailable: true })), ...dataset.files.flatMap((file) => file.symbols.map((symbol) => resolveNode(dataset, symbol.id)!).filter(Boolean))]; const first = candidates.find((node) => `${node.name} ${node.module} ${node.file ?? ""}`.toLowerCase().includes(next.toLowerCase())); if (next && first) selectSemantic(first.id); const url = new URL(location.href); if (next) url.searchParams.set("q", next); else url.searchParams.delete("q"); history.replaceState(null, "", url); }
  function clearSearch() { setQuery(""); setSubmitted(""); const url = new URL(location.href); url.searchParams.delete("q"); history.replaceState(null, "", url); }
  function chooseCoreView(next: "overview" | "range") { if (next === "range" && !dataset.files.some((file) => file.symbols.some((symbol) => symbol.id === semanticSelection))) return; setCoreView(next); const url = new URL(location.href); if (next === "range") url.searchParams.set("core_view", "range"); else url.searchParams.delete("core_view"); history.pushState(null, "", url); }
  function traverse(event: KeyboardEvent<HTMLDivElement>) { if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return; const tabs: CodeView[] = [...LENSES, "files"]; const delta = event.key === "ArrowRight" ? 1 : -1; const next = tabs[(tabs.indexOf(view) + delta + tabs.length) % tabs.length]; chooseView(next); event.currentTarget.querySelector<HTMLButtonElement>(`[data-tab="${next}"]`)?.focus(); event.preventDefault(); }

  if (!pinned) return null;
  return <div className={`cd-root ${view === "files" ? "is-files" : "is-semantic"} ${view === "core" ? "is-core" : ""}`}>
    <div className="cd-main">
      <div className="cd-controlbar">
        <div className="cd-tabs" role="tablist" aria-label="Code lenses" onKeyDown={traverse}>
          {LENSES.map((item) => <button key={item} type="button" role="tab" data-tab={item} aria-selected={view === item} tabIndex={view === item ? 0 : -1} className={view === item ? "is-on" : ""} onClick={() => chooseView(item)}>{item.toUpperCase()}</button>)}
          <button type="button" role="tab" data-tab="files" aria-selected={view === "files"} tabIndex={view === "files" ? 0 : -1} className={view === "files" ? "is-on is-files" : "is-files"} onClick={() => chooseView("files")}>EXACT FILES</button>
        </div>
        {view !== "files" ? <form className="cd-search" role="search" onSubmit={submitSearch}><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" fill="none" stroke="currentColor" strokeWidth="1.2" /><path d="m10.4 10.4 3.2 3.2" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" /></svg><input ref={searchRef} value={query} onChange={(event) => setQuery(event.target.value)} placeholder={dataset.source === "authored-fixture" ? "Find an authored symbol or file" : "Find a measured crate"} aria-label="Search Code field" />{query ? <button type="button" className="cd-search-clear" onClick={clearSearch} aria-label="Clear Code search">×</button> : <kbd>⌘K</kbd>}<button type="submit" className="cd-search-go">FIND</button></form> : <div className="cd-files-source">MEASURED GIT SNAPSHOT · {atlasData.revision.slice(0, 8)}</div>}
      </div>
      {submitted && view !== "files" ? <div className="cd-matchbar" aria-label="Code search results"><span>{results.length ? `${results.length} matches` : "NO MATCHES"}</span>{results.map((node) => <button key={node.id} type="button" onClick={() => selectSemantic(node.id)}>{node.name}<small>{node.module}</small></button>)}</div> : null}
      {view !== "files" ? <AuthorityBoard dataset={dataset} /> : null}
      <section className="cd-well" aria-label={view === "files" ? "Exact file atlas" : `${lens} semantic lens`}>
        {view === "files" ? <RepositoryAtlas context="code" initialSelection={atlasSelection} onSelect={selectAtlas} /> : <>
          <SourceRibbon dataset={dataset} lens={lens} />
          <SemanticField dataset={dataset} lens={lens} selectedId={pinned.id} coreView={coreView} onCoreView={chooseCoreView} onSelect={selectSemantic} onPreview={setPreviewId} onDrill={drill} onBack={back} />
          {!dataset.semanticAvailable && lens !== "cortex" ? <MissingSemantic lens={lens} onCortex={() => chooseView("cortex")} onFiles={() => chooseView("files")} /> : null}
          <div className="cd-lens-readout"><span>{lens === "cortex" ? `${dataset.regions.length} REGIONS` : !dataset.semanticAvailable ? `${lens.toUpperCase()} COUNTS UNAVAILABLE` : lens === "trace" ? `${dataset.nodes.length} SYMBOLS · ${dataset.edges.length} CHANNELS` : `${dataset.files.length} FILE CORES · ${dataset.files.reduce((sum, file) => sum + file.symbols.length, 0)} SOURCE BANDS`}</span><b>{pinned.name}</b><em>{dataset.source === "authored-fixture" ? "EXAMPLE DATA" : "MEASURED SNAPSHOT"}</em></div>
          <button type="button" className="cd-exact-open" onClick={() => setExactFallback(true)}>EXACT SOURCE TABLE</button>
          {exactFallback ? <ExactFallback dataset={dataset} selectedId={pinned.id} onSelect={(id) => { selectSemantic(id); setExactFallback(false); }} onClose={() => setExactFallback(false)} /> : null}
        </>}
      </section>
    </div>
    {view === "core" && dataset.semanticAvailable ? <CoreInspector dataset={dataset} pinned={pinned} preview={preview} coreView={coreView} onCoreView={chooseCoreView} onSelect={selectSemantic} /> : view !== "files" ? <SemanticInspector dataset={dataset} lens={lens} pinned={pinned} preview={preview} /> : null}
  </div>;
}
