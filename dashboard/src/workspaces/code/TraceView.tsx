/**
 * TRACE. The Code workspace's symbol-anatomy drill-in (plan 11b Surface 1).
 *
 * The selected symbol is a machined plate of measured fields; its callers
 * stand left and its callees right as bars on one call-site scale, two hops
 * deep, with every drawn call link as a connector between row ports.
 *
 *   `viz/trace/model.ts`  wire payload → model and counts. Every figure
 *                         printed on this surface comes from there.
 *   `viz/trace/plate.ts`  model → geometry. Decides no number.
 *
 * DEPTH is not decided here. `traceNeighborhood.ts` is the only module that
 * knows a two-hop neighbourhood costs more than one request, and it says why
 * that is provisional; everything above it receives payloads.
 *
 * ACCESSIBILITY. Every symbol on the plate is a focusable control, and
 * `TraceList` is its exact text equivalent: every drawn symbol, in call-site
 * order. The two always render together from the one model. Nothing moves, so
 * reduced motion only drops the hover fade.
 */
import { useEffect, useMemo } from 'react';
import { ArrowLeft } from 'lucide-react';

import { CenteredState, ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import {
  buildTraceModel,
  undrawnNeighbours,
  type NeighborsPayload,
} from '../../viz/trace/model.ts';
import { PlateField } from '../../viz/trace/PlateField.tsx';
import { useReducedMotion } from '../../viz/trace/reducedMotion.ts';
import { CallChain } from './CallChain.tsx';
import { NodeEvidence } from './NodeEvidence.tsx';
import { TraceList } from './TraceList.tsx';
import { TraceReadoutStrip } from './TraceReadoutStrip.tsx';
import { useTraceNeighborhood } from './traceNeighborhood.ts';

/**
 * The symbol a Code surface is currently centred on.
 *
 * Not a wire shape. The three things that can set a focus hold three different
 * amounts: the search list and the hub field hold a whole `GraphNodeV1` off the
 * graph routes, while a click inside the trace field holds only what the
 * plate carries, id, kind, name, file and line. A `GraphNodeV1` satisfies
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
  end_line?: number | null;
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
        <ReadSection
          title="Trace"
          chrome="centered"
          state={envelopeReadState(neighborhood.pending, neighborhood.result, {
            loading: 'reading call edges for this symbol',
            transport: 'call edges could not be read',
          })}
        >
          {(envelope) => {
            const payload = envelope.payload;
            const callers = payload.callers ?? [];
            const callees = payload.callees ?? [];
            if (callers.length === 0 && callees.length === 0) {
              return (
                <div className="flex flex-col gap-2 p-6">
                  <CenteredState title="Call-edge result is unverified" kind="partial" />
                  <p className="mx-auto max-w-md text-center text-xs leading-relaxed text-text-muted">
                    The graph response returned no <code className="font-mono">calls</code>{' '}
                    rows for {displayName(focus)}, but the envelope carries no per-edge read
                    health. The frontend cannot distinguish a successful empty result from a query
                    failure without a typed refusal.
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
        </ReadSection>
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
  const input = useMemo(
    () => ({
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
  const model = useMemo(() => buildTraceModel(input), [input]);
  const undrawn = useMemo(() => undrawnNeighbours(input, model), [input, model]);

  const { reduced } = useReducedMotion();

  return (
    <div className="flex flex-col">
      {/* Every figure on the strip is counted from `model` by `readoutCells`. */}
      <TraceReadoutStrip model={model} expanding={expanding} />

      <figure className="flex flex-col border-b border-edge-subtle pt-2">
        <PlateField
          model={model}
          root={root}
          meta={{ signature: focus.signature ?? null, endLine: focus.end_line ?? null }}
          undrawn={undrawn}
          reduced={reduced}
          onPin={onFocusChange}
        />
      </figure>

      {/* What is known about the focus beyond its call edges, and a route
       * through the neighbourhood the field can only show two hops of. Both
       * sit between the field and the ranked list because they are readings
       * about the SAME symbol the plate above is measuring, the list below is
       * about its neighbours. */}
      <NodeEvidence nodeId={focus.id} nodeName={displayName(focus)} />
      <CallChain model={model} focusId={focus.id} />

      <TraceList model={model} focusId={focus.id} {...(onFocusChange ? { onFocusChange } : {})} />
    </div>
  );
}
