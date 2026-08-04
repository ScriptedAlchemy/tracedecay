import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { ActivationField } from './activation.ts';
import { isMeasuredField } from './layout.ts';
import { hasWebGl, watchWebGlContext } from './renderer.ts';
import {
  buildEmergentScene,
  buildMeasuredScene,
  type GraphScene,
  type SceneRequest,
} from './scene.ts';
import {
  DEFAULT_ENCODING,
  type FieldExtent,
  type GraphCanvasEdge,
  type GraphCanvasEncoding,
  type GraphCanvasNode,
} from './types.ts';
import { useReducedMotion } from '../trace/reducedMotion.ts';
import { EvidencePattern } from '../../ui/EvidencePattern';
import { cn } from '../../ui/cn';

export type { GraphCanvasEdge, GraphCanvasEncoding, GraphCanvasNode } from './types.ts';

/** Sigma over Graphology (plan 11a: default connected-graph renderer).
 *
 * Deterministic ForceAtlas2 settle (laid out once, never animated), nodes
 * sized by degree and lit by their real vitality, relations drawn as curved
 * connective tissue rather than chords. Everything that moves is a response to
 * a real event: an activation strike from the live stream, a search that hit,
 * or the pointer. At rest the field is completely still and the render loop is
 * asleep. The synchronized list next to the canvas remains the accessible
 * surface.
 *
 * This component owns the React side only — the props, the container's
 * measured box, and when a scene may exist. Preparing the layout, drawing it
 * and animating it live in `layout.ts`, `renderer.ts` and
 * `activationOverlay.ts`; `scene.ts` composes the three into one thing with
 * one lifetime. */
export function GraphCanvas({
  nodes,
  edges,
  selectedId,
  onSelect,
  height = 320,
  fill = false,
  activation,
  canvasClassName,
  caption,
  encoding = DEFAULT_ENCODING,
  ariaLabel,
  fallbackDescription,
  extent,
}: {
  nodes: GraphCanvasNode[];
  edges: GraphCanvasEdge[];
  selectedId?: string | null;
  onSelect?: (id: string | null) => void;
  height?: number;
  /** Occupy the parent's full height instead of a fixed one. The parent must
   * establish the height (e.g. `flex-1 min-h-0`). */
  fill?: boolean;
  /** External synapse field; when omitted the canvas owns a local one fed by
   * selection strikes. */
  activation?: ActivationField;
  /** Extra classes merged onto the canvas element itself (not the figure) --
   * for a caller that needs to guarantee a minimum rendered height on a
   * breakpoint where its own flex ancestors would otherwise squeeze a `fill`
   * canvas toward zero. */
  canvasClassName?: string;
  /** What this particular field means. The default sentence describes a
   * force-laid symbol graph; any caller composing a different field MUST
   * replace it, because the caption is the only place the reader is told what
   * position, size and brightness encode — leaving the default on a measured
   * layout would state something untrue about the picture. */
  caption?: ReactNode;
  /** Compact visible key for the canvas's four visual channels. Callers with
   * measured placement or mass must name those meanings explicitly. */
  encoding?: GraphCanvasEncoding;
  /** Accessible description of the canvas, for the same reason. */
  ariaLabel?: string;
  /** The caller's actual non-canvas continuation. Different graph views expose
   * different text alternatives, so a fallback cannot truthfully name one
   * generic "symbol list". */
  fallbackDescription?: string;
  /** The frame a measured field is drawn in, in the caller's own coordinates.
   * Only meaningful alongside placed nodes. Without it the camera frames the
   * bodies that happen to exist, so a field with an empty region — no dormant
   * projects, say — silently loses that region and the reader is never shown
   * the absence. With it, an empty part of the axis stays empty on screen,
   * which is the finding. */
  extent?: FieldExtent;
}) {
  const unknownDegreeCount = nodes.filter((node) => node.degree == null).length;
  const containerRef = useRef<HTMLDivElement | null>(null);
  /** The live scene, or nothing. Every out-of-band poke at the renderer —
   * a selection repaint, a resize, a theme flip, a strike — goes through this,
   * so none of them can reach a renderer that has already been killed. */
  const sceneRef = useRef<GraphScene | null>(null);
  /**
   * Tears the live scene down. Held in a ref so the size observer can call
   * it synchronously, ahead of any frame Sigma has scheduled for itself.
   */
  const teardownRef = useRef<(() => void) | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  /**
   * The container's last measured box.
   *
   * Sigma's render path calls `resize()`, which THROWS on a zero-width
   * container, and one of the callers of that path is a `window` resize
   * listener installed inside Sigma that no guard on our side can reach. So
   * the renderer's lifetime is bound to a real measured box rather than
   * merely started once one appears: it is built when the container has been
   * measured non-zero, and torn down the moment the measurement says it has
   * none. That is why this is observed state and not a mount-time retry — a
   * retry answers "has it arrived yet", and the error we were getting came
   * from the other direction, a container that had a box and then lost it as
   * its workspace was navigated away from.
   */
  const [box, setBox] = useState<{ width: number; height: number }>({ width: 0, height: 0 });
  /**
   * Whether the container has a box at all — the only distinction the renderer's
   * lifetime turns on.
   *
   * Sigma reads `offsetWidth` in `resize()` and throws on a 0×0 or detached
   * container, so a renderer may exist exactly while this is true. How large the
   * box is does not affect that, which is why the mount effect depends on this
   * boolean and the dimensions drive `resize()` instead.
   */
  const hasBox = box.width > 0 && box.height > 0;
  /** Bumped whenever a collapse killed a live renderer, so the mount effect can
   * rebuild even when the box it measures never appeared to change. */
  const [teardownGeneration, setTeardownGeneration] = useState(0);
  /**
   * The topology whose layout engine failed to load, if one did.
   *
   * An emergent field fetches ForceAtlas2 on demand, so for the first time
   * this canvas has a way to fail that is neither "no context" nor "too
   * large". Drawing the seed circle instead would be a lie — a ring of nodes
   * is a composition, and the reader would read meaning into it — so the
   * failure is stated. Held as the topology it happened to rather than a
   * boolean, so a caller handing over different nodes gets a fresh attempt
   * without any reset of its own.
   */
  const [engineFailedFor, setEngineFailedFor] = useState<readonly GraphCanvasNode[] | null>(
    null,
  );
  /**
   * The topology whose GPU context was lost after it had been drawn, if one
   * was.
   *
   * The only failure on this canvas that arrives after a successful frame, and
   * the one with no symptom of its own: a lost context leaves the last drawn
   * pixels frozen, or clears them to nothing, and either reading is false. So
   * it is stated, and held as the topology it happened to for the same reason
   * the engine failure is — different nodes are a different attempt.
   */
  const [contextLostFor, setContextLostFor] = useState<readonly GraphCanvasNode[] | null>(
    null,
  );
  /**
   * Releases the watch on the WebGL layers of the renderer that is, or was,
   * live.
   *
   * Held for as long as this canvas is mounted rather than for one scene's
   * lifetime: a lost context has to take its renderer down with it, and the
   * restore that brings the field back is dispatched at the canvas of the
   * renderer that died. A watch released with the scene could report the loss
   * but never the recovery.
   */
  const contextWatchRef = useRef<(() => void) | null>(null);

  /**
   * Attach the observer as the container mounts rather than in an effect: an
   * effect would need the container in its own dependency list to notice it
   * appearing, and the element is behind three early returns.
   */
  const attachContainer = useCallback((node: HTMLDivElement | null) => {
    containerRef.current = node;
    resizeObserverRef.current?.disconnect();
    resizeObserverRef.current = null;
    if (!node) {
      setBox({ width: 0, height: 0 });
      return;
    }
    const measure = (): void => {
      // `offsetWidth`, matching what Sigma itself reads in `resize()`. A
      // display:none ancestor and a detached node both report 0 here, which
      // are exactly the two states that make Sigma throw.
      const width = node.offsetWidth;
      const height = node.offsetHeight;
      if (width === 0 || height === 0) {
        // Synchronous, before React re-renders: a scheduled Sigma frame would
        // otherwise reach `resize()` first and throw. Killing here also
        // removes Sigma's own window-resize listener.
        const teardown = teardownRef.current;
        teardown?.();
        // This teardown is imperative, so the mount effect cannot infer it from
        // the box alone: a collapse and a re-expansion that land in one commit
        // leave the measured box non-zero at both ends, and the effect would
        // see no change to react to and never rebuild the renderer it no longer
        // has. The generation makes the teardown itself observable.
        if (teardown) setTeardownGeneration((generation) => generation + 1);
      }
      setBox((previous) =>
        previous.width === width && previous.height === height
          ? previous
          : { width, height },
      );
    };
    measure();
    // Same guard the other observing surfaces use. Without a ResizeObserver
    // the one measurement above still lets a sized container mount; what is
    // lost is the teardown on collapse, which is the honest degradation.
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    resizeObserverRef.current = observer;
  }, []);
  const webglRef = useRef<boolean | null>(null);
  if (webglRef.current === null) webglRef.current = hasWebGl();
  const fieldRef = useRef<ActivationField | null>(null);
  if (activation) fieldRef.current = activation;
  else if (!fieldRef.current) fieldRef.current = new ActivationField();
  const field = fieldRef.current;
  // Selection and the select handler are read through refs rather than closed
  // over, so they can change without re-running the mount effect. They used to
  // sit in its dependency list, and `onSelect` is an inline arrow at every call
  // site: every parent render — including one per live SSE pulse — tore the
  // renderer down and re-ran a 200-iteration ForceAtlas2 layout. That both
  // burned a layout per event and hid the sleeping render loop behind a
  // remount. The effect now depends on topology alone.
  const selectedIdRef = useRef<string | null | undefined>(selectedId);
  selectedIdRef.current = selectedId;
  const onSelectRef = useRef<((id: string | null) => void) | undefined>(onSelect);
  onSelectRef.current = onSelect;
  // The app's persisted three-state motion control, not the bare OS query this
  // used to read: pinning "Reduced" had no effect on the field, which is the one
  // surface in the product where motion is actually the point. Held in a ref for
  // the same reason selection is — the renderer costs a 200-iteration
  // ForceAtlas2 layout to build, so a preference flip must reach the live render
  // loop without tearing the field down and re-laying it out.
  const { reduced } = useReducedMotion();
  const reducedRef = useRef(reduced);
  reducedRef.current = reduced;

  // Selection is a static repaint, not an animation: recolour once and leave
  // the loop asleep. `sceneRef` is cleared the moment the container loses its
  // box, so this cannot repaint into a renderer that has nothing to measure.
  useEffect(() => {
    sceneRef.current?.repaint();
  }, [selectedId]);

  // A caller-owned field is struck from entirely outside this component: the
  // Brain's SSE effect calls `field.strike(...)` when a real event lands, with
  // no knowledge of any render loop. If the loop is asleep (which, correctly,
  // it is whenever the field is cold) that heat would sit undrawn and
  // undecayed forever. Subscribing turns every real strike, wherever it
  // originates, into exactly one wake — and nothing else can produce one,
  // because the field has no clock. The subscription belongs to the FIELD, not
  // to any one scene: a strike that arrives while no scene exists finds
  // nothing to wake, which is the same nothing the unsubscribed version did.
  useEffect(() => field.subscribe(() => sceneRef.current?.wake()), [field]);

  // A theme flip is a property of the document, not of any one renderer, so
  // the observer lives as long as this canvas does and re-samples whichever
  // scene is live at the time — or nothing, if none is.
  useEffect(() => {
    const observer = new MutationObserver(() => sceneRef.current?.retheme());
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    return () => observer.disconnect();
  }, []);

  // The context watch is the one piece of wiring that outlives the scene it was
  // armed for, so releasing it is this canvas's own last act. The effect below
  // cannot: it has already unwound, along with the container it owned, by the
  // time a restore can arrive.
  useEffect(() => () => contextWatchRef.current?.(), []);

  // The scene's own lifetime. The dependency list is deliberately exactly the
  // set that invalidates a composed field: its topology, the axis it is framed
  // in, whether the container has a box at all, and any teardown that happened
  // out of band. Everything else the scene needs — selection, the select
  // handler, the motion preference, the activation field — is read through a
  // ref precisely so it can change without costing a layout.
  useEffect(() => {
    const container = containerRef.current;
    if (!container || nodes.length === 0 || !webglRef.current) return;
    // Not a retry: `hasBox` is derived from the observed measurement, so this
    // effect re-runs by itself once the container has one, and unwinds again if
    // it loses it.
    if (!hasBox) return;

    let cancelled = false;
    let detach: (() => void) | null = null;
    const request: SceneRequest = {
      container,
      nodes,
      edges,
      extent,
      // Read through the ref for the same reason selection is: swapping the
      // field a caller owns must not cost a teardown and a fresh layout.
      field: fieldRef.current ?? field,
      selectedId: () => selectedIdRef.current,
      onSelect: (id) => onSelectRef.current?.(id),
      isReduced: () => reducedRef.current,
    };
    const install = (scene: GraphScene): void => {
      sceneRef.current = scene;
      detach = () => {
        scene.teardown();
        if (sceneRef.current === scene) sceneRef.current = null;
        if (teardownRef.current === detach) teardownRef.current = null;
      };
      teardownRef.current = detach;
      // Sigma keeps no watch of its own, so a GPU context dropped after this
      // field was drawn would leave a frozen or blank canvas on screen while
      // everything around it went on presenting a drawn graph.
      contextWatchRef.current?.();
      contextWatchRef.current = watchWebGlContext(scene.webGlCanvases, {
        onLost: () => {
          if (cancelled) return;
          // Synchronously, ahead of any frame the overlay has already asked
          // for: the renderer died with its context, and this is the same
          // one-way latch the size observer tears a live scene down through,
          // so the animation loop stops and the pointer wiring goes with it.
          detach?.();
          setContextLostFor(nodes);
        },
        onRestored: () => {
          // Deliberately NOT gated on `cancelled`: by the time a restore lands
          // this effect has unwound along with the container it owned, which is
          // precisely why nothing is installed from here. Clearing the state
          // re-renders the container and the generation makes the rebuild
          // observable to the effect, which then owns whatever it measures.
          setContextLostFor(null);
          setTeardownGeneration((generation) => generation + 1);
        },
      });
    };

    if (isMeasuredField(nodes)) {
      // Synchronous, and reaching no layout engine at all: the coordinates are
      // already the caller's measurement.
      install(buildMeasuredScene(request));
    } else {
      // Fetches a layout engine before it can compose, so the cleanup below
      // has to be able to reach a build that is still in flight: cancelling
      // drops the resolved module instead of installing a scene into a
      // container this effect no longer owns.
      void buildEmergentScene(request, () => cancelled).then(
        (scene) => {
          if (scene) install(scene);
        },
        () => {
          if (!cancelled) setEngineFailedFor(nodes);
        },
      );
    }

    return () => {
      cancelled = true;
      detach?.();
    };
  }, [nodes, edges, extent, hasBox, teardownGeneration]);

  /**
   * A resize of a container that still has a box is a resize, not a remount.
   *
   * The renderer's lifetime is bound to `hasBox` above rather than to the
   * measured numbers, because depending on the numbers made every drag of a
   * window edge or opening of a side panel kill the renderer, rebuild the whole
   * graphology graph and re-run the 200-iteration ForceAtlas2 settle — a
   * layout per resize frame. Only the zero/non-zero transition changes what
   * Sigma can legally do; every other change is something Sigma resizes itself
   * into. `resize()` is also what Sigma's own window listener would call.
   */
  useEffect(() => {
    if (!hasBox) return;
    sceneRef.current?.resize();
  }, [hasBox, box.width, box.height]);

  // Turning motion off has to take effect on the field the reader is looking at,
  // not merely on the next one they open: a loop already running keeps running
  // until something stops it. Turning it back on needs no counterpart — the next
  // real event wakes the loop through `wake`.
  useEffect(() => {
    if (reduced) sceneRef.current?.settle();
  }, [reduced]);

  if (nodes.length === 0) {
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        no graph neighborhood to draw
      </p>
    );
  }
  // Sigma is WebGL-only and throws during construction without a context,
  // which React Router's error boundary turns into a dead workspace. Browsers
  // with WebGL disabled or blocklisted get the truthful state instead.
  if (!webglRef.current) {
    return (
      <GraphUnavailable>
        this browser has no WebGL context, so the {nodes.length.toLocaleString()}-symbol
        graph canvas cannot draw — {fallbackDescription ?? 'read the field description below'}
      </GraphUnavailable>
    );
  }
  // Scale tier guard (plan 11a graph tiers): this Sigma canvas owns graphs up
  // to ~5k nodes. Larger brains (the profile holds stores up to 1.6M nodes)
  // belong to the GPU tier — render the truthful tier state, never a frozen
  // tab pretending to cope.
  if (nodes.length > 5_000) {
    return (
      <GraphUnavailable>
        {nodes.length.toLocaleString()} symbols exceeds this renderer's tier —
        the GPU canvas (cosmos.gl adapter) owns brains this large; narrow the
        neighborhood to explore here
      </GraphUnavailable>
    );
  }
  if (engineFailedFor === nodes) {
    return (
      <GraphUnavailable>
        the force layout could not be loaded, so the{' '}
        {nodes.length.toLocaleString()}-symbol graph canvas has no positions to
        draw — {fallbackDescription ?? 'read the field description below'}
      </GraphUnavailable>
    );
  }
  // The one failure that happens to a field that WAS drawn. The frozen last
  // frame is the trap: it is a picture of a graph, so it reads as a live one,
  // and a cleared context reads as an empty one. Neither is a reading, so the
  // canvas is taken down and this is said in its place.
  if (contextLostFor === nodes) {
    return (
      <GraphUnavailable>
        the graph canvas lost its WebGL context, so the{' '}
        {nodes.length.toLocaleString()}-symbol field is no longer being drawn —
        {fallbackDescription ?? 'read the field description below'}, and the field
        returns if the browser restores the context
      </GraphUnavailable>
    );
  }
  return (
    <figure className={cn('flex flex-col gap-1.5', fill && 'h-full min-h-0')}>
      <div
        ref={attachContainer}
        style={fill ? undefined : { height }}
        className={cn(
          'relative overflow-hidden rounded-[var(--radius-card)] border border-edge-subtle/60',
          // Three composed layers, none of which draws an entity: the nebula
          // field belongs to the network, the grain denies it a perfectly even
          // surface, and the bezel screen ruling belongs to the chassis.
          // Together the canvas reads as a lit instrument screen rather than a
          // picture pasted onto a panel.
          'td-graph-field td-grain td-scanlines',
          // The aperture's own depth, now a design-system token rather than an
          // arbitrary value spelled out here.
          'shadow-[var(--shadow-field)]',
          fill && 'min-h-0 flex-1',
          canvasClassName,
        )}
        role="img"
        aria-label={
          ariaLabel ??
          `Code graph: ${nodes.length} symbols, ${edges.length} relations. The symbol list alongside is the accessible equivalent.`
        }
      />
      <figcaption className="flex flex-col gap-1.5 text-2xs text-text-muted">
        <GraphEncodingKey encoding={encoding} />
        {unknownDegreeCount > 0 ? (
          // Provenance is carried by the shared evidence PATTERN axis, not by
          // prose alone: the dashed `unknown` swatch says "this quantity was
          // never measured" in the same visual language the rest of the app
          // uses, and it survives monochrome and forced-colors. The sentence
          // stays, because the pattern says which class of evidence this is and
          // only the sentence says what was missing.
          <p
            data-state="partial"
            className="flex flex-wrap items-center gap-x-2 gap-y-0.5 leading-relaxed text-text-muted"
          >
            <EvidencePattern quality="unknown" />
            <span className="text-3xs">
              Connectedness is absent for {unknownDegreeCount}{' '}
              {unknownDegreeCount === 1 ? 'symbol' : 'symbols'}; each uses the
              minimum marker, not zero.
            </span>
          </p>
        ) : null}
        <div>
          {caption ?? (
            <>
              {nodes.length} symbols · {edges.length} relations · hover isolates
              a neighbourhood · click fires it and the glow decays with the
              activation
            </>
          )}
        </div>
      </figcaption>
    </figure>
  );
}

/** A field that is NOT being drawn — no WebGL context, no layout engine, a
 * graph past this renderer's tier, or a context the GPU took back after the
 * field had been composed.
 *
 * Distinct from an empty field on purpose, and the distinction has to be visible
 * rather than only readable: "nothing is here" and "this could not be rendered"
 * are different claims, and a quiet line of muted prose reads as the first.
 * Wearing the dashed `unknown` evidence pattern states in the app's own visual
 * language that no measurement backs this region, so it can never be mistaken
 * for a drawn graph that happens to be sparse. Deliberately NOT given the
 * atmospheric graph field: the aperture treatment is what a rendered field
 * looks like, and lending it to a failure is exactly the kind of beautiful
 * smoothing-over that would make a failure look like data. */
function GraphUnavailable({ children }: { children: ReactNode }) {
  return (
    <div
      data-state="unavailable"
      role="status"
      aria-live="polite"
      className="flex flex-col items-center gap-2 border-y border-dashed border-edge-strong bg-surface-1 p-6 text-center"
    >
      <span
        aria-hidden
        className="h-1 w-full max-w-40 opacity-70"
        style={{ backgroundImage: 'var(--ev-unknown)' }}
      />
      <p className="text-sm text-text-secondary">{children}</p>
      <EvidencePattern quality="unknown" />
    </div>
  );
}

function GraphEncodingKey({ encoding }: { encoding: GraphCanvasEncoding }) {
  const items = [
    { label: 'disc', value: encoding.body },
    { label: 'size', value: encoding.size },
    { label: 'hue', value: encoding.hue },
    { label: 'glow', value: encoding.signal },
    { label: 'line', value: encoding.relation },
  ];
  return (
    <div
      aria-label="Graph visual key"
      className="grid grid-cols-3 items-start gap-x-3 gap-y-1 border-y border-edge-subtle/70 py-1 sm:flex sm:flex-wrap sm:items-center sm:gap-x-4"
    >
      {items.map((item, index) => (
        <span
          key={item.label}
          className="flex min-w-0 flex-col gap-0.5 sm:inline-flex sm:flex-row sm:items-center sm:gap-1.5"
        >
          <span className="inline-flex items-center gap-1.5">
            {index === 0 ? (
              <span
                aria-hidden
                className="size-2 rounded-full border border-accent/70 bg-accent/25 shadow-[0_0_7px_var(--raw-accent)]"
              />
            ) : null}
            <span className="td-legend">{item.label}</span>
          </span>
          <span aria-hidden className="hidden text-text-muted sm:inline">
            ·
          </span>
          <span className="td-value min-w-0 text-3xs leading-tight text-text-secondary">
            {item.value}
          </span>
        </span>
      ))}
    </div>
  );
}
