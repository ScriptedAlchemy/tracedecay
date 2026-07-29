import { cssColorToRgb, type ActivationField } from './activation.ts';
import { createActivationOverlay } from './activationOverlay.ts';
import {
  frameEmergentField,
  loadForceAtlas2,
  settleEmergentField,
} from './emergentField.ts';
import { kindColor } from './kindColor.ts';
import { buildDendrites, prepareField, type FieldFrame, type PreparedField } from './layout.ts';
import { frameMeasuredField } from './measuredField.ts';
import { palette, type ThemeBox } from './palette.ts';
import { createFieldRenderer, createFocusState } from './renderer.ts';
import type { FieldExtent, GraphCanvasEdge, GraphCanvasNode } from './types.ts';

/**
 * A live field: one prepared layout, one Sigma renderer, one activation
 * overlay, and the single latch that kills all three together.
 *
 * The two builders below are the field's two paths, and they differ in exactly
 * one thing: whether the coordinates were measured by the caller or have to be
 * discovered. A measured field is composed synchronously and never reaches the
 * layout engine at all; an emergent one waits for that engine before a single
 * pixel is drawn, so the reader never sees the seed circle it starts from.
 */
export interface GraphScene {
  /** Repaint the current composition. Static: no loop is started. */
  repaint(): void;
  resize(): void;
  settle(): void;
  wake(): void;
  retheme(): void;
  /** Idempotent: React's cleanup and the size observer both call it, and on
   * an ordinary unmount they both fire. */
  teardown(): void;
}

export interface SceneRequest {
  container: HTMLElement;
  nodes: readonly GraphCanvasNode[];
  edges: readonly GraphCanvasEdge[];
  extent: FieldExtent | undefined;
  field: ActivationField;
  selectedId: () => string | null | undefined;
  onSelect: (id: string | null) => void;
  isReduced: () => boolean;
}

/** Build the scene for a field whose coordinates are the caller's own
 * measurement. Synchronous end to end — there is no layout to wait for. */
export function buildMeasuredScene(request: SceneRequest): GraphScene {
  const { theme, prepared } = prepare(request);
  return compose(request, theme, prepared, frameMeasuredField(prepared, request.extent));
}

/**
 * Build the scene for a field whose shape is the finding.
 *
 * The graph is constructed first, then the layout engine is loaded, and only
 * then is anything drawn: Sigma is not constructed until the coordinates are
 * final, so there is no frame in which the seed circle is on screen. Resolves
 * to `null` when the caller cancelled while the engine was in flight — the
 * resolved module is dropped rather than handed to a component that is gone.
 */
export async function buildEmergentScene(
  request: SceneRequest,
  cancelled: () => boolean,
): Promise<GraphScene | null> {
  const { theme, prepared } = prepare(request);
  const forceAtlas2 = await loadForceAtlas2();
  if (cancelled()) return null;
  settleEmergentField(prepared, forceAtlas2);
  return compose(request, theme, prepared, frameEmergentField(prepared));
}

function prepare(request: SceneRequest): { theme: ThemeBox; prepared: PreparedField } {
  const { container, nodes, edges } = request;
  const theme: ThemeBox = { colors: palette(container) };
  const prepared = prepareField({
    nodes,
    edges,
    viewport: { width: container.clientWidth, height: container.clientHeight },
    kindRgb: (kind) => cssColorToRgb(kindColor(kind, theme.colors.light)),
  });
  return { theme, prepared };
}

function compose(
  request: SceneRequest,
  theme: ThemeBox,
  prepared: PreparedField,
  frame: FieldFrame,
): GraphScene {
  const { container, edges, field, isReduced, onSelect, selectedId } = request;
  const { graph, realNodes, neighborsOf, nodeCount, denseField, roominess } = prepared;
  const strands = buildDendrites(graph, edges.length);
  const focus = createFocusState();

  /**
   * One-way latch guarding every repaint below. Once the container has lost
   * its box there is no such thing as a correct frame, so the loop is not
   * slowed or deferred — it stops, and `paint` becomes a no-op for whatever
   * is still holding a closure over this renderer.
   */
  let alive = true;
  let renderer: ReturnType<typeof createFieldRenderer> | null = null;
  const paint = (): void => {
    if (alive) renderer?.refresh();
  };

  const overlay = createActivationOverlay({
    graph,
    realNodes,
    strands,
    field,
    neighborsOf,
    theme,
    focus,
    paint,
    isReduced,
  });

  renderer = createFieldRenderer({
    graph,
    container,
    theme,
    focus,
    field,
    neighborsOf,
    nodeCount,
    denseField,
    roominess,
    frame,
    selectedId,
    onNodeClick: (node) => {
      onSelect(node);
      overlay.fireNeighborhood(node);
    },
    onStageClick: () => onSelect(null),
    onFocusChange: () => overlay.wake(),
  });

  // One static composition of the resting field, so the graph is fully
  // rendered before anything ever fires.
  overlay.repaintResting();
  // Heat that landed while the layout engine was loading is still real; the
  // field has no clock of its own, so nothing else would ever draw or decay it.
  if (field.warm) overlay.wake();

  return {
    repaint: paint,
    resize: () => {
      if (alive) renderer?.resize();
    },
    settle: () => {
      if (alive) overlay.settle();
    },
    wake: () => {
      if (alive) overlay.wake();
    },
    retheme: () => {
      if (!alive) return;
      renderer?.retheme();
      overlay.repaintResting();
    },
    teardown: () => {
      if (!alive) return;
      alive = false;
      overlay.stop();
      renderer?.kill();
    },
  };
}
