/**
 * Interactive code-graph canvas.
 *
 * Canvas 2D rendering (not SVG) so a few hundred nodes stay at 60fps:
 * - wheel zoom around the cursor + drag pan,
 * - node drag with the force layout settling around the pinned node,
 * - click to select, double-click to expand neighbors,
 * - hover highlights the direct neighborhood and dims the rest,
 * - labels are culled at low zoom (always shown for hubs / selection),
 * - "+N" badges mark nodes with unexpanded (collapsed) neighbors,
 * - an optional path id set renders a highlighted route.
 */

import React, { useEffect, useRef } from "react";
import {
  edgeStyle,
  fitCameraToNodes,
  hitTestNode,
  neighborhoodIds,
  readTheme,
  toWorldPoint,
  type Camera,
  type CanvasSize,
  type ThemeColors,
  zoomCameraAtPoint,
} from "./canvasHelpers";
import { labelCapForArea, selectLabels, type LabelBox } from "./labelLayout";
import { createSimulation, type SimEdge, type SimNode, type Simulation } from "./simulation";
import { kindFamily } from "./types";
import type { GraphEdge, GraphNode } from "./types";

export interface GraphCanvasProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** Node the camera should fly to (search-to-focus / expansion seed). */
  focusId: string | null;
  selectedId: string | null;
  /** Node ids on the active shortest path, in order. */
  pathIds: string[];
  /** Increment to zoom-to-fit the current node set (camera reset). */
  fitSignal?: number;
  onSelect: (id: string) => void;
  onExpand: (id: string) => void;
}

const DIM_ALPHA = 0.13;

export default function GraphCanvas({
  nodes,
  edges,
  focusId,
  selectedId,
  pathIds,
  fitSignal = 0,
  onSelect,
  onExpand,
}: GraphCanvasProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const simRef = useRef<Simulation | null>(null);
  const cameraRef = useRef<Camera>({ x: 0, y: 0, k: 1 });
  const hoverRef = useRef<SimNode | null>(null);
  const dragRef = useRef<{ node: SimNode | null; panning: boolean; lastX: number; lastY: number }>(
    { node: null, panning: false, lastX: 0, lastY: 0 },
  );
  const needsRenderRef = useRef(true);
  const rafRef = useRef(0);
  // Cached per-frame inputs: CSS-pixel canvas size (refreshed by the
  // ResizeObserver) and resolved theme tokens (dropped on data-theme flips) —
  // getBoundingClientRect/getComputedStyle are too expensive at 60fps.
  const sizeRef = useRef<CanvasSize>({ width: 0, height: 0 });
  const themeRef = useRef<ThemeColors | null>(null);
  const propsRef = useRef({ selectedId, pathIds, focusId, onSelect, onExpand });
  propsRef.current = { selectedId, pathIds, focusId, onSelect, onExpand };

  // Rebuild the simulation when data changes, preserving prior positions.
  useEffect(() => {
    simRef.current = createSimulation(nodes, edges, simRef.current, focusId);
    simRef.current.reheat(0.9);
    needsRenderRef.current = true;
  }, [nodes, edges]); // eslint-disable-line react-hooks/exhaustive-deps

  // Fly to the focused node, then keep tracking it while the layout settles
  // (cleared as soon as the user pans manually).
  const followIdRef = useRef<string | null>(null);
  // Keep the whole graph framed while the layout settles after a fit
  // (cleared on any manual pan/zoom or focus-follow).
  const followFitRef = useRef(false);
  useEffect(() => {
    if (!focusId || !simRef.current) return;
    const node = simRef.current.nodes.find((n) => n.id === focusId);
    if (!node) return;
    const camera = cameraRef.current;
    camera.x = node.x;
    camera.y = node.y;
    camera.k = Math.max(camera.k, 0.8);
    followIdRef.current = focusId;
    followFitRef.current = false;
    needsRenderRef.current = true;
  }, [focusId, nodes]);

  useEffect(() => {
    needsRenderRef.current = true;
  }, [selectedId, pathIds]);

  // Zoom-to-fit: frame the bounding box of every simulated node, and keep
  // it framed while the layout is still settling. Recovers from zoom
  // extremes / off-screen drift without clearing the exploration.
  useEffect(() => {
    if (!fitSignal) return;
    const canvas = canvasRef.current;
    const sim = simRef.current;
    if (!canvas || !sim || sim.nodes.length === 0) return;
    fitCameraToNodes(sizeRef.current, sim, cameraRef.current);
    followIdRef.current = null;
    followFitRef.current = true;
    needsRenderRef.current = true;
  }, [fitSignal]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    function toWorld(px: number, py: number) {
      return toWorldPoint(cameraRef.current, canvas.getBoundingClientRect(), { x: px, y: py });
    }

    function hitTest(px: number, py: number): SimNode | null {
      const sim = simRef.current;
      if (!sim) return null;
      return hitTestNode(sim.nodes, cameraRef.current, canvas.getBoundingClientRect(), { x: px, y: py });
    }

    function neighborhood(node: SimNode | null): Set<string> | null {
      const sim = simRef.current;
      if (!node || !sim) return null;
      return neighborhoodIds(node.id, sim.edges);
    }

    function drawArrow(edge: SimEdge, camera: Camera) {
      const { sourceNode: a, targetNode: b } = edge;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const dist = Math.sqrt(dx * dx + dy * dy) || 1;
      const ux = dx / dist;
      const uy = dy / dist;
      const tipX = b.x - ux * (b.radius + 3);
      const tipY = b.y - uy * (b.radius + 3);
      const size = Math.min(7, 5.5 / Math.sqrt(camera.k) + 2);
      ctx.beginPath();
      ctx.moveTo(tipX, tipY);
      ctx.lineTo(tipX - ux * size - uy * size * 0.55, tipY - uy * size + ux * size * 0.55);
      ctx.lineTo(tipX - ux * size + uy * size * 0.55, tipY - uy * size - ux * size * 0.55);
      ctx.closePath();
      ctx.fill();
    }

    function render() {
      const sim = simRef.current;
      const theme = (themeRef.current ??= readTheme(canvas));
      const rect = sizeRef.current;
      const dpr = window.devicePixelRatio || 1;
      if (canvas.width !== Math.round(rect.width * dpr) || canvas.height !== Math.round(rect.height * dpr)) {
        canvas.width = Math.round(rect.width * dpr);
        canvas.height = Math.round(rect.height * dpr);
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, rect.width, rect.height);
      if (!sim) return;

      const camera = cameraRef.current;
      ctx.translate(rect.width / 2, rect.height / 2);
      ctx.scale(camera.k, camera.k);
      ctx.translate(-camera.x, -camera.y);

      const { selectedId: selId, pathIds: path, focusId: focId } = propsRef.current;
      const hover = hoverRef.current;
      const highlight = neighborhood(hover);
      const pathSet = new Set(path);
      const pathPairs = new Set<string>();
      for (let i = 0; i + 1 < path.length; i++) {
        pathPairs.add(`${path[i]}>${path[i + 1]}`);
        pathPairs.add(`${path[i + 1]}>${path[i]}`);
      }

      // --- edges ---
      for (const edge of sim.edges) {
        const onPath = pathPairs.has(`${edge.source}>${edge.target}`);
        const inHighlight =
          !highlight || (highlight.has(edge.source) && highlight.has(edge.target));
        const style = edgeStyle(edge.kind, theme);
        ctx.globalAlpha = inHighlight || onPath ? 1 : DIM_ALPHA;
        ctx.strokeStyle = onPath ? theme.path : style.color;
        ctx.lineWidth = (onPath ? 2.4 : style.width) / Math.sqrt(camera.k);
        ctx.setLineDash(onPath ? [] : style.dash);
        ctx.beginPath();
        ctx.moveTo(edge.sourceNode.x, edge.sourceNode.y);
        ctx.lineTo(edge.targetNode.x, edge.targetNode.y);
        ctx.stroke();
        ctx.setLineDash([]);
        ctx.fillStyle = onPath ? theme.path : style.color;
        drawArrow(edge, camera);
      }

      // --- nodes ---
      for (const node of sim.nodes) {
        const isSelected = node.id === selId;
        const isHovered = hover ? node.id === hover.id : false;
        const onPath = pathSet.has(node.id);
        const inHighlight = !highlight || highlight.has(node.id);
        ctx.globalAlpha = inHighlight || onPath ? 1 : DIM_ALPHA;

        ctx.beginPath();
        ctx.arc(node.x, node.y, node.radius, 0, Math.PI * 2);
        ctx.fillStyle = theme.family[kindFamily(node.kind)] || theme.family.other;
        ctx.fill();
        if (isSelected || isHovered || onPath) {
          ctx.lineWidth = (isSelected ? 3 : 2) / camera.k;
          ctx.strokeStyle = onPath && !isSelected ? theme.path : theme.ring;
          ctx.stroke();
        } else {
          ctx.lineWidth = 1.4 / camera.k;
          ctx.strokeStyle = theme.halo;
          ctx.stroke();
        }

        // Collapsed-neighbor badge: full degree minus visible edges.
        const collapsed = Math.max(0, (node.degree || 0) - node.visibleDegree);
        if (collapsed > 0 && camera.k > 0.35) {
          const bx = node.x + node.radius * 0.85;
          const by = node.y - node.radius * 0.85;
          const br = Math.max(6.5, 8 / camera.k);
          ctx.beginPath();
          ctx.arc(bx, by, br, 0, Math.PI * 2);
          ctx.fillStyle = theme.halo;
          ctx.fill();
          ctx.lineWidth = 1 / camera.k;
          ctx.strokeStyle = theme.path;
          ctx.stroke();
          ctx.fillStyle = theme.path;
          ctx.font = `${Math.max(8, 9 / camera.k)}px "IBM Plex Mono", monospace`;
          ctx.textAlign = "center";
          ctx.textBaseline = "middle";
          ctx.fillText(`+${collapsed > 99 ? "99" : collapsed}`, bx, by);
        }

      }

      // --- labels (collision-aware selection at every zoom level) ---
      // Candidates are measured in screen space and chosen greedily by
      // priority (hover/selection always win, then path, then the hover
      // neighborhood / focus, then hubs by degree), so dense hub-spoke
      // clusters show the hub plus a handful of legible spoke labels
      // instead of overlapping soup. Hover always reveals that label.
      const screenFont = Math.max(8, Math.min(13, 11 * Math.pow(camera.k, 0.6)));
      const fontSize = screenFont / camera.k;
      ctx.font = `${fontSize}px "IBM Plex Mono", monospace`;
      ctx.textAlign = "center";
      ctx.textBaseline = "top";
      const boxes: LabelBox[] = [];
      const byId = new Map<string, { node: SimNode; label: string }>();
      for (const node of sim.nodes) {
        const isSelected = node.id === selId;
        const isHovered = hover ? node.id === hover.id : false;
        const onPath = pathSet.has(node.id);
        // While hovering, only the highlighted neighborhood competes for
        // labels (plus the selection/path, which stay visible while dimmed).
        if (highlight && !highlight.has(node.id) && !isSelected && !onPath) continue;
        const label = node.name.length > 28 ? `${node.name.slice(0, 27)}…` : node.name;
        const width = ctx.measureText(label).width * camera.k;
        const sx = (node.x - camera.x) * camera.k + rect.width / 2;
        const sy = (node.y + node.radius + 4 - camera.y) * camera.k + rect.height / 2;
        if (sx + width / 2 < 0 || sx - width / 2 > rect.width) continue;
        if (sy + screenFont < 0 || sy > rect.height) continue;
        const priority = isHovered
          ? 0
          : isSelected
            ? 1
            : onPath
              ? 2
              : (highlight && highlight.has(node.id)) || node.id === focId
                ? 3
                : 4;
        boxes.push({
          id: node.id,
          priority,
          degree: node.degree || 0,
          left: sx - width / 2,
          top: sy,
          right: sx + width / 2,
          bottom: sy + screenFont,
          sticky: isHovered || isSelected,
        });
        byId.set(node.id, { node, label });
      }
      for (const id of selectLabels(boxes, labelCapForArea(rect.width, rect.height))) {
        const entry = byId.get(id);
        if (!entry) continue;
        const { node, label } = entry;
        const onPath = pathSet.has(id);
        const inHighlight = !highlight || highlight.has(id);
        ctx.globalAlpha = inHighlight || onPath ? 1 : DIM_ALPHA;
        ctx.lineWidth = 3.5 / camera.k;
        ctx.strokeStyle = theme.halo;
        ctx.strokeText(label, node.x, node.y + node.radius + 4);
        ctx.fillStyle = theme.label;
        ctx.fillText(label, node.x, node.y + node.radius + 4);
      }
      ctx.globalAlpha = 1;
    }

    function frame() {
      rafRef.current = requestAnimationFrame(frame);
      // Idle while the panel is hidden (the shell keeps visited tabs
      // mounted); pending work resumes on the first visible frame.
      if (!canvas.offsetParent) return;
      const sim = simRef.current;
      const simActive = sim ? sim.isActive() : false;
      if (simActive) {
        sim?.tick();
        // Track the focused node while the layout is still moving.
        const followId = followIdRef.current;
        if (followId && sim) {
          const node = sim.nodes.find((n) => n.id === followId);
          if (node) {
            const camera = cameraRef.current;
            camera.x += (node.x - camera.x) * 0.18;
            camera.y += (node.y - camera.y) * 0.18;
          }
        } else if (followFitRef.current && sim) {
          // Keep the whole graph framed as it spreads out and settles.
          fitCameraToNodes(sizeRef.current, sim, cameraRef.current, true);
        }
      }
      if (simActive || needsRenderRef.current) {
        needsRenderRef.current = false;
        render();
      }
    }
    rafRef.current = requestAnimationFrame(frame);

    function onWheel(event: WheelEvent) {
      event.preventDefault();
      followFitRef.current = false;
      const camera = cameraRef.current;
      const factor = event.deltaY < 0 ? 1.12 : 1 / 1.12;
      zoomCameraAtPoint(camera, canvas.getBoundingClientRect(), { x: event.clientX, y: event.clientY }, camera.k * factor);
      needsRenderRef.current = true;
    }

    function onPointerDown(event: PointerEvent) {
      try {
        canvas.setPointerCapture(event.pointerId);
      } catch {
        /* synthetic events have no active pointer to capture */
      }
      const node = hitTest(event.clientX, event.clientY);
      if (!node) followIdRef.current = null;
      followFitRef.current = false;
      dragRef.current = {
        node,
        panning: !node,
        lastX: event.clientX,
        lastY: event.clientY,
      };
      if (node) {
        node.fx = node.x;
        node.fy = node.y;
      }
    }

    function onPointerMove(event: PointerEvent) {
      const drag = dragRef.current;
      const camera = cameraRef.current;
      if (drag.node) {
        const world = toWorld(event.clientX, event.clientY);
        drag.node.fx = world.x;
        drag.node.fy = world.y;
        simRef.current?.reheat(0.3);
        needsRenderRef.current = true;
      } else if (drag.panning) {
        camera.x -= (event.clientX - drag.lastX) / camera.k;
        camera.y -= (event.clientY - drag.lastY) / camera.k;
        drag.lastX = event.clientX;
        drag.lastY = event.clientY;
        needsRenderRef.current = true;
      } else {
        const node = hitTest(event.clientX, event.clientY);
        if (node !== hoverRef.current) {
          hoverRef.current = node;
          canvas.style.cursor = node ? "pointer" : "grab";
          needsRenderRef.current = true;
        }
      }
    }

    function onPointerUp(event: PointerEvent) {
      const drag = dragRef.current;
      const moved =
        Math.abs(event.clientX - drag.lastX) > 4 || Math.abs(event.clientY - drag.lastY) > 4;
      if (drag.node) {
        drag.node.fx = null;
        drag.node.fy = null;
        simRef.current?.reheat(0.25);
        if (!moved) propsRef.current.onSelect(drag.node.id);
      }
      dragRef.current = { node: null, panning: false, lastX: 0, lastY: 0 };
    }

    function onDoubleClick(event: MouseEvent) {
      const node = hitTest(event.clientX, event.clientY);
      if (node) propsRef.current.onExpand(node.id);
    }

    function onPointerLeave() {
      hoverRef.current = null;
      needsRenderRef.current = true;
    }

    const readSize = () => {
      const rect = canvas.getBoundingClientRect();
      sizeRef.current = { width: rect.width, height: rect.height };
    };
    readSize();
    const resizeObserver = new ResizeObserver(() => {
      readSize();
      needsRenderRef.current = true;
    });
    resizeObserver.observe(canvas);

    // Theme tokens only change when the shell flips <html data-theme>;
    // re-resolve them then instead of per frame.
    const themeObserver = new MutationObserver(() => {
      themeRef.current = null;
      needsRenderRef.current = true;
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });

    canvas.addEventListener("wheel", onWheel, { passive: false });
    canvas.addEventListener("pointerdown", onPointerDown);
    canvas.addEventListener("pointermove", onPointerMove);
    canvas.addEventListener("pointerup", onPointerUp);
    canvas.addEventListener("pointerleave", onPointerLeave);
    canvas.addEventListener("dblclick", onDoubleClick);

    return () => {
      cancelAnimationFrame(rafRef.current);
      resizeObserver.disconnect();
      themeObserver.disconnect();
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("pointerdown", onPointerDown);
      canvas.removeEventListener("pointermove", onPointerMove);
      canvas.removeEventListener("pointerup", onPointerUp);
      canvas.removeEventListener("pointerleave", onPointerLeave);
      canvas.removeEventListener("dblclick", onDoubleClick);
    };
  }, []);

  return <canvas ref={canvasRef} className="tsg-canvas" aria-label="Code graph canvas" />;
}
