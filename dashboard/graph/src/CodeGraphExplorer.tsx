import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "./api";
import GraphCanvas from "./GraphCanvas";
import OverviewPanel from "./OverviewPanel";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Input, cn } from "./sdk";
import {
  colorForKind,
  KIND_FAMILY_COLORS,
  KIND_FAMILY_LABELS,
  kindFamily,
  languageForPath,
} from "./types";
import type {
  GraphEdge,
  GraphNeighborsResponse,
  GraphNode,
  GraphOverview,
  GraphPathResponse,
} from "./types";

const ShellCard = Card || "div";
const ShellCardHeader = CardHeader || "div";
const ShellCardTitle = CardTitle || "h3";
const ShellCardContent = CardContent || "div";
const ShellBadge = Badge || "span";
const ShellButton = Button || "button";
const ShellInput = Input || "input";

/** Soft cap on accumulated canvas nodes; expansion pauses above this. */
const CANVAS_NODE_CAP = 600;

function fmt(n: number | undefined) {
  return Number(n || 0).toLocaleString();
}

function short(text: string | null | undefined, max = 72) {
  const raw = String(text || "");
  return raw.length > max ? `${raw.slice(0, max - 1)}…` : raw;
}

function edgeKey(edge: GraphEdge) {
  return `${edge.source}>${edge.target}:${edge.kind}`;
}

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
      <ShellCard className="tsg-panel">
        <ShellCardHeader><ShellCardTitle>Inspector</ShellCardTitle></ShellCardHeader>
        <ShellCardContent>
          <div className="tsg-empty">
            Click a node to inspect it. Double-click (or use the +N badge counts as a guide) to
            expand its neighbors into the canvas.
          </div>
        </ShellCardContent>
      </ShellCard>
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
    <ShellCard className="tsg-panel">
      <ShellCardHeader>
        <ShellCardTitle>{node.name}</ShellCardTitle>
        <div className="tsg-panel-badges">
          <ShellBadge>{node.kind}</ShellBadge>
          {node.visibility && <ShellBadge>{node.visibility}</ShellBadge>}
          <ShellBadge>{fmt(node.degree)} edges</ShellBadge>
        </div>
      </ShellCardHeader>
      <ShellCardContent>
        <div className="tsg-detail">
          <code>{node.qualified_name}</code>
          <span>{node.file_path}:{node.span?.start_line || 0}-{node.span?.end_line || 0}</span>
          {node.signature && <pre>{node.signature}</pre>}
          {node.doc && <p>{short(node.doc, 360)}</p>}
        </div>
        <div className="tsg-panel-actions">
          <ShellButton size="sm" onClick={onShowCallers}>Show callers</ShellButton>
          <ShellButton size="sm" onClick={onShowCallees}>Show callees</ShellButton>
        </div>
        {renderList("Callers", neighbors?.callers || [])}
        {renderList("Callees", neighbors?.callees || [])}
        <section className="tsg-edge-kinds">
          {(neighbors?.edges_by_kind || []).map((row) => (
            <ShellBadge key={row.kind}>{row.kind}: {fmt(row.count)}</ShellBadge>
          ))}
        </section>
      </ShellCardContent>
    </ShellCard>
  );
}

export default function CodeGraphExplorer() {
  const [view, setView] = useState<"overview" | "canvas">("overview");
  const [overview, setOverview] = useState<GraphOverview | null>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<GraphNode[]>([]);
  const [searchOpen, setSearchOpen] = useState(false);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  // Accumulated canvas graph (progressive exploration).
  const [graphNodes, setGraphNodes] = useState<Map<string, GraphNode>>(new Map());
  const [graphEdges, setGraphEdges] = useState<Map<string, GraphEdge>>(new Map());
  const [focusId, setFocusId] = useState<string | null>(null);
  const [fitSignal, setFitSignal] = useState(0);
  const [selected, setSelected] = useState<GraphNode | null>(null);
  const [neighbors, setNeighbors] = useState<GraphNeighborsResponse | null>(null);
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

  const searchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchSeq = useRef(0);
  const searchBoxRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    api.overview().then(setOverview).catch((err) => setError(String(err)));
  }, []);

  const mergeGraph = useCallback((nodes: GraphNode[], edges: GraphEdge[]) => {
    setGraphNodes((prev) => {
      const next = new Map(prev);
      for (const node of nodes) next.set(node.id, { ...next.get(node.id), ...node });
      return next;
    });
    setGraphEdges((prev) => {
      const next = new Map(prev);
      for (const edge of edges) next.set(edgeKey(edge), edge);
      return next;
    });
  }, []);

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
  }, []);

  const expandNode = useCallback(
    async (id: string, { focus = false } = {}) => {
      if (graphNodes.size >= CANVAS_NODE_CAP) {
        setError(`Canvas holds ${graphNodes.size} nodes — clear it to keep expanding.`);
        return;
      }
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

  // Sequenced like the search dropdown: rapid clicks can resolve out of
  // order, and a stale response must not repoint the Inspector (and the
  // "Show callers/callees" actions) at the wrong node.
  const inspectSeq = useRef(0);
  const inspect = useCallback(async (id: string) => {
    const seq = ++inspectSeq.current;
    try {
      const [detail, nextNeighbors] = await Promise.all([
        api.node(id),
        api.neighbors(id, { limit: 60 }),
      ]);
      if (seq !== inspectSeq.current) return;
      setSelected(detail.node);
      setNeighbors(nextNeighbors);
    } catch (err) {
      if (seq === inspectSeq.current) setError(String(err));
    }
  }, []);

  const runPath = useCallback(
    async (from: GraphNode, to: GraphNode) => {
      setLoading(true);
      setError("");
      try {
        const payload = await api.path({ from: from.id, to: to.id, max_depth: 8 });
        setPathResult(payload);
        if (payload.found) {
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
      setHistory((prev) => {
        const trimmed = prev.filter((entry) => entry.id !== node.id);
        return [...trimmed.slice(-7), { id: node.id, name: node.name }];
      });
      void expandNode(node.id, { focus: true });
      void inspect(node.id);
    },
    [expandNode, inspect],
  );

  /** Keyboard support for the search dropdown: Enter opens the top hit, arrows
   *  move focus through the results, Escape closes the popup. */
  const onSearchKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLElement>) => {
      const box = searchBoxRef.current;
      if (!box) return;
      if (event.key === "Escape") {
        event.preventDefault();
        setSearchOpen(false);
        box.querySelector<HTMLInputElement>("input")?.focus();
        return;
      }
      if (!searchOpen || results.length === 0) return;
      const buttons = Array.from(
        box.querySelectorAll<HTMLButtonElement>(".tsg-search-pop button"),
      );
      const active = document.activeElement as HTMLElement | null;
      const index = buttons.indexOf(active as HTMLButtonElement);
      if (event.key === "Enter") {
        // Enter from the input jumps to the top result; from a focused row it
        // opens that row.
        event.preventDefault();
        if (index >= 0 && index < results.length) focusSymbol(results[index]);
        else focusSymbol(results[0]);
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        const next = index < 0 ? buttons[0] : buttons[Math.min(index + 1, buttons.length - 1)];
        next?.focus();
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        if (index <= 0) box.querySelector<HTMLInputElement>("input")?.focus();
        else buttons[index - 1]?.focus();
      }
    },
    [searchOpen, results, focusSymbol],
  );

  const onQueryChange = useCallback((value: string) => {
    setQuery(value);
    setSearchOpen(true);
    if (searchTimer.current) clearTimeout(searchTimer.current);
    searchTimer.current = setTimeout(() => {
      const seq = ++searchSeq.current;
      api.search({ q: value, limit: 20 })
        .then((payload) => {
          // Drop stale responses that resolve after a newer query.
          if (seq === searchSeq.current) setResults(payload.results);
        })
        .catch((err) => setError(String(err)));
    }, 180);
  }, []);

  // Visible (filtered) slice of the accumulated canvas graph.
  const visible = useMemo(() => {
    const nodes: GraphNode[] = [];
    const keep = new Set<string>();
    for (const node of graphNodes.values()) {
      if (kindFilters.size > 0 && !kindFilters.has(kindFamily(node.kind))) continue;
      if (langFilters.size > 0 && !langFilters.has(languageForPath(node.file_path))) continue;
      if (dirScope && !node.file_path.startsWith(dirScope)) continue;
      nodes.push(node);
      keep.add(node.id);
    }
    const edges: GraphEdge[] = [];
    for (const edge of graphEdges.values()) {
      if (keep.has(edge.source) && keep.has(edge.target)) edges.push(edge);
    }
    return { nodes, edges };
  }, [graphNodes, graphEdges, kindFilters, langFilters, dirScope]);

  // Filter chip options derived from what is actually loaded.
  const chipOptions = useMemo(() => {
    const families = new Set<string>();
    const languages = new Set<string>();
    for (const node of graphNodes.values()) {
      families.add(kindFamily(node.kind));
      languages.add(languageForPath(node.file_path));
    }
    return { families: [...families].sort(), languages: [...languages].sort() };
  }, [graphNodes]);

  const toggleSet = (set: Set<string>, value: string) => {
    const next = new Set(set);
    if (next.has(value)) next.delete(value);
    else next.add(value);
    return next;
  };

  const openFilteredCanvas = useCallback(
    (kind: string | null, language: string | null) => {
      setView("canvas");
      if (kind) setKindFilters(new Set([kind]));
      if (language) setLangFilters(new Set([language]));
      // Seed from the most connected symbol so the canvas is not empty.
      const seed = overview?.top_connected?.[0];
      if (seed && graphNodes.size === 0) focusSymbol(seed);
    },
    [overview, graphNodes.size, focusSymbol],
  );

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
          <ShellInput
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
        <div className="tsg-error" onClick={() => setError("")} role="alert">{error}</div>
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
                <span className="tsg-breadcrumb-hint">Search a symbol to start exploring.</span>
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
              <ShellButton
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
              </ShellButton>
              <ShellButton
                size="sm"
                variant="outline"
                onClick={() => setFitSignal((prev) => prev + 1)}
                title="Zoom to fit every loaded node"
              >
                Fit
              </ShellButton>
              <ShellButton size="sm" variant="outline" onClick={clearCanvas}>Clear</ShellButton>
            </div>
          </div>

          {(chipOptions.families.length > 1 || chipOptions.languages.length > 1 || dirScope) && (
            <div className="tsg-chip-bar">
              {chipOptions.families.map((family) => (
                <button
                  key={family}
                  className={cn("tsg-chip", kindFilters.has(family) && "tsg-chip-active")}
                  onClick={() => setKindFilters((prev) => toggleSet(prev, family))}
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
                  onClick={() => setLangFilters((prev) => toggleSet(prev, language))}
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
                  {graphNodes.size === 0
                    ? "The canvas is empty — search a symbol above or pick one from Overview."
                    : "All loaded nodes are hidden by the current filters."}
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
