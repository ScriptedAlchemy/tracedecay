import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "./api";
import { canvasEmptyMessage, DEFAULT_VIEW_LIMITS } from "./defaultView";
import GraphCanvas from "./GraphCanvas";
import OverviewPanel from "./OverviewPanel";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Input, cn } from "../../lib/sdk";
import { fmt, short } from "../../lib/format";
import { EmptyState, ErrorPanel } from "../../lib/primitives";
import { makeSequence } from "../../lib/sequence";
import { useGraphInspection } from "./useGraphInspection";
import { useGraphSearch } from "./useGraphSearch";
import {
  appendFocusHistory,
  applyGraphFilters,
  deriveChipOptions,
  mergeEdgesInto,
  mergeNodesInto,
  toggleStringSet,
} from "./explorerState";
import {
  colorForKind,
  KIND_FAMILY_COLORS,
  KIND_FAMILY_LABELS,
} from "./types";
import type {
  GraphEdge,
  GraphNeighborsResponse,
  GraphNode,
  GraphOverview,
  GraphPathResponse,
} from "./types";

/** Soft cap on accumulated canvas nodes; expansion pauses above this. */
const CANVAS_NODE_CAP = 600;

const EDGE_LEGEND: Array<{ kind: string; label: string; className: string }> = [
  { kind: "calls", label: "calls", className: "tsg-legend-calls" },
  { kind: "uses", label: "uses", className: "tsg-legend-uses" },
  { kind: "implements", label: "implements / extends", className: "tsg-legend-impl" },
  { kind: "contains", label: "contains", className: "tsg-legend-contains" },
];

function Legend() {
  return (
    <div className="tsg-legend">
      {Object.entries(KIND_FAMILY_LABELS).map(([family, label]) => (
        <span key={family} className="tsg-legend-item">
          <i style={{ background: KIND_FAMILY_COLORS[family] }} />
          {label}
        </span>
      ))}
      <span className="tsg-legend-sep" />
      {EDGE_LEGEND.map((row) => (
        <span key={row.kind} className="tsg-legend-item">
          <em className={row.className} />
          {row.label}
        </span>
      ))}
    </div>
  );
}

function DetailPanel({
  node,
  neighbors,
  onJump,
  onShowCallers,
  onShowCallees,
}: {
  node: GraphNode | null;
  neighbors: GraphNeighborsResponse | null;
  onJump: (node: GraphNode) => void;
  onShowCallers: () => void;
  onShowCallees: () => void;
}) {
  if (!node) {
    return (
      <Card className="tsg-panel">
        <CardHeader><CardTitle>Inspector</CardTitle></CardHeader>
        <CardContent>
          <EmptyState>
            Click a node to inspect it. Double-click (or use the +N badge counts as a guide) to
            expand its neighbors into the canvas.
          </EmptyState>
        </CardContent>
      </Card>
    );
  }
  const renderList = (title: string, rows: GraphNode[]) => (
    <section className="tsg-neighbor-section">
      <h4>{title} <span>{rows.length}</span></h4>
      {rows.length === 0 ? (
        <p>None in the indexed graph.</p>
      ) : (
        rows.slice(0, 10).map((row) => (
          <button key={row.id} onClick={() => onJump(row)}>
            <span>{row.name}</span>
            <small>{short(row.file_path, 42)}</small>
          </button>
        ))
      )}
    </section>
  );
  return (
    <Card className="tsg-panel">
      <CardHeader>
        <CardTitle>{node.name}</CardTitle>
        <div className="tsg-panel-badges">
          <Badge>{node.kind}</Badge>
          {node.visibility && <Badge>{node.visibility}</Badge>}
          <Badge>{fmt(node.degree)} edges</Badge>
        </div>
      </CardHeader>
      <CardContent>
        <div className="tsg-detail">
          <code>{node.qualified_name}</code>
          <span>{node.file_path}:{node.span?.start_line || 0}-{node.span?.end_line || 0}</span>
          {node.signature && <pre>{node.signature}</pre>}
          {node.doc && <p>{short(node.doc, 360)}</p>}
        </div>
        <div className="tsg-panel-actions">
          <Button size="sm" onClick={onShowCallers}>Show callers</Button>
          <Button size="sm" onClick={onShowCallees}>Show callees</Button>
        </div>
        {renderList("Callers", neighbors?.callers || [])}
        {renderList("Callees", neighbors?.callees || [])}
        <section className="tsg-edge-kinds">
          {(neighbors?.edges_by_kind || []).map((row) => (
            <Badge key={row.kind}>{row.kind}: {fmt(row.count)}</Badge>
          ))}
        </section>
      </CardContent>
    </Card>
  );
}

export default function CodeGraphExplorer() {
  // Canvas is the landing view: it self-populates with the default slice,
  // so tab entry shows the graph immediately (Overview stays one click away).
  const [view, setView] = useState<"overview" | "canvas">("canvas");
  const [overview, setOverview] = useState<GraphOverview | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  // Accumulated canvas graph (progressive exploration).
  const [graphNodes, setGraphNodes] = useState<Map<string, GraphNode>>(new Map());
  const [graphEdges, setGraphEdges] = useState<Map<string, GraphEdge>>(new Map());
  const [focusId, setFocusId] = useState<string | null>(null);
  const [fitSignal, setFitSignal] = useState(0);
  const [history, setHistory] = useState<Array<{ id: string; name: string }>>([]);

  // Filters.
  const [kindFilters, setKindFilters] = useState<Set<string>>(new Set());
  const [langFilters, setLangFilters] = useState<Set<string>>(new Set());
  const [dirScope, setDirScope] = useState("");

  // Path-finding mode.
  const [pathMode, setPathMode] = useState(false);
  const [pathFrom, setPathFrom] = useState<GraphNode | null>(null);
  const [pathTo, setPathTo] = useState<GraphNode | null>(null);
  const [pathResult, setPathResult] = useState<GraphPathResponse | null>(null);

  // True while the canvas shows the untouched seedless default view; the
  // first search-to-focus replaces it instead of merging into it.
  const defaultViewRef = useRef(false);
  // Invalidates an in-flight default load once the user takes over the canvas.
  const defaultSeq = useRef(makeSequence()).current;

  const {
    selected,
    neighbors,
    setSelected,
    setNeighbors,
    inspect,
  } = useGraphInspection({
    loadNode: api.node,
    loadNeighbors: (id: string) => api.neighbors(id, { limit: 60 }),
    onError: setError,
  });

  useEffect(() => {
    api.overview().then(setOverview).catch((err) => setError(String(err)));
  }, []);

  const mergeGraph = useCallback((nodes: GraphNode[], edges: GraphEdge[]) => {
    setGraphNodes((prev) => mergeNodesInto(prev, nodes));
    setGraphEdges((prev) => mergeEdgesInto(prev, edges));
  }, []);

  /** Loads the seedless default slice (top-connected hubs + their edges). */
  const loadDefaultView = useCallback(async () => {
    const ticket = defaultSeq.next();
    setLoading(true);
    setError("");
    try {
      const payload = await api.subgraph(DEFAULT_VIEW_LIMITS);
      if (!defaultSeq.isCurrent(ticket)) return;
      mergeGraph(payload.nodes, payload.edges);
      defaultViewRef.current = true;
      setFitSignal((prev) => prev + 1);
    } catch (err) {
      if (defaultSeq.isCurrent(ticket)) setError(String(err));
    } finally {
      if (defaultSeq.isCurrent(ticket)) setLoading(false);
    }
  }, [defaultSeq, mergeGraph]);

  useEffect(() => {
    void loadDefaultView();
  }, [loadDefaultView]);

  // Clear returns to the default view rather than an empty canvas. Filters
  // reset too: stale chips would otherwise hide the reloaded default slice
  // and leave the canvas looking broken.
  const clearCanvas = useCallback(() => {
    setGraphNodes(new Map());
    setGraphEdges(new Map());
    setHistory([]);
    setSelected(null);
    setNeighbors(null);
    setPathResult(null);
    setPathFrom(null);
    setPathTo(null);
    setFocusId(null);
    setKindFilters(new Set());
    setLangFilters(new Set());
    setDirScope("");
    void loadDefaultView();
  }, [loadDefaultView]);

  const expandNode = useCallback(
    async (id: string, { focus = false } = {}) => {
      if (graphNodes.size >= CANVAS_NODE_CAP) {
        setError(`Canvas holds ${graphNodes.size} nodes — clear it to keep expanding.`);
        return;
      }
      defaultViewRef.current = false;
      setLoading(true);
      setError("");
      try {
        const payload = await api.subgraph({ node_id: id, limit_nodes: 60, limit_edges: 120 });
        mergeGraph(payload.nodes, payload.edges);
        if (focus) setFocusId(id);
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
    },
    [graphNodes.size, mergeGraph],
  );

  const runPath = useCallback(
    async (from: GraphNode, to: GraphNode) => {
      setLoading(true);
      setError("");
      try {
        const payload = await api.path({ from: from.id, to: to.id, max_depth: 8 });
        setPathResult(payload);
        if (payload.found) {
          defaultViewRef.current = false;
          mergeGraph(payload.nodes, payload.edges);
        } else {
          setError(`No path within ${payload.max_depth} hops between ${from.name} and ${to.name}.`);
        }
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
    },
    [mergeGraph],
  );

  const selectInCanvas = useCallback(
    (id: string) => {
      const node = graphNodes.get(id);
      void inspect(id);
      if (pathMode && node) {
        if (!pathFrom || pathTo) {
          // First pick, or a fresh pick after a completed run: start a new path.
          setPathFrom(node);
          setPathTo(null);
          setPathResult(null);
        } else if (pathFrom.id !== node.id) {
          setPathTo(node);
          void runPath(pathFrom, node);
        }
      }
    },
    [graphNodes, inspect, pathMode, pathFrom, pathTo, runPath],
  );

  /** Search-to-focus: seed the canvas at the symbol and fly the camera to it. */
  const focusSymbol = useCallback(
    (node: Pick<GraphNode, "id" | "name">) => {
      setView("canvas");
      setSearchOpen(false);
      // Search refines rather than appends: the pristine default view is
      // replaced by the focused neighborhood. Once the user has built up a
      // custom exploration, focusing merges into it as before.
      if (defaultViewRef.current) {
        defaultViewRef.current = false;
        defaultSeq.invalidate();
        setGraphNodes(new Map());
        setGraphEdges(new Map());
        setPathResult(null);
        setPathFrom(null);
        setPathTo(null);
      }
      setHistory((prev) => appendFocusHistory(prev, node));
      void expandNode(node.id, { focus: true });
      void inspect(node.id);
    },
    [defaultSeq, expandNode, inspect],
  );

  const {
    query,
    results,
    searchOpen,
    searchBoxRef,
    setSearchOpen,
    onQueryChange,
    onSearchKeyDown,
  } = useGraphSearch({
    search: api.search,
    focusSymbol,
    onError: setError,
  });

  // Visible (filtered) slice of the accumulated canvas graph.
  const visible = useMemo(
    () => applyGraphFilters(graphNodes.values(), graphEdges.values(), { kindFilters, langFilters, dirScope }),
    [graphNodes, graphEdges, kindFilters, langFilters, dirScope],
  );

  // Filter chip options derived from what is actually loaded.
  const chipOptions = useMemo(() => deriveChipOptions(graphNodes.values()), [graphNodes]);

  // The canvas self-populates with the default view, so opening a filter
  // from Overview only needs to set the chips and switch views.
  const openFilteredCanvas = useCallback((kind: string | null, language: string | null) => {
    setView("canvas");
    if (kind) setKindFilters(new Set([kind]));
    if (language) setLangFilters(new Set([language]));
  }, []);

  const pathIds = pathResult?.found ? pathResult.path : [];

  /** Merges only the caller or callee side of the inspected node's neighborhood. */
  const showDirectedNeighbors = useCallback(
    (direction: "callers" | "callees") => {
      if (!selected || !neighbors) return;
      const targets = neighbors[direction];
      const edges: GraphEdge[] = targets.map((node) =>
        direction === "callers"
          ? { source: node.id, target: selected.id, kind: "calls" }
          : { source: selected.id, target: node.id, kind: "calls" },
      );
      defaultViewRef.current = false;
      mergeGraph([selected, ...targets], edges);
      setFocusId(selected.id);
    },
    [selected, neighbors, mergeGraph],
  );

  return (
    <div className="tsg-root">
      <div className="tsg-toolbar">
        <div className="tsg-toolbar-left">
          <span className="tsg-kicker">Code Graph</span>
          <nav className="tsg-views">
            <button
              className={cn("tsg-view-tab", view === "overview" && "tsg-view-tab-active")}
              onClick={() => setView("overview")}
            >
              Overview
            </button>
            <button
              className={cn("tsg-view-tab", view === "canvas" && "tsg-view-tab-active")}
              onClick={() => setView("canvas")}
            >
              Canvas
            </button>
          </nav>
        </div>
        <div className="tsg-searchbox" ref={searchBoxRef} onKeyDown={onSearchKeyDown}>
          <Input
            value={query}
            onChange={(event: React.ChangeEvent<HTMLInputElement>) => onQueryChange(event.target.value)}
            onFocus={() => query && setSearchOpen(true)}
            placeholder="Search symbols, qualified names, signatures, paths…"
            aria-label="Search code graph"
          />
          {searchOpen && results.length > 0 && (
            <div className="tsg-search-pop" role="listbox" aria-label="Search results">
              {results.map((node) => (
                <button key={node.id} role="option" onClick={() => focusSymbol(node)}>
                  <i style={{ background: colorForKind(node.kind) }} />
                  <span>{node.name}</span>
                  <small>{node.kind} · {short(node.file_path, 40)} · {fmt(node.degree)} edges</small>
                </button>
              ))}
            </div>
          )}
        </div>
        {overview && (
          <div className="tsg-totals">
            <span>{fmt(overview.totals.nodes)} nodes</span>
            <span>{fmt(overview.totals.edges)} edges</span>
            <span>{fmt(overview.totals.files)} files</span>
          </div>
        )}
      </div>

      {error && (
        <div onClick={() => setError("")} style={{ cursor: "pointer" }}>
          <ErrorPanel error={error} />
        </div>
      )}

      {view === "overview" ? (
        <OverviewPanel
          overview={overview}
          onFocusSymbol={focusSymbol}
          onFilterKind={(family) => openFilteredCanvas(family, null)}
          onFilterLanguage={(language) => openFilteredCanvas(null, language)}
        />
      ) : (
        <>
          <div className="tsg-controls">
            <div className="tsg-breadcrumbs">
              {history.length === 0 ? (
                <span className="tsg-breadcrumb-hint">
                  Most-connected symbols — search to focus, double-click to expand.
                </span>
              ) : (
                history.map((entry, index) => (
                  <React.Fragment key={entry.id}>
                    {index > 0 && <span className="tsg-crumb-sep">›</span>}
                    <button
                      className={cn("tsg-crumb", focusId === entry.id && "tsg-crumb-active")}
                      onClick={() => {
                        setFocusId(entry.id);
                        void inspect(entry.id);
                      }}
                    >
                      {entry.name}
                    </button>
                  </React.Fragment>
                ))
              )}
            </div>
            <div className="tsg-control-buttons">
              <Button
                size="sm"
                variant={pathMode ? undefined : "outline"}
                onClick={() => {
                  setPathMode((prev) => !prev);
                  setPathFrom(null);
                  setPathTo(null);
                  setPathResult(null);
                }}
                title="Pick two nodes to find the shortest path between them. Click again to exit."
              >
                {!pathMode
                  ? "Find path"
                  : !pathFrom
                    ? "Path: pick start"
                    : !pathTo
                      ? `Path: ${short(pathFrom.name, 16)} → pick target`
                      : loading
                        ? `Path: ${short(pathFrom.name, 14)} → ${short(pathTo.name, 14)} · finding…`
                        : pathResult?.found
                          ? `Path: ${short(pathFrom.name, 14)} → ${short(pathTo.name, 14)} · ${Math.max(0, pathResult.path.length - 1)} hop${pathResult.path.length === 2 ? "" : "s"}`
                          : `Path: ${short(pathFrom.name, 14)} → ${short(pathTo.name, 14)} · none`}
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => setFitSignal((prev) => prev + 1)}
                title="Zoom to fit every loaded node"
              >
                Fit
              </Button>
              <Button size="sm" variant="outline" onClick={clearCanvas}>Clear</Button>
            </div>
          </div>

          {(chipOptions.families.length > 1 || chipOptions.languages.length > 1 || dirScope) && (
            <div className="tsg-chip-bar">
              {chipOptions.families.map((family) => (
                <button
                  key={family}
                  className={cn("tsg-chip", kindFilters.has(family) && "tsg-chip-active")}
                  onClick={() => setKindFilters((prev) => toggleStringSet(prev, family))}
                >
                  <i style={{ background: KIND_FAMILY_COLORS[family] }} />
                  {KIND_FAMILY_LABELS[family] || family}
                </button>
              ))}
              <span className="tsg-chip-sep" />
              {chipOptions.languages.map((language) => (
                <button
                  key={language}
                  className={cn("tsg-chip", langFilters.has(language) && "tsg-chip-active")}
                  onClick={() => setLangFilters((prev) => toggleStringSet(prev, language))}
                >
                  {language}
                </button>
              ))}
              <input
                className="tsg-scope-input"
                value={dirScope}
                onChange={(event) => setDirScope(event.target.value)}
                placeholder="scope: src/dashboard/"
                aria-label="Directory scope filter"
              />
              {(kindFilters.size > 0 || langFilters.size > 0 || dirScope) && (
                <button
                  className="tsg-chip tsg-chip-clear"
                  onClick={() => {
                    setKindFilters(new Set());
                    setLangFilters(new Set());
                    setDirScope("");
                  }}
                >
                  reset filters
                </button>
              )}
            </div>
          )}

          <div className="tsg-canvas-layout">
            <div className="tsg-canvas-shell">
              {visible.nodes.length === 0 ? (
                <div className="tsg-graph-empty">
                  {canvasEmptyMessage({
                    indexedNodes: overview ? overview.totals.nodes : null,
                    loadedNodes: graphNodes.size,
                    loading,
                  })}
                </div>
              ) : (
                <GraphCanvas
                  nodes={visible.nodes}
                  edges={visible.edges}
                  focusId={focusId}
                  selectedId={selected?.id || null}
                  pathIds={pathIds}
                  fitSignal={fitSignal}
                  onSelect={selectInCanvas}
                  onExpand={(id) => void expandNode(id)}
                />
              )}
              <div className="tsg-canvas-footer">
                <Legend />
                <span className="tsg-canvas-count">
                  {fmt(visible.nodes.length)} / {fmt(graphNodes.size)} nodes ·{" "}
                  {fmt(visible.edges.length)} edges{loading ? " · loading…" : ""}
                </span>
              </div>
            </div>
            <DetailPanel
              node={selected}
              neighbors={neighbors}
              onJump={(node) => focusSymbol(node)}
              onShowCallers={() => showDirectedNeighbors("callers")}
              onShowCallees={() => showDirectedNeighbors("callees")}
            />
          </div>
        </>
      )}
    </div>
  );
}
