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
 * This file composes those three through the siblings that present them — the
 * plate, the field, the key, the felt channels, the motion control and the
 * ranked list — and is responsible for one thing of its own: saying out loud
 * what the picture is and is not showing.
 *
 * DEPTH is not decided here. `traceNeighborhood.ts` is the only module that
 * knows a two-hop neighbourhood costs more than one request, and it says why
 * that is provisional; everything above it receives payloads.
 *
 * ACCESSIBILITY. The canvas is one `role="img"` with a description that carries
 * the same claims as the caption, and `TraceList` is its exact equivalent:
 * every symbol on the field is in it, in call-site order, as keyboard-reachable
 * text. That pairing is a property of this composition and of nothing smaller —
 * neither component can assert it alone — which is why the two are always
 * rendered together, from the one model, and never conditionally. Reduced
 * motion is a rendering MODE rather than a switched-off feature (see
 * `TraceCanvas`), and it can be pinned on or off from the surface regardless of
 * the OS setting.
 */
import { useEffect, useMemo, useState } from 'react';
import { ArrowLeft } from 'lucide-react';

import { CenteredState, LegacyBoundary } from '../../ui/ReadSection.tsx';
import { buildTraceModel, type NeighborsPayload } from '../../viz/trace/model.ts';
import { useReducedMotion } from '../../viz/trace/reducedMotion.ts';
import { CallChain } from './CallChain.tsx';
import { NodeEvidence } from './NodeEvidence.tsx';
import { TraceCanvas } from './TraceCanvas.tsx';
import { TraceFeltChannels } from './TraceFeltChannels.tsx';
import { TraceLegend } from './TraceLegend.tsx';
import { TraceList } from './TraceList.tsx';
import { TraceMotionToggle } from './TraceMotionToggle.tsx';
import { TraceReadoutStrip } from './TraceReadoutStrip.tsx';
import { useTraceNeighborhood } from './traceNeighborhood.ts';

/**
 * The symbol a Code surface is currently centred on.
 *
 * Not a wire shape. The three things that can set a focus hold three different
 * amounts: the search list and the hub field hold a whole `GraphNodeV1` off the
 * graph routes, while a click inside the trace field holds only what the
 * simulation carries — id, kind, name, file and line. A `GraphNodeV1` satisfies
 * this, so the richer sources pass straight through; the trace field states
 * what it actually knows instead of padding the rest of the wire shape with
 * nulls it never received.
 */
export interface TraceFocus {
  id: string;
  kind: string;
  name?: string | null;
  qualified_name?: string | null;
  file_path?: string | null;
  start_line?: number | null;
  signature?: string | null;
  degree?: number | null;
}

function displayName(node: {
  name?: string | null;
  qualified_name?: string | null;
  id: string;
}): string {
  return node.name ?? node.qualified_name ?? node.id;
}

/* ---- the surface -------------------------------------------------------- */

export function TraceView({
  focus,
  onClose,
  onFocusChange,
}: {
  focus: TraceFocus;
  onClose: () => void;
  /** Re-flood the field on another symbol, from the list below. */
  onFocusChange?: (node: TraceFocus) => void;
}) {
  const neighborhood = useTraceNeighborhood(focus.id);

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
        <LegacyBoundary title="Trace" pending={neighborhood.pending} result={neighborhood.result}>
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
                expanded={neighborhood.expanded}
                expanding={neighborhood.expanding}
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
  focus: TraceFocus;
  root: NeighborsPayload;
  expanded: ReadonlyMap<string, NeighborsPayload>;
  expanding: boolean;
  onFocusChange?: (node: TraceFocus) => void;
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
      {/* The plate above the field. Every figure on it is counted from `model`
       * by `readoutCells`, which is the same record `TraceCanvas` draws. */}
      <TraceReadoutStrip model={model} expanding={expanding} />

      <figure className="flex flex-col gap-1.5 border-b border-edge-subtle px-3 pb-2 pt-2">
        <TraceCanvas
          model={model}
          reduced={reduced}
          onHoverChange={setHovered}
          onDragChange={setDragging}
        />
        {/* The key below the field. A single run-on `TRACE_ENCODINGS` line
         * used to carry all of this; the legend says it as layout, and says
         * what each channel is carrying right now, which prose could not. */}
        <figcaption className="flex flex-col gap-2" data-testid="trace-key">
          <TraceLegend model={model} />
          <TraceFeltChannels model={model} />
        </figcaption>
      </figure>

      <div className="flex flex-wrap items-center gap-2.5 border-b border-edge-subtle px-3 py-1.5">
        <span className="td-legend shrink-0">motion</span>
        <TraceMotionToggle preference={preference} reduced={reduced} onChange={setPreference} />
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

      {/* What is known about the focus beyond its call edges, and a route
       * through the neighbourhood the field can only show two hops of. Both
       * sit between the field and the ranked list because they are readings
       * about the SAME symbol the plate above is measuring — the list below is
       * about its neighbours. */}
      <NodeEvidence nodeId={focus.id} nodeName={displayName(focus)} />
      <CallChain model={model} focusId={focus.id} />

      <TraceList model={model} focusId={focus.id} {...(onFocusChange ? { onFocusChange } : {})} />
    </div>
  );
}
