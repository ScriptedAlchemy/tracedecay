/**
 * PATH — `GET /api/plugins/graph/call-chain` (plan 11b Surface 1).
 *
 * The trace field shows a neighbourhood: everything within two hops of the
 * focus. This answers the different question the field cannot, because the
 * answer is frequently longer than the picture — "is there a call path from
 * the focus to that symbol, and what is it?"
 *
 * Target selection comes from the symbols already on the field rather than a
 * free-text box. A path query needs two node IDs, and the only IDs this surface
 * can offer without a second search index are the ones it has already drawn.
 *
 * The honesty load here is carried by one field. `found: false` is a real
 * measurement — the producer searched to `max_depth` and there is no path
 * within it — and it is NOT the same as an error or an unmeasured read. But it
 * is also not proof that no path exists, because the search is depth-bounded.
 * So a negative result prints the depth it searched to. A bare "no path" would
 * be a stronger claim than the endpoint made.
 */
import { useMemo, useState } from 'react';
import { CornerDownRight } from 'lucide-react';

import {
  StructureReadV1Schema as CallChainReadSchema,
  type CallChainMeasurementV1,
} from '../../contracts/generated.ts';
import { absenceReason, useStructure } from '../../data/query/structure.ts';
import { elideStart } from '../../ui/format.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import type { TraceModel } from '../../viz/trace/types.ts';

const BASE = '/api/plugins/graph';

export function CallChain({ model, focusId }: { model: TraceModel; focusId: string }) {
  const [target, setTarget] = useState('');

  // Everything on the field except the focus, in the list's own order, so the
  // dropdown and the ranked list below agree about what is reachable.
  const candidates = useMemo(
    () =>
      [...model.nodes]
        .filter((node) => node.id !== focusId)
        .sort((a, b) => Math.abs(a.ring) - Math.abs(b.ring) || a.name.localeCompare(b.name)),
    [model.nodes, focusId],
  );

  const chain = useStructure<CallChainMeasurementV1>(
    ['graph', 'call-chain', focusId, target],
    `${BASE}/call-chain?from=${encodeURIComponent(focusId)}&to=${encodeURIComponent(target)}`,
    CallChainReadSchema,
    { enabled: target !== '' },
  );

  return (
    <section
      className="flex flex-col gap-1.5 border-b border-edge-subtle px-3 py-2"
      aria-label="Call path from the focus"
    >
      <div className="flex flex-wrap items-center gap-2.5">
        <h3 className="td-legend shrink-0">path</h3>
        <span aria-hidden className="td-rule" />
        <label className="flex min-w-0 flex-1 items-center gap-1.5">
          <span className="td-legend shrink-0 normal-case tracking-normal text-text-muted">
            from focus to
          </span>
          <select
            value={target}
            onChange={(event) => setTarget(event.target.value)}
            aria-label="Path target symbol"
            className="h-6 min-w-0 flex-1 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-1.5 text-2xs text-text-primary focus:border-accent/60 focus:outline-none"
          >
            <option value="">select a symbol on the field</option>
            {candidates.map((node) => (
              <option key={node.id} value={node.id}>
                {node.name}
              </option>
            ))}
          </select>
        </label>
      </div>

      {target === '' ? (
        <p className="text-3xs leading-snug text-text-muted">
          the field draws a neighbourhood; this walks a route through it, which can be
          longer than the two hops drawn
        </p>
      ) : chain.isPending ? (
        <p className="text-2xs text-state-loading">searching…</p>
      ) : chain.data === undefined ? (
        <p className="text-2xs text-state-unknown">no response recorded</p>
      ) : chain.data.outcome !== 'measured' ? (
        <p className="text-2xs leading-relaxed text-state-unknown">
          {absenceReason(chain.data)}
        </p>
      ) : (
        <ChainReading measurement={chain.data.measurement} />
      )}
    </section>
  );
}

function ChainReading({ measurement }: { measurement: CallChainMeasurementV1 }) {
  if (!measurement.found) {
    return (
      <p className="text-2xs leading-relaxed text-state-unknown">
        no {measurement.edge_kind} path within {measurement.max_depth} hops
        {measurement.directed ? ' following call direction' : ' in either direction'}. A
        longer path is not excluded — the search is depth-bounded.
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-1">
      <p className="text-3xs leading-snug text-text-secondary">
        {measurement.hop_count ?? measurement.steps.length}{' '}
        {(measurement.hop_count ?? measurement.steps.length) === 1 ? 'hop' : 'hops'} ·{' '}
        {measurement.selection} · {measurement.edge_kind} edges
        {measurement.directed ? ', directed' : ', undirected'}
      </p>
      <ol className="flex flex-col">
        {measurement.steps.map((step, index) => (
          <li
            key={`${step.node.id}-${index}`}
            className="flex min-w-0 items-baseline gap-1.5 py-0.5"
          >
            {index === 0 ? (
              <span aria-hidden className="w-3 shrink-0" />
            ) : (
              <CornerDownRight
                aria-hidden
                size={10}
                className="w-3 shrink-0 translate-y-[1px] text-text-muted"
              />
            )}
            <span
              aria-hidden
              className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
              style={kindColorVars(step.node.kind)}
            />
            <span className="td-value min-w-0 flex-1 truncate text-2xs text-text-primary">
              {step.node.name}
            </span>
            {step.incoming_edge?.line != null ? (
              <span className="td-legend shrink-0" data-cell="numeric">
                :{step.incoming_edge.line}
              </span>
            ) : null}
            <span
              className="td-value w-40 shrink-0 truncate text-right text-3xs text-text-muted"
              title={step.node.file_path}
            >
              {elideStart(step.node.file_path, 24)}
            </span>
          </li>
        ))}
      </ol>
    </div>
  );
}
