/**
 * TRACE — the Code workspace's call-topography drill-in (plan 11b, "Topography
 * round one — coordinator verdict", "Sensory contract", "Rendering strategy").
 *
 * A selected symbol floods the field: its callers converge from above as
 * tributaries, its callees fan below as a delta, every channel as wide as the
 * call sites on that one edge, and the whole thing under real spring physics so
 * the structure can be FELT — hubs are slow and deep, leaves flick, and dragging
 * a symbol deforms its neighbourhood in proportion to how tightly it is called.
 *
 * The division of labour is the plan's honesty boundary and is not negotiable:
 *
 *   `viz/trace/model.ts`   wire payload → positions and counts. Every figure
 *                          printed on this surface comes from there.
 *   `viz/trace/sim.ts`     positions → forces. Every felt quantity comes from
 *                          there, computed from a stated measurement.
 *   `viz/trace/render.ts`  draws. Decides nothing.
 *
 * This file composes the three, fetches the data, and is responsible for one
 * thing of its own: saying out loud what the picture is and is not showing.
 *
 * DEPTH. `GET /api/plugins/graph/node/{id}/neighbors` serves one hop. Hop 2 is
 * assembled here by fetching the drawn hop-1 neighbours' own neighbourhoods,
 * bounded by `TRACE_BUDGET.expand`, deduped in the model. Whatever that leaves
 * out is counted and printed — see `coverageCaption`.
 *
 * ACCESSIBILITY. The canvas is one `role="img"` with a description that carries
 * the same claims as the caption, and the ranked list below is its exact
 * equivalent: every symbol on the field is in it, in call-site order, as
 * keyboard-reachable text. Reduced motion is a rendering MODE — the identical
 * `step()` sequence run to rest and painted once, with tension drawn as
 * thickness — not a switched-off feature, and it can be pinned on or off from
 * the surface regardless of the OS setting.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ArrowLeft, Crosshair, Gauge } from 'lucide-react';
import { useQueries } from '@tanstack/react-query';

import { CenteredState, LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { cn } from '../../ui/cn';
import { elideStart } from '../../ui/format.ts';
import { fetchLegacy, type LegacyResult } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import {
  TRACE_BUDGET,
  TRACE_ENCODINGS,
  buildSimSpec,
  buildTraceModel,
  coverageCaption,
  fieldDescription,
  type NeighborsPayload,
} from '../../viz/trace/model.ts';
import { createRenderer, type TraceRenderer } from '../../viz/trace/render.ts';
import { bloomStep, createSimulation, type Simulation } from '../../viz/trace/sim.ts';
import { resolveTracePalette } from '../../viz/trace/palette.ts';
import { useReducedMotion, type MotionPreference } from '../../viz/trace/reducedMotion.ts';
import type { TraceModel, TraceNode } from '../../viz/trace/types.ts';
import { GraphNeighborsPayloadSchema, type GraphNode } from './contracts.ts';

const BASE = '/api/plugins/graph';
/** The endpoint's own hard cap (`coerce_limit(params.limit, 50, 200)`). */
const NEIGHBOR_LIMIT = 200;
/** One fixed step per frame. Wall-clock jitter never reaches the integrator. */
const DT = 1 / 60;
/** Narrowest column the field can be read in. Below it, the list stands alone. */
const MIN_FIELD_WIDTH = 460;

function neighborsUrl(id: string): string {
  return `${BASE}/node/${encodeURIComponent(id)}/neighbors?limit=${NEIGHBOR_LIMIT}`;
}

function displayName(node: {
  name?: string | null;
  qualified_name?: string | null;
  id: string;
}): string {
  return node.name ?? node.qualified_name ?? node.id;
}

/* ---- data --------------------------------------------------------------- */

/**
 * The focus's neighbourhood plus, for as many of its drawn neighbours as the
 * budget allows, theirs. Expansion is a second wave of independent reads, so a
 * neighbour that fails leaves a hole that gets COUNTED rather than one that
 * takes the surface down.
 */
function useTraceNeighborhood(focusId: string) {
  const scope = useScope((s) => s.scope);
  const root = useLegacy(
    ['graph', 'neighbors', focusId],
    neighborsUrl(focusId),
    GraphNeighborsPayloadSchema,
  );

  const hop1 = useMemo(() => {
    if (root.data?.outcome !== 'ok') return [] as string[];
    const seen = new Set<string>();
    for (const row of [...(root.data.data.callers ?? []), ...(root.data.data.callees ?? [])]) {
      if (row.id !== focusId) seen.add(row.id);
    }
    // Ordered by first appearance, which is the endpoint's own
    // `ORDER BY n.qualified_name` — stable across reloads, so the same
    // neighbourhood expands the same way twice.
    return [...seen].slice(0, TRACE_BUDGET.expand);
  }, [root.data, focusId]);

  const expansions = useQueries({
    queries: hop1.map((id) => ({
      queryKey: ['graph', 'neighbors', id, scopeKey(scope)],
      queryFn: () =>
        fetchLegacy(scopedUrl(scope, neighborsUrl(id)), GraphNeighborsPayloadSchema),
      staleTime: 60_000,
    })),
  });

  // `useQueries` returns a fresh array every render, so memoising on it would
  // rebuild the model — and therefore tear down and re-seed the simulation —
  // sixty times a second. The identity that actually matters is which ids have
  // settled, which is exactly what this signature carries.
  const signature = hop1.map((id, i) => `${id}:${expansions[i]?.status ?? 'idle'}`).join('|');
  const expanded = useMemo(() => {
    const out = new Map<string, NeighborsPayload>();
    hop1.forEach((id, i) => {
      const result = expansions[i]?.data as LegacyResult<NeighborsPayload> | undefined;
      if (result?.outcome === 'ok') out.set(id, result.data);
    });
    return out;

  }, [signature]);

  return { root, expanded, expanding: expansions.some((q) => q.isPending) };
}

/* ---- the surface -------------------------------------------------------- */

export function TraceView({
  focus,
  onClose,
  onFocusChange,
}: {
  focus: GraphNode;
  onClose: () => void;
  /** Re-flood the field on another symbol, from the list below. */
  onFocusChange?: (node: GraphNode) => void;
}) {
  const { root, expanded, expanding } = useTraceNeighborhood(focus.id);

  // Escape returns to the spine. Bound on the document because the pointer is
  // usually over the canvas, which is not a focusable control.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.stopPropagation();
        onClose();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <button
          type="button"
          onClick={onClose}
          className="flex shrink-0 items-center gap-1 text-2xs text-text-muted hover:text-text-primary focus-visible:text-text-primary"
        >
          <ArrowLeft aria-hidden size={12} />
          Back to spine
        </button>
        <span aria-hidden className="td-rule" />
        <h2 className="td-title min-w-0 truncate">
          <span className="text-text-muted">trace · </span>
          {displayName(focus)}
        </h2>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        <LegacyBoundary title="Trace" pending={root.isPending} result={root.data}>
          {(payload) => {
            const callers = payload.callers ?? [];
            const callees = payload.callees ?? [];
            if (callers.length === 0 && callees.length === 0) {
              return (
                <div className="flex flex-col gap-2 p-6">
                  <CenteredState title="Call-edge result is unverified" kind="partial" />
                  <p className="mx-auto max-w-md text-center text-xs leading-relaxed text-text-muted">
                    The legacy graph response returned no <code className="font-mono">calls</code>{' '}
                    rows for {displayName(focus)}, but it carries no read-health field. The
                    frontend cannot distinguish a successful empty result from a query failure.
                  </p>
                </div>
              );
            }
            return (
              <TraceField
                focus={focus}
                root={payload}
                expanded={expanded}
                expanding={expanding}
                {...(onFocusChange ? { onFocusChange } : {})}
              />
            );
          }}
        </LegacyBoundary>
      </div>
    </div>
  );
}

/* ---- the field ---------------------------------------------------------- */

function TraceField({
  focus,
  root,
  expanded,
  expanding,
  onFocusChange,
}: {
  focus: GraphNode;
  root: NeighborsPayload;
  expanded: ReadonlyMap<string, NeighborsPayload>;
  expanding: boolean;
  onFocusChange?: (node: GraphNode) => void;
}) {
  const model = useMemo(
    () =>
      buildTraceModel({
        focus: {
          id: focus.id,
          kind: focus.kind,
          name: focus.name ?? null,
          qualified_name: focus.qualified_name ?? null,
          file_path: focus.file_path ?? null,
          start_line: focus.start_line ?? null,
          degree: focus.degree ?? null,
        },
        root,
        expanded,
      }),
    [focus, root, expanded],
  );

  const { reduced, preference, setPreference } = useReducedMotion();
  const [hovered, setHovered] = useState<string | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);

  return (
    <div className="flex flex-col">
      <figure className="flex flex-col gap-1.5 border-b border-edge-subtle px-3 pb-2 pt-2">
        <TraceCanvas
          model={model}
          reduced={reduced}
          onHoverChange={setHovered}
          onDragChange={setDragging}
        />
        <figcaption className="flex flex-col gap-1 leading-tight">
          <span className="text-3xs text-text-muted">{TRACE_ENCODINGS}</span>
          {/* The coverage sentence is generated from the same counts the field
           * was built from, so the caption and the picture cannot drift. */}
          <span className="text-3xs text-text-secondary">{coverageCaption(model)}</span>
          {expanding ? (
            <span className="text-3xs text-state-loading">
              still expanding hop 2 — the counts above are for what has arrived so far
            </span>
          ) : null}
        </figcaption>
      </figure>

      <div className="flex flex-wrap items-center gap-2.5 border-b border-edge-subtle px-3 py-1.5">
        <span className="td-legend shrink-0">motion</span>
        <MotionToggle preference={preference} reduced={reduced} onChange={setPreference} />
        <span aria-hidden className="td-rule" />
        <span className="td-legend shrink-0 normal-case tracking-normal text-text-muted">
          {reduced
            ? 'settled once; tension drawn as rail thickness'
            : hovered || dragging
              ? `${dragging ? 'dragging' : 'hovering'} ${
                  model.nodes.find((n) => n.id === (dragging ?? hovered))?.name ?? '—'
                }`
              : 'hover a symbol to feel its weight, drag it to deform its neighbourhood'}
        </span>
      </div>

      <TraceList model={model} focusId={focus.id} {...(onFocusChange ? { onFocusChange } : {})} />
    </div>
  );
}

/* ---- canvas + physics loop ---------------------------------------------- */

function TraceCanvas({
  model,
  reduced,
  onHoverChange,
  onDragChange,
}: {
  model: TraceModel;
  reduced: boolean;
  onHoverChange: (id: string | null) => void;
  onDragChange: (id: string | null) => void;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [narrow, setNarrow] = useState(false);

  const simRef = useRef<Simulation | null>(null);
  const rendererRef = useRef<TraceRenderer | null>(null);
  const bloomRef = useRef<Float64Array>(new Float64Array(model.nodes.length));
  const hoveredRef = useRef<string | null>(null);
  const draggingRef = useRef<string | null>(null);
  /** Set by the effect below; lets the pointer handlers re-settle in reduced
   * motion without being torn down and rebuilt on every gesture. */
  const reSettleRef = useRef<(() => void) | null>(null);
  /** Mirror of `narrow` readable from inside the draw loop. */
  const narrowRef = useRef(false);

  const description = useMemo(() => fieldDescription(model), [model]);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    if (!host || !canvas) return;

    let renderer: TraceRenderer;
    try {
      renderer = createRenderer(canvas, model);
    } catch {
      // jsdom and any context-starved environment land here. The field is a
      // picture of data that is fully present in the list below, so the
      // surface stays usable rather than blank-screening.
      setUnavailable(true);
      return;
    }
    rendererRef.current = renderer;
    renderer.setPalette(resolveTracePalette(host));

    const sim = createSimulation(buildSimSpec(model));
    simRef.current = sim;
    const bloom = new Float64Array(model.nodes.length);
    bloomRef.current = bloom;

    let raf = 0;
    let disposed = false;

    const paint = () => {
      if (narrowRef.current) return;
      renderer.draw({
        positions: sim.positions(),
        stretches: sim.stretches(),
        bloom,
        draggingId: draggingRef.current,
        hoveredId: hoveredRef.current,
        reducedMotion: reduced,
      });
    };

    /** Reduced motion: the identical step sequence, run to rest, painted once. */
    const settleAndPaint = () => {
      sim.settle({ dt: DT });
      // Bloom is a hover channel, not physics; at rest it is simply resolved to
      // its target so a keyboard reader sees the same emphasis a pointer would.
      for (let i = 0; i < bloom.length; i += 1) {
        bloom[i] = model.nodes[i]!.id === hoveredRef.current ? 1 : 0;
      }
      paint();
    };

    const frame = () => {
      if (disposed) return;
      sim.step(DT);
      model.nodes.forEach((node, i) => {
        const target = node.id === hoveredRef.current ? 1 : 0;
        bloom[i] = bloomStep(bloom[i]!, target, node.degree ?? 0, DT);
      });
      paint();
      raf = requestAnimationFrame(frame);
    };

    const resize = () => {
      const box = host.getBoundingClientRect();
      const width = Math.max(320, Math.round(box.width));
      // Below this the field stops being a reading of the data and becomes a
      // texture: sills fall under a pixel, labels overprint, and a reader would
      // be looking at something that cannot be measured. The ranked list below
      // carries every figure the field encodes, so the honest move is to say
      // the picture does not fit rather than to draw an unreadable one.
      // A zero measurement means "not laid out yet" (or a detached/jsdom
      // tree), which is not the same claim as "too narrow" — so it is not
      // treated as one.
      const tooNarrow = box.width > 0 && box.width < MIN_FIELD_WIDTH;
      narrowRef.current = tooNarrow;
      setNarrow(tooNarrow);
      // The world is 1440x1160; holding that ratio keeps the hop rings evenly
      // spaced at every width instead of squashing them at narrow viewports.
      const height = Math.round((width * model.world.height) / model.world.width);
      renderer.setViewport({ width, height, dpr: window.devicePixelRatio || 1 });
      if (reduced) paint();
    };
    resize();

    const observer =
      typeof ResizeObserver === 'function' ? new ResizeObserver(resize) : null;
    observer?.observe(host);

    const themeObserver =
      typeof MutationObserver === 'function'
        ? new MutationObserver(() => {
            renderer.setPalette(resolveTracePalette(host));
            paint();
          })
        : null;
    themeObserver?.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });

    if (reduced) settleAndPaint();
    else raf = requestAnimationFrame(frame);

    reSettleRef.current = settleAndPaint;

    return () => {
      disposed = true;
      if (raf) cancelAnimationFrame(raf);
      observer?.disconnect();
      themeObserver?.disconnect();
      rendererRef.current = null;
      simRef.current = null;
      reSettleRef.current = null;
    };
  }, [model, reduced]);

  const toWorld = useCallback((event: React.PointerEvent<HTMLCanvasElement>) => {
    const renderer = rendererRef.current;
    if (!renderer) return null;
    const rect = event.currentTarget.getBoundingClientRect();
    return renderer.toWorld(event.clientX - rect.left, event.clientY - rect.top);
  }, []);

  const onPointerMove = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>) => {
      const sim = simRef.current;
      const renderer = rendererRef.current;
      const world = toWorld(event);
      if (!sim || !renderer || !world) return;
      if (draggingRef.current) {
        sim.applyDrag(draggingRef.current, world.x, world.y);
        if (reduced) reSettleRef.current?.();
        return;
      }
      const hit = renderer.hitTest(sim.positions(), world.x, world.y);
      if (hit !== hoveredRef.current) {
        hoveredRef.current = hit;
        onHoverChange(hit);
        if (reduced) reSettleRef.current?.();
      }
    },
    [toWorld, reduced, onHoverChange],
  );

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>) => {
      const sim = simRef.current;
      const renderer = rendererRef.current;
      const world = toWorld(event);
      if (!sim || !renderer || !world) return;
      const hit = renderer.hitTest(sim.positions(), world.x, world.y);
      if (!hit) return;
      draggingRef.current = hit;
      onDragChange(hit);
      event.currentTarget.setPointerCapture?.(event.pointerId);
      sim.applyDrag(hit, world.x, world.y);
      if (reduced) reSettleRef.current?.();
    },
    [toWorld, reduced, onDragChange],
  );

  const endDrag = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>) => {
      const sim = simRef.current;
      if (!sim || !draggingRef.current) return;
      draggingRef.current = null;
      onDragChange(null);
      event.currentTarget.releasePointerCapture?.(event.pointerId);
      // Release carries no fling: a pinned node has zero velocity, so letting
      // go drops it home rather than throwing it. Momentum would feel good and
      // encode nothing measured.
      sim.release();
      if (reduced) reSettleRef.current?.();
    },
    [reduced, onDragChange],
  );

  return (
    <div ref={hostRef} className="relative w-full">
      {/* When the field is not drawn it is not described either: a `role="img"`
       * whose description narrates a picture nobody can see is worse than no
       * picture at all. */}
      <div {...(narrow ? {} : { role: 'img', 'aria-label': description })}>
        <canvas
          ref={canvasRef}
          data-testid="trace-canvas"
          aria-hidden
          className={cn('block w-full touch-none', narrow && 'hidden')}
          onPointerMove={onPointerMove}
          onPointerDown={onPointerDown}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          onPointerLeave={(event) => {
            endDrag(event);
            if (hoveredRef.current !== null) {
              hoveredRef.current = null;
              onHoverChange(null);
              if (reduced) reSettleRef.current?.();
            }
          }}
        />
      </div>
      {narrow && !unavailable ? (
        <p className="border border-edge-subtle bg-surface-1 p-4 text-center text-xs leading-relaxed text-text-muted">
          The field needs about {MIN_FIELD_WIDTH} px to be read and this column is
          narrower, so it is not drawn rather than drawn illegibly. Every symbol,
          hop and call-site count it carries is in the list below.
        </p>
      ) : null}
      {unavailable ? (
        <p className="p-4 text-center text-xs text-text-muted">
          This browser gave no 2D canvas, so the field is not drawn. Every symbol and
          call-site count it would carry is in the list below.
        </p>
      ) : null}
    </div>
  );
}

/* ---- controls ----------------------------------------------------------- */

const MOTION_OPTIONS: ReadonlyArray<{ value: MotionPreference; label: string }> = [
  { value: 'system', label: 'System' },
  { value: 'full', label: 'Full' },
  { value: 'reduced', label: 'Reduced' },
];

function MotionToggle({
  preference,
  reduced,
  onChange,
}: {
  preference: MotionPreference;
  reduced: boolean;
  onChange: (next: MotionPreference) => void;
}) {
  return (
    <div
      role="radiogroup"
      aria-label="Motion"
      className="flex shrink-0 items-center overflow-hidden rounded-[var(--radius-standard)] border border-edge-subtle"
    >
      {MOTION_OPTIONS.map((option) => {
        const active = preference === option.value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => onChange(option.value)}
            className={cn(
              'px-2 py-0.5 text-3xs',
              active
                ? 'bg-surface-2 text-text-primary'
                : 'bg-surface-0 text-text-muted hover:text-text-primary',
            )}
          >
            {option.value === 'system' && reduced ? `${option.label} · reduced` : option.label}
          </button>
        );
      })}
      <Gauge aria-hidden size={11} className="mx-1.5 shrink-0 text-text-muted" />
    </div>
  );
}

/* ---- the accessible equivalent ------------------------------------------ */

/**
 * The same symbols the field draws, as text, in call-site order.
 *
 * This is not a summary of the picture — it is the picture's exact contents.
 * Anything the field encodes as a position, a width or a latency is printed
 * here as a number, which is what makes the canvas legitimately one `role="img"`
 * rather than a grid of controls a screen reader has to walk.
 */
function TraceList({
  model,
  focusId,
  onFocusChange,
}: {
  model: TraceModel;
  focusId: string;
  onFocusChange?: (node: GraphNode) => void;
}) {
  const callSites = useMemo(() => {
    const totals = new Map<string, number>();
    for (const channel of model.channels) {
      totals.set(channel.a, (totals.get(channel.a) ?? 0) + channel.calls);
      totals.set(channel.b, (totals.get(channel.b) ?? 0) + channel.calls);
    }
    return totals;
  }, [model.channels]);

  const ordered = useMemo(
    () =>
      [...model.nodes].sort(
        (a, b) =>
          Math.abs(a.ring) - Math.abs(b.ring) ||
          (callSites.get(b.id) ?? 0) - (callSites.get(a.id) ?? 0) ||
          a.name.localeCompare(b.name),
      ),
    [model.nodes, callSites],
  );

  return (
    <div className="flex flex-col">
      <div className="flex items-center gap-2.5 border-b border-edge-subtle px-3 py-2">
        <span className="td-legend">symbols on the field</span>
        <span aria-hidden className="td-rule" />
        <span className="td-legend shrink-0 normal-case tracking-normal">
          {ordered.length} drawn · ordered by hop, then call sites
        </span>
      </div>
      <ol className="grid grid-cols-1 sm:grid-cols-2 xl:grid-cols-3">
        {ordered.map((node) => (
          <li key={node.id} className="min-w-0 border-b border-l border-edge-subtle">
            <TraceRow
              node={node}
              callSites={callSites.get(node.id) ?? 0}
              isFocus={node.id === focusId}
              {...(onFocusChange ? { onFocusChange } : {})}
            />
          </li>
        ))}
      </ol>
    </div>
  );
}

function TraceRow({
  node,
  callSites,
  isFocus,
  onFocusChange,
}: {
  node: TraceNode;
  callSites: number;
  isFocus: boolean;
  onFocusChange?: (node: GraphNode) => void;
}) {
  const side = node.ring === 0 ? 'focus' : node.ring < 0 ? 'calls it' : 'called by it';
  const hop = node.ring === 0 ? 'focus' : `${Math.abs(node.ring)} hop${Math.abs(node.ring) === 1 ? '' : 's'} ${node.ring < 0 ? 'up' : 'down'}`;
  const body = (
    <>
      <span className="flex min-w-0 items-baseline gap-2 leading-tight">
        <span
          aria-hidden
          className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
          style={kindColorVars(node.kind)}
        />
        <span className="td-value min-w-0 flex-1 truncate text-xs text-text-primary">
          {node.name}
        </span>
        <span className="td-value shrink-0 text-2xs text-text-secondary" data-cell="numeric">
          {callSites}
          <span className="td-unit ml-1">call sites</span>
        </span>
      </span>
      <span className="flex min-w-0 items-baseline gap-2 pl-3.5 leading-tight">
        <span className="td-legend shrink-0">{hop}</span>
        <span className="td-legend shrink-0 max-w-20 truncate normal-case tracking-normal text-text-muted">
          {side}
        </span>
        <span className="td-value min-w-0 flex-1 truncate text-right text-3xs text-text-muted">
          {node.degree == null ? 'degree absent' : `deg ${node.degree}`}
          {node.selfCalls ? ` · ${node.selfCalls} self-calls` : ''}
          {node.undrawnEdges ? ` · ${node.undrawnEdges} edges not drawn` : ''}
        </span>
      </span>
      {node.filePath ? (
        <span
          className="td-value truncate pl-3.5 text-left text-3xs text-text-muted"
          title={node.filePath}
        >
          {elideStart(node.filePath, 40)}
          {node.startLine == null ? '' : `:${node.startLine}`}
        </span>
      ) : null}
    </>
  );

  if (!onFocusChange || isFocus) {
    return (
      <div
        className={cn(
          'flex h-full w-full flex-col gap-0.5 px-3 py-1.5 text-left',
          isFocus ? 'bg-surface-2' : 'bg-surface-0',
        )}
      >
        {isFocus ? (
          <span className="flex items-center gap-1 text-3xs uppercase tracking-[0.18em] text-accent">
            <Crosshair aria-hidden size={10} />
            focus
          </span>
        ) : null}
        {body}
      </div>
    );
  }
  return (
    <button
      type="button"
      onClick={() =>
        onFocusChange({
          id: node.id,
          kind: node.kind,
          name: node.name,
          file_path: node.filePath,
          start_line: node.startLine,
          // An absent degree stays absent. Substituting 0 here would make the
          // re-centred field draw an unmeasured symbol as a measured leaf.
          ...(node.degree == null ? {} : { degree: node.degree }),
        })
      }
      title={`Re-centre the trace on ${node.name}`}
      className="flex h-full w-full flex-col gap-0.5 bg-surface-0 px-3 py-1.5 text-left hover:bg-surface-1 focus-visible:bg-surface-1"
    >
      {body}
    </button>
  );
}
