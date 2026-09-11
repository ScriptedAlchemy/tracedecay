import { isPivotSurface } from './loom/pivots';
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { NavRail } from "./app/shell/NavRail";
import { Corners } from "./app/shell/Corners";
import { SurfaceAperture } from "./app/shell/SurfaceAperture";
import { SurfaceInspector } from "./app/shell/SurfaceInspector";
import { SurfaceRegister } from "./app/shell/SurfaceRegister";
import { SurfaceStatus } from "./app/shell/SurfaceStatus";
import { INTERIORS } from "./app/shell/interiors";
import {
  chromeFor,
  parseStateParam,
  surfaceNeedsState,
  type SurfaceInspectorSpec,
  type SurfaceSlug,
} from "./app/shell/surfaceChrome";
import { BrainPage, VIEWS, type BrainDim } from "./brain/BrainPage";
import { Inspector } from "./brain/Inspector";
import {
  PROJECTS,
  SYNAPSE_EVENT,
  channelToSurface,
  surfaceToChannel,
  type BrainView,
  type Surface,
} from "./data/fixtures";
import type { SurfaceInspect } from "./surfaces/inspect";
import { useDemo } from './app/workspace';
import { atlasData } from './structure';

function parseView(): BrainView {
  const q = new URLSearchParams(window.location.search).get("view");
  if (q === "hover" || q === "repo-zoom" || q === "scoped" || q === "synapse" || q === "firing-tree" || q === "neuron-lab") return q;
  return "overview";
}

function parseScope(): string | null {
  const q = new URLSearchParams(window.location.search).get("scope");
  if (q && PROJECTS.some((p) => p.id === q)) return q;
  return null;
}

function parseGrain(): "session" | "worktree" {
  return new URLSearchParams(window.location.search).get("grain") === "worktree" ? "worktree" : "session";
}

function parseDim(): BrainDim {
  return new URLSearchParams(window.location.search).get("dim") === "3d" ? "3d" : "2d";
}

function parseSurface(): Surface {
  return channelToSurface(new URLSearchParams(window.location.search).get("surface") ?? "brain");
}

function inspectToSpec(i: SurfaceInspect): SurfaceInspectorSpec {
  return {
    title: i.title,
    kind: i.kind,
    id: i.id,
    sections: i.sections.map((s) => ({
      k: s.k,
      rows: s.rows.map((r) => ({ label: r.l, value: r.r ?? "" })),
      text: i.hint && s === i.sections[i.sections.length - 1] ? undefined : undefined,
    })),
  };
}

const ICONS = {
  link: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <path
        d="M6.4 9.6a3.2 3.2 0 0 0 4.53 0l1.6-1.6a3.2 3.2 0 0 0-4.53-4.53l-.8.8"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinecap="round"
      />
      <path
        d="M9.6 6.4a3.2 3.2 0 0 0-4.53 0l-1.6 1.6a3.2 3.2 0 1 0 4.53 4.53l.8-.8"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinecap="round"
      />
    </svg>
  ),
  wifi: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M2.2 7.2a8.4 8.4 0 0 1 11.6 0" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
      <path d="M4.4 9.2a5.4 5.4 0 0 1 7.2 0" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
      <path d="M6.6 11.1a2.4 2.4 0 0 1 2.8 0" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
      <circle cx="8" cy="13.1" r="0.9" fill="currentColor" />
    </svg>
  ),
  graph: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="4.2" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <circle cx="4.2" cy="12" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <circle cx="12" cy="11.4" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <path d="M7.1 5.5 5.1 10.5M8.9 5.6l2.2 4.3M5.8 12h4.4" fill="none" stroke="currentColor" strokeWidth="1.15" />
    </svg>
  ),
  scope: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <path
        fill="currentColor"
        fillRule="evenodd"
        d="M6.68 4.81 L7.15 2.87 L8.85 2.87 L9.32 4.81 L11.02 3.77 L12.23 4.98 L11.19 6.68 L13.13 7.15 L13.13 8.85 L11.19 9.32 L12.23 11.02 L11.02 12.23 L9.32 11.19 L8.85 13.13 L7.15 13.13 L6.68 11.19 L4.98 12.23 L3.77 11.02 L4.81 9.32 L2.87 8.85 L2.87 7.15 L4.81 6.68 L3.77 4.98 L4.98 3.77 L6.68 4.81 Z M9.70 8.00 A1.70 1.70 0 1 0 6.30 8.00 A1.70 1.70 0 1 0 9.70 8.00 Z"
      />
    </svg>
  ),
};

export function App() {
  const { mode, navigate } = useDemo();
  const initialView = parseView();
  const initialScope = parseScope();
  const initialSurface = parseSurface();
  const [surface, setSurface] = useState<Surface>(initialSurface);
  const [surfaceState, setSurfaceState] = useState(() =>
    parseStateParam(initialSurface, new URLSearchParams(window.location.search).get("state")),
  );
  const [liveInspect, setLiveInspect] = useState<SurfaceInspect | null>(null);
  const [explorerQuery, setExplorerQuery] = useState('');
  const [inspectorClosed, setInspectorClosed] = useState(false);
  const inspectSurface = useCallback((value: SurfaceInspect) => { setLiveInspect(value); setInspectorClosed(false); }, []);
  const [view, setView] = useState<BrainView>(initialView);
  const [dim, setDim] = useState<BrainDim>(parseDim);
  const [labScope, setLabScope] = useState<string | null>(() =>
    initialView === "neuron-lab" ? initialScope : null,
  );
  const [labGrain, setLabGrain] = useState<"session" | "worktree">(parseGrain);
  const [focusedId, setFocusedId] = useState(() => initialScope ?? PROJECTS[0].id);
  const [scoped, setScoped] = useState(initialView === "scoped");
  const focused = useMemo(
    () => PROJECTS.find((p) => p.id === focusedId) ?? PROJECTS[0],
    [focusedId],
  );

  const channel = surfaceToChannel(surface);
  useEffect(() => { document.title = `TRACEDECAY · ${channel}`; }, [channel]);
  const isBrain = surface === "brain";
  const isSnapshotBrain = isBrain && mode === 'snapshot';
  const isAtlas = isSnapshotBrain && new URLSearchParams(location.search).get("atlas") === "1";
  const slug = isBrain ? null : (surface as SurfaceSlug);
  const baseChrome = slug ? chromeFor(slug, new URLSearchParams(location.search).get("loom_source"), isPivotSurface(slug) && new URLSearchParams(location.search).has("loom_pivot"), mode) : null;
  const requestedCamera = ['facts','geometry','curation','oplog'].includes(surfaceState) ? surfaceState : new URLSearchParams(location.search).get('knowledge_camera');
  const knowledgeCamera = requestedCamera && ['facts','geometry','curation','oplog'].includes(requestedCamera) ? requestedCamera : 'facts';
  let chrome = baseChrome;
  if (baseChrome && slug === 'code') {
    const codeLens = ['cortex', 'trace', 'core', 'files'].includes(surfaceState) ? surfaceState : 'cortex';
    chrome = {...baseChrome, kicker:`CODE / ${codeLens === 'files' ? 'EXACT FILES' : codeLens.toUpperCase()}`};
  } else if (baseChrome && slug === 'knowledge') {
    chrome = {...baseChrome, status:baseChrome.status.map(cell => cell.lab === 'CAMERA'
      ? {...cell, val:knowledgeCamera[0].toUpperCase()+knowledgeCamera.slice(1)} : cell)};
  } else if (baseChrome && (slug === 'automations' || slug === 'workflows') && surfaceState !== '01') {
    chrome = {...baseChrome, status:baseChrome.status.map(cell => cell.lab === 'SELECTION'
      ? {...cell, val:surfaceState} : cell)};
  }
  const showInspector = Boolean(chrome?.inspector) && !inspectorClosed && !(slug && INTERIORS[slug]);

  useEffect(() => {
    setLiveInspect(null);
    setInspectorClosed(false);
  }, [surface]);
  useEffect(() => {
    if (isBrain || !showInspector) return;
    const close = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || (event.target as Element).closest('select')) return;
      setInspectorClosed(true);
      document.querySelector<HTMLButtonElement>('.sess-card')?.focus();
    };
    window.addEventListener('keydown',close);
    return () => window.removeEventListener('keydown',close);
  },[isBrain,showInspector]);

  const prevScope = useRef(labScope);
  useEffect(() => {
    const u = new URL(window.location.href);
    if (isBrain) {
      u.searchParams.delete("surface");
      u.searchParams.delete("state");
      u.searchParams.set("view", view);
      u.searchParams.set("dim", dim);
      if (view === "neuron-lab" && labScope) u.searchParams.set("scope", labScope);
      else if (view === "scoped" || view === "repo-zoom") u.searchParams.set("scope", focusedId);
      else u.searchParams.delete("scope");
      if (view === "neuron-lab" && labScope && labGrain === "worktree") {
        u.searchParams.set("grain", "worktree");
      } else {
        u.searchParams.delete("grain");
      }
    } else {
      u.searchParams.set("surface", surface);
      if (surfaceNeedsState(surface)) u.searchParams.set("state", surfaceState);
      else u.searchParams.delete("state");
    }
    const next = `${u.pathname}${u.search}`;
    const cur = `${window.location.pathname}${window.location.search}`;
    if (next === cur) {
      prevScope.current = labScope;
      return;
    }
    const scopeChanged = prevScope.current !== labScope;
    prevScope.current = labScope;
    if (scopeChanged) history.pushState(null, "", u);
    else history.replaceState(null, "", u);
  }, [surface, surfaceState, view, dim, labScope, labGrain, focusedId, isBrain]);

  useEffect(() => {
    const onPop = () => {
      const nextSurface = parseSurface();
      setSurface(nextSurface);
      setSurfaceState(parseStateParam(nextSurface, new URLSearchParams(window.location.search).get("state")));
      if (nextSurface !== "brain") return;
      const nextView = parseView();
      setView(nextView);
      setDim(parseDim());
      const nextScope = parseScope();
      setLabScope(nextView === "neuron-lab" ? nextScope : null);
      setLabGrain(parseGrain());
      if (nextScope) setFocusedId(nextScope);
      setScoped(nextView === "scoped");
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const title =
    view === "synapse"
      ? "ACTIVITY BECOMES SYNAPSE — ADMITTED INGRESS ONLY"
      : view === "neuron-lab"
        ? labScope
          ? `NEURON LAB — ${focused.name}`
          : "NEURON LAB — TRACEDECAY CONCEPT"
        : view === "firing-tree"
        ? "FIRING TREE — TRACEDECAY SESSION ARBOR"
        : view === "repo-zoom"
        ? "BRAIN / REPOSITORY NEIGHBORHOOD"
        : view === "scoped"
          ? "BRAIN / WHAT TRACEDECAY KNOWS"
          : "REGISTRY OVERVIEW";

  const scopeProject = view === "scoped" || view === "repo-zoom" ? focused : labScope ? PROJECTS.find(project => project.id === labScope) : !isBrain && initialScope ? PROJECTS.find(project => project.id === initialScope) : null;
  const scopeVal = scopeProject?.name ?? "all";
  function allProjects() {
    setView("overview");
    setScoped(false);
  }

  function onChannel(name: string) {
    const next = channelToSurface(name);
    if (next === surface) return;
    navigate(next);
  }

  const inspectorSpec =
    showInspector && chrome?.inspector
      ? liveInspect
        ? inspectToSpec(liveInspect)
        : chrome.inspector
      : null;

  return (
    <div className="chassis" data-surface={surface} data-brain-view={isBrain ? view : undefined}>
      <div className={(isBrain && !isAtlas && view !== "repo-zoom") || showInspector ? "shell" : "shell no-inspector"}>
        <NavRail channel={channel} onChannel={onChannel} />
        {isBrain ? (
          <>
            <header className="register">
              <Corners />
              <div className="reg-left">
                <h1>
                  Project: <em>{isAtlas ? 'tracedecay' : scopeVal}</em>
                  {view === "scoped" ? (
                    <button type="button" className="reg-all" onClick={allProjects}>
                      All projects
                    </button>
                  ) : null}
                </h1>
                <p className="kicker">{isAtlas ? 'STRUCTURAL ATLAS / GIT SNAPSHOT' : title}</p>
              </div>
              <div className="reg-meta">
                <div className="reg-line">
                  {isAtlas ? `Git ${atlasData.revision.slice(0, 8)}` : isSnapshotBrain ? "recorded profile registry" : view === "repo-zoom" ? "exported checkout registry" : "design fixture · registry field"}
                  <br />
                  {isAtlas ? 'tracked structure · declared dependencies' : isSnapshotBrain ? "registered indexed projects only" : view === "repo-zoom" ? focused.name : "illustrative material study"}
                </div>
                <div className="reg-box">
                  <Corners />
                  {isAtlas ? 'stable landmarks' : isSnapshotBrain ? "recorded indexed heads" : view === "repo-zoom" ? "checkout glyphs" : "indexed mass"}
                  <br />
                  {isAtlas ? 'file containment' : isSnapshotBrain ? "branch recency positions" : view === "repo-zoom" ? "fixed categorical size" : "exported registry value"}
                </div>
                <div className="reg-line bright">
                  {isAtlas ? 'change is a separate layer' : isSnapshotBrain ? "activity feed · empty" : view === "repo-zoom" ? "solid links = registered checkouts" : "brightness = recency"}
                </div>
              </div>
            </header>
            <BrainPage
              view={view}
              setView={setView}
              dim={dim}
              setDim={setDim}
              focused={focused}
              setFocusedId={setFocusedId}
              scoped={scoped}
              setScoped={setScoped}
              labScope={labScope}
              setLabScope={setLabScope}
              labGrain={labGrain}
              setLabGrain={setLabGrain}
            />
            {!isSnapshotBrain && view !== "repo-zoom" ? <Inspector view={view} project={focused} labScope={labScope} /> : null}
            <footer className="status" tabIndex={0} aria-label="Snapshot status">
              <Corners />
              <div className="cell tone-quiet">
                {ICONS.link}
                <div className="stack">
                  <span className="lab">DATA</span>
                  <span className="val">{mode === 'fixture' ? 'design fixture' : isAtlas ? 'Git snapshot' : 'recorded profile'}</span>
                </div>
              </div>
              <div className="cell tone-quiet">
                {ICONS.wifi}
                <div className="stack">
                  <span className="lab">FEED</span>
                  <span className="val">{isSnapshotBrain ? 'empty · 0 admitted events' : view === "synapse" ? "sample frame" : "fixture"}</span>
                </div>
              </div>
              <div className="cell tone-ready">
                {ICONS.graph}
                <div className="stack">
                  <span className="lab">{isAtlas ? 'STRUCTURE' : 'REGISTRY'}</span>
                  <span className="val">{isSnapshotBrain && !isAtlas ? 'recorded only' : 'ready'}</span>
                </div>
              </div>
              <div className="cell tone-scope">
                {ICONS.scope}
                <div className="stack">
                  <span className="lab">SCOPE</span>
                  <span className="val">{isAtlas ? 'tracedecay' : scopeVal}</span>
                </div>
              </div>
              <div className="stamp">{isAtlas ? `GIT ${atlasData.revision.slice(0,8)} / NO LIVE FEED` : isSnapshotBrain ? 'RECORDED SNAPSHOT / READ ONLY' : 'DESIGN FIXTURE / NOT LIVE'}</div>
              {!isAtlas && <select
                className="view-park"
                aria-label="View"
                value={view}
                onChange={(e) => {
                  const next = e.target.value as BrainView;
                  setView(next);
                  if (next === "scoped") setScoped(true);
                  if (next === "overview" || next === "hover" || next === "synapse" || next === "repo-zoom") {
                    setScoped(false);
                  }
                  if (next === "hover" || next === "synapse") setFocusedId(SYNAPSE_EVENT.projectId);
                  if (next !== "neuron-lab") {
                    setLabScope(null);
                    setLabGrain("session");
                  }
                }}
              >
                {VIEWS.map((v) => (
                  <option key={v.id} value={v.id}>
                    {v.label}
                  </option>
                ))}
              </select>}
            </footer>
          </>
        ) : chrome && slug ? (
          <>
            <SurfaceRegister chrome={chrome} query={explorerQuery} onQuery={setExplorerQuery} scope={scopeProject ? {name:scopeProject.name,id:scopeProject.id} : undefined} hideExtra={Boolean(INTERIORS[slug]) || chrome.extra?.type === "loom-follow"} />
            <SurfaceAperture
              surface={slug}
              state={surfaceState}
              onState={setSurfaceState}
              onInspect={inspectSurface}
              query={explorerQuery}
            />
            {inspectorSpec ? <SurfaceInspector inspector={inspectorSpec} onClose={()=>setInspectorClosed(true)} /> : null}
            <SurfaceStatus chrome={chrome} />
          </>
        ) : null}
      </div>
    </div>
  );
}
