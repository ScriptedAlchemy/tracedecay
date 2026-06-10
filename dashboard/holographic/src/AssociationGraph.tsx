import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FocusEvent, KeyboardEvent } from "react";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import type { HolographicGraphNode } from "./types";
import { NUM_BADGE } from "./ui";
import { buildAdjacency } from "./associationGraphAdjacency";
import { computeBounds, useGraphLayout } from "./associationGraphLayout";
import {
  EDGE_LEGEND,
  GRAPH_HEIGHT,
  GRAPH_WIDTH,
  KIND_ORDER,
  type Neighbor,
  type SimNode,
  type ViewBox,
  type WeightedGraphEdge,
} from "./associationGraphTypes";
import {
  coOccurrenceWeight,
  colorOf,
  edgeStyle,
  linkEndpoint,
} from "./associationGraphUtils";
import { GraphEdges, GraphLabels, GraphNodes, GraphSettleLayer } from "./GraphSvgLayers";
import { nearestInDirection, resolveVisibleLabels } from "./graphHitTest";
import {
  animateViewBox,
  cancelViewAnimation,
  createPointerHandlers,
  createTouchHandlers,
  handleWheelZoom,
  type ViewAnimRef,
} from "./graphViewBox";
import { NodeDetailPanel } from "./NodeDetailPanel";
import { Spinner } from "./Spinner";

const EMPTY_LABELS: Set<string> = new Set();

export default function AssociationGraph({
  graph,
}: {
  graph: { nodes: HolographicGraphNode[]; edges: WeightedGraphEdge[] };
}) {
  const [minDegree, setMinDegree] = useState(2);
  const [hiddenKinds, setHiddenKinds] = useState<Set<string>>(() => new Set());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [hoverId, setHoverId] = useState<string | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [focusWithin, setFocusWithin] = useState(false);
  const [edgesShown, setEdgesShown] = useState(false);
  const [view, setView] = useState<ViewBox>({ x: 0, y: 0, w: GRAPH_WIDTH, h: GRAPH_HEIGHT });

  const svgRef = useRef<SVGSVGElement | null>(null);
  const dragRef = useRef<{ x: number; y: number } | null>(null);
  const pinchRef = useRef<{
    id1: number;
    id2: number;
    x1: number; y1: number;
    x2: number; y2: number;
    view: ViewBox;
  } | null>(null);
  const animRef = useRef<number | null>(null) as ViewAnimRef;
  const viewRef = useRef(view);
  viewRef.current = view;
  const nodeElRef = useRef<Map<string, SVGGElement>>(new Map());
  const userInteractedRef = useRef(false);
  const hoverRafRef = useRef<number | null>(null);
  const pendingHoverRef = useRef<string | null>(null);
  // Live mirrors so the node-level callbacks can stay referentially stable
  // (keeps `GraphNodes` memoized across pan/zoom).
  const visibleNodesRef = useRef<SimNode[]>([]);
  const selectedIdRef = useRef<string | null>(null);
  selectedIdRef.current = selectedId;

  const nodeById = useMemo(
    () => new Map(graph.nodes.map((node) => [node.id, node])),
    [graph],
  );
  const { degreeMap, maxEntityDegree, adjacency } = useMemo(
    () => buildAdjacency(graph.nodes, graph.edges),
    [graph],
  );

  const layout = useGraphLayout(graph.nodes, graph.edges, degreeMap);
  const settling = layout.progress !== null;
  const sliderMax = Math.max(2, maxEntityDegree);

  const { visibleIds, visibleNodes, hiddenEntities } = useMemo(() => {
    const threshold = Math.min(minDegree, maxEntityDegree);
    const ids = new Set<string>();
    const vis: SimNode[] = [];
    let hidden = 0;
    for (const node of layout.simNodes) {
      const kindHidden = hiddenKinds.has(node.kind);
      const degreeOk = node.kind !== "entity" || node.degree >= threshold;
      if (!degreeOk && node.kind === "entity") hidden += 1;
      if (!kindHidden && degreeOk) {
        ids.add(node.id);
        vis.push(node);
      }
    }
    return { visibleIds: ids, visibleNodes: vis, hiddenEntities: hidden };
  }, [layout.simNodes, minDegree, maxEntityDegree, hiddenKinds]);
  visibleNodesRef.current = visibleNodes;

  const maxWeight = useMemo(() => {
    let max = 1;
    for (const link of layout.simLinks) {
      const source = linkEndpoint(link.source);
      const target = linkEndpoint(link.target);
      if (!source || !target) continue;
      if (!visibleIds.has(source.id) || !visibleIds.has(target.id)) continue;
      max = Math.max(max, coOccurrenceWeight(link, source, target));
    }
    return max;
  }, [layout.simLinks, visibleIds]);

  // Selection wins for highlighting; otherwise hover, otherwise keyboard focus.
  const highlightAnchorId =
    selectedId ?? hoverId ?? (focusWithin ? activeId : null);
  const highlightIds = useMemo(() => {
    if (!highlightAnchorId || !visibleIds.has(highlightAnchorId)) return null;
    const set = new Set<string>([highlightAnchorId]);
    for (const nb of adjacency.get(highlightAnchorId) ?? []) {
      if (visibleIds.has(nb.id)) set.add(nb.id);
    }
    return set;
  }, [highlightAnchorId, adjacency, visibleIds]);

  const zoom = GRAPH_WIDTH / view.w;
  // Quantize so panning never changes the label props (keeps GraphNodes
  // memoized → pan is a pure viewBox attribute update); the font/label set only
  // recompute as you cross a zoom step.
  const quantZoom = Math.round(zoom * 5) / 5;
  const worldLabelFont = 10 / quantZoom;
  const labeledIds = useMemo(() => {
    if (settling) return EMPTY_LABELS;
    return resolveVisibleLabels(visibleNodes, highlightIds, worldLabelFont);
  }, [visibleNodes, highlightIds, worldLabelFont, settling]);

  const ringId = hoverId ?? (focusWithin ? activeId : null);

  // --- view helpers -------------------------------------------------------
  const fitTo = (nodes: SimNode[], duration = 450) => {
    if (nodes.length === 0) return;
    userInteractedRef.current = true;
    animateViewBox(viewRef.current, computeBounds(nodes), duration, setView, animRef);
  };
  const onInteract = () => {
    userInteractedRef.current = true;
    cancelViewAnimation(animRef);
  };

  // Track the expanding layout while settling (until the user grabs the view).
  useEffect(() => {
    if (settling && !userInteractedRef.current && visibleNodes.length > 0) {
      setView(computeBounds(visibleNodes));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layout.frame]);

  // Reset interaction/fade state when a fresh layout starts; fade edges in once
  // it settles.
  useEffect(() => {
    if (settling) {
      userInteractedRef.current = false;
      setEdgesShown(false);
      return;
    }
    const raf = requestAnimationFrame(() => setEdgesShown(true));
    return () => cancelAnimationFrame(raf);
  }, [settling]);

  // Re-frame smoothly when the degree/kind filter changes after settling.
  const didFilterMount = useRef(false);
  useEffect(() => {
    if (!didFilterMount.current) {
      didFilterMount.current = true;
      return;
    }
    if (!settling && visibleNodes.length > 0) {
      animateViewBox(viewRef.current, computeBounds(visibleNodes), 450, setView, animRef);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [minDegree, hiddenKinds]);

  // Keep a sensible roving-tabindex target as filters/selection change.
  useEffect(() => {
    setActiveId((cur) => {
      if (cur && visibleIds.has(cur)) return cur;
      let best: string | null = null;
      let bestDeg = -1;
      for (const node of visibleNodes) {
        if (node.degree > bestDeg) {
          bestDeg = node.degree;
          best = node.id;
        }
      }
      return best;
    });
    setSelectedId((cur) => (cur && !visibleIds.has(cur) ? null : cur));
  }, [visibleIds, visibleNodes]);

  // Wheel zoom (Ctrl/Cmd or focused graph only — see graphViewBox). Bound via
  // callback ref, not a mount-once effect: the svg can mount late when the
  // graph starts empty and data arrives on a later refresh, and a non-passive
  // listener is required to preventDefault page scroll.
  const wheelCleanupRef = useRef<(() => void) | null>(null);
  const bindSvg = useCallback((node: SVGSVGElement | null) => {
    svgRef.current = node;
    wheelCleanupRef.current?.();
    wheelCleanupRef.current = null;
    if (!node) return;
    const onWheel = (event: WheelEvent) =>
      handleWheelZoom(event, node, setView, () => {
        userInteractedRef.current = true;
        cancelViewAnimation(animRef);
      });
    node.addEventListener("wheel", onWheel, { passive: false });
    wheelCleanupRef.current = () => node.removeEventListener("wheel", onWheel);
  }, []);
  useEffect(() => () => wheelCleanupRef.current?.(), []);

  useEffect(() => () => cancelViewAnimation(animRef), []);

  const { onPointerDown, onPointerMove, onPointerUp } = createPointerHandlers(
    dragRef,
    view,
    setView,
    onInteract,
  );
  const { onTouchStart, onTouchMove, onTouchEnd } = createTouchHandlers(
    svgRef,
    dragRef,
    pinchRef,
    view,
    setView,
    onInteract,
  );

  // --- selection / hover / keyboard --------------------------------------
  // These callbacks are referentially stable (state read via refs) so the
  // memoized GraphNodes does not re-render while panning/zooming.
  const selectNode = useCallback((id: string | null) => {
    setSelectedId(id);
    if (id) setActiveId(id);
  }, []);

  const queueHover = useCallback((id: string | null) => {
    pendingHoverRef.current = id;
    if (hoverRafRef.current === null) {
      hoverRafRef.current = requestAnimationFrame(() => {
        hoverRafRef.current = null;
        setHoverId(pendingHoverRef.current);
      });
    }
  }, []);
  const handleHoverLeave = useCallback(() => queueHover(null), [queueHover]);
  useEffect(
    () => () => {
      if (hoverRafRef.current !== null) cancelAnimationFrame(hoverRafRef.current);
    },
    [],
  );

  const ensureNodeInView = useCallback((node: SimNode) => {
    const v = viewRef.current;
    const margin = Math.min(v.w, v.h) * 0.12;
    const x = node.x ?? 0;
    const y = node.y ?? 0;
    if (x < v.x + margin || x > v.x + v.w - margin || y < v.y + margin || y > v.y + v.h - margin) {
      animateViewBox(v, { x: x - v.w / 2, y: y - v.h / 2, w: v.w, h: v.h }, 280, setView, animRef);
    }
  }, []);

  const focusNode = useCallback((id: string) => {
    setActiveId(id);
    nodeElRef.current.get(id)?.focus({ preventScroll: true });
  }, []);

  const onNodeKeyDown = useCallback(
    (event: KeyboardEvent<SVGGElement>, id: string) => {
      if (event.key === "Escape") {
        if (selectedIdRef.current) {
          event.preventDefault();
          setSelectedId(null);
        }
        return;
      }
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        selectNode(id);
        return;
      }
      const nodes = visibleNodesRef.current;
      if (event.key === "Home") {
        event.preventDefault();
        let best: SimNode | null = null;
        for (const node of nodes) if (!best || node.degree > best.degree) best = node;
        if (best) {
          focusNode(best.id);
          ensureNodeInView(best);
        }
        return;
      }
      const dir =
        event.key === "ArrowRight"
          ? [1, 0]
          : event.key === "ArrowLeft"
            ? [-1, 0]
            : event.key === "ArrowDown"
              ? [0, 1]
              : event.key === "ArrowUp"
                ? [0, -1]
                : null;
      if (!dir) return;
      const from = nodes.find((node) => node.id === id);
      if (!from) return;
      const next = nearestInDirection(nodes, from, dir[0], dir[1]);
      if (next) {
        event.preventDefault();
        focusNode(next.id);
        ensureNodeInView(next);
      }
    },
    [selectNode, focusNode, ensureNodeInView],
  );

  const onNodeFocus = useCallback((id: string) => {
    setFocusWithin(true);
    setActiveId(id);
  }, []);

  const registerNodeEl = useCallback((id: string, el: SVGGElement | null) => {
    if (el) nodeElRef.current.set(id, el);
    else nodeElRef.current.delete(id);
  }, []);

  const onSvgFocus = () => setFocusWithin(true);
  const onSvgBlur = (event: FocusEvent<SVGSVGElement>) => {
    const next = event.relatedTarget as Node | null;
    if (!next || !svgRef.current?.contains(next)) setFocusWithin(false);
  };

  const selected = selectedId ? (nodeById.get(selectedId) ?? null) : null;
  const selectedNeighbors = selectedId ? (adjacency.get(selectedId) ?? []) : [];
  const groupedNeighbors: Record<string, Neighbor[]> = {};
  for (const nb of selectedNeighbors) {
    (groupedNeighbors[nb.kind] ??= []).push(nb);
  }
  const factNeighborCount = groupedNeighbors.fact?.length ?? 0;

  const frameSelected = () => {
    if (!selected) return;
    const ids = new Set<string>([selected.id]);
    for (const nb of adjacency.get(selected.id) ?? []) {
      if (visibleIds.has(nb.id)) ids.add(nb.id);
    }
    const nodes = visibleNodes.filter((node) => ids.has(node.id));
    fitTo(nodes.length ? nodes : visibleNodes, 450);
  };

  if (graph.nodes.length === 0) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Association Graph</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="flex h-[42rem] items-center justify-center border border-border bg-background/30 text-xs text-text-tertiary">
            No graph data.
          </div>
        </CardContent>
      </Card>
    );
  }

  const progressPct = Math.round((layout.progress ?? 0) * 100);

  return (
    <div
      className={
        selected
          ? "grid min-w-0 items-start gap-4 lg:grid-cols-[minmax(0,1fr)_22rem]"
          : "grid min-w-0 gap-4"
      }
    >
      <Card>
        <CardHeader>
          <CardTitle>Association Graph</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <p className="text-xs leading-relaxed text-text-tertiary">
            Force-directed view of facts, the entities they mention, and their
            category banks. Click or arrow-key to a node to focus its
            neighborhood, hover to preview connections, then walk them in the
            panel. Raise the degree filter or toggle a kind in the legend to
            declutter.
          </p>

          <div className="flex flex-wrap items-center gap-4">
            <label className="flex min-w-0 flex-1 items-center gap-3">
              <span className="shrink-0 font-mono-ui text-xs text-text-secondary">
                min degree {minDegree}
              </span>
              <input
                type="range"
                min={1}
                max={sliderMax}
                value={Math.min(minDegree, sliderMax)}
                onChange={(e) => setMinDegree(Number(e.target.value))}
                className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                aria-label="Minimum entity degree"
              />
              <span className="shrink-0 font-mono-ui text-xs text-text-tertiary">
                {sliderMax}
              </span>
            </label>
            <Button ghost size="xs" onClick={() => fitTo(visibleNodes)}>
              Fit
            </Button>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            {KIND_ORDER.map((kind) => {
              const off = hiddenKinds.has(kind);
              return (
                <button
                  key={kind}
                  type="button"
                  role="switch"
                  aria-checked={!off}
                  aria-label={`${off ? "Show" : "Hide"} ${kind} nodes`}
                  onClick={() =>
                    setHiddenKinds((prev) => {
                      const next = new Set(prev);
                      if (next.has(kind)) next.delete(kind);
                      else next.add(kind);
                      return next;
                    })
                  }
                  className={`flex items-center gap-1.5 rounded-full border px-2 py-0.5 ${
                    off ? "" : "border-border hover:border-primary/60"
                  }`}
                  style={{
                    opacity: off ? 0.4 : 1,
                    borderColor: off ? "transparent" : undefined,
                    transition: "opacity 0.16s ease, border-color 0.16s ease",
                  }}
                >
                  <span
                    className="inline-block h-2.5 w-2.5 rounded-full"
                    style={{ backgroundColor: colorOf(kind) }}
                  />
                  <span
                    className={`font-mono-ui text-xs text-text-tertiary ${
                      off ? "line-through" : ""
                    }`}
                  >
                    {kind}
                  </span>
                </button>
              );
            })}
            {hiddenEntities > 0 && (
              <Badge tone="outline" className={NUM_BADGE}>
                {hiddenEntities} entities hidden
              </Badge>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 border-t border-border pt-2">
            <span className="font-mono-ui text-xs text-text-tertiary">
              links · thicker = stronger co-occurrence
            </span>
            {EDGE_LEGEND.map((edge) => (
              <span key={edge.kind} className="flex items-center gap-1.5">
                <svg
                  width="20"
                  height="8"
                  viewBox="0 0 20 8"
                  aria-hidden="true"
                  className="shrink-0"
                >
                  <line
                    x1="1"
                    y1="4"
                    x2="19"
                    y2="4"
                    stroke={edgeStyle(edge.kind).color}
                    strokeDasharray={edgeStyle(edge.kind).dash}
                    strokeWidth={edge.bold ? 2.6 : 1.6}
                    strokeOpacity={edge.bold ? 0.95 : 0.9}
                    strokeLinecap="round"
                  />
                </svg>
                <span className="font-mono-ui text-xs text-text-secondary">
                  {edge.kind}
                </span>
                <span className="font-mono-ui text-xs text-text-tertiary">
                  {edge.relation}
                </span>
              </span>
            ))}
          </div>

          <div className="relative overflow-hidden border border-border bg-background/30 touch-none">
            <svg
              ref={bindSvg}
              role="group"
              aria-label={`Association graph: ${visibleNodes.length} nodes. Tab to enter, arrow keys to move between nodes, Enter to focus a node, Escape to clear.`}
              viewBox={`${view.x} ${view.y} ${view.w} ${view.h}`}
              className="h-[60vh] md:h-[42rem] w-full select-none"
              preserveAspectRatio="xMidYMid meet"
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerLeave={onPointerUp}
              onTouchStart={onTouchStart}
              onTouchMove={onTouchMove}
              onTouchEnd={onTouchEnd}
              onFocus={onSvgFocus}
              onBlur={onSvgBlur}
            >
              {settling ? (
                <GraphSettleLayer nodes={visibleNodes} />
              ) : (
                <>
                  <g
                    style={{
                      opacity: edgesShown ? 1 : 0,
                      transition: "opacity 280ms ease-out",
                    }}
                  >
                    <GraphEdges
                      links={layout.simLinks}
                      visibleIds={visibleIds}
                      highlightId={highlightAnchorId}
                      highlightIds={highlightIds}
                      maxWeight={maxWeight}
                    />
                  </g>
                  <GraphNodes
                    nodes={visibleNodes}
                    selectedId={selectedId}
                    activeId={activeId}
                    ringId={ringId}
                    highlightIds={highlightIds}
                    registerNodeEl={registerNodeEl}
                    onSelect={selectNode}
                    onHoverEnter={queueHover}
                    onHoverLeave={handleHoverLeave}
                    onNodeKeyDown={onNodeKeyDown}
                    onNodeFocus={onNodeFocus}
                  />
                  <GraphLabels
                    nodes={visibleNodes}
                    labeledIds={labeledIds}
                    worldLabelFont={worldLabelFont}
                  />
                </>
              )}
            </svg>

            {settling && (
              <div
                className="pointer-events-none absolute flex items-center gap-2 rounded-full border border-border px-2 py-1"
                style={{
                  bottom: "0.5rem",
                  left: "0.5rem",
                  background: "var(--hm-panel, var(--color-card))",
                }}
                role="status"
                aria-live="polite"
              >
                <Spinner className="text-primary" aria-label="Laying out graph" />
                <span className="font-mono-ui text-xs text-text-tertiary">
                  Laying out… {progressPct}%
                </span>
              </div>
            )}

            <span
              className="pointer-events-none absolute font-mono-ui text-[0.65rem] text-text-tertiary"
              style={{ bottom: "0.5rem", right: "0.5rem", opacity: 0.7 }}
            >
              ⌘/Ctrl + scroll to zoom · drag to pan
            </span>
          </div>
        </CardContent>
      </Card>

      {selected && (
        <NodeDetailPanel
          selected={selected}
          degreeMap={degreeMap}
          groupedNeighbors={groupedNeighbors}
          selectedNeighbors={selectedNeighbors}
          factNeighborCount={factNeighborCount}
          onSelect={selectNode}
          onClear={() => selectNode(null)}
          onFrame={frameSelected}
        />
      )}
    </div>
  );
}
