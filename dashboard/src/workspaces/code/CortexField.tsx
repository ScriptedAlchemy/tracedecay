/**
 * The Cortex aperture: one served graph slice with selectable canvas views.
 *
 * Luminous, Plate and Relief reuse the current canvas lifetime, camera and
 * picking. Their legends describe their own encodings over the same data.
 *
 * Hover on the field inspects; click pins. Both are wired through the
 * canvas's own `onInspect` / `onSelect`, so the list beside it and the
 * inspector share one identity model with the picture and the picture never
 * owns a selection.
 */
import { useMemo, type ComponentProps, type ReactNode } from 'react';

import { StructureReadV12Schema as StrataReadSchema, type GraphSubgraphPayloadV1, type StrataMeasurementV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { useStructure } from '../../data/query/structure.ts';
import { CenteredState, envelopeReadState } from '../../ui/ReadSection.tsx';
import type { ActivationField } from '../../viz/graph/activation.ts';
import { kindColor } from '../../viz/graph/kindColor.ts';
import { CortexSceneCanvas } from './CortexSceneCanvas.tsx';
import { kindLegend, relationLegend, type LegendEntry } from './cortex.ts';
import { reliefPainterOffThread } from './cortexReliefOffThread.ts';
import { luminousPainter } from './cortexLuminous.ts';
import { DEPENDENCY_KINDS, describePlate, platePainter } from './cortexPlate.ts';
import { CORTEX_RENDERER_DETAILS, type CortexRenderer } from './cortexRenderers.ts';
import { kindShape, sceneFromSlice, type KindShape } from './cortexScene.ts';
import { describeSubgraph } from './hubs.ts';

/** The most kinds a legend prints before folding the rest into one line. */
const LEGEND_ROWS = 7;

export function CortexField({
  renderer,
  pending,
  result,
  selectedId,
  inspectedId,
  onSelect,
  onInspect,
  activation,
  totalNodes,
  seedLabel,
}: {
  renderer: CortexRenderer;
  pending: boolean;
  result: EnvelopeResult<GraphSubgraphPayloadV1> | undefined;
  selectedId: string | null;
  inspectedId: string | null;
  onSelect: (id: string | null) => void;
  onInspect: (id: string | null) => void;
  activation: ActivationField;
  totalNodes: number | null;
  seedLabel: string | null;
}) {
  const state = envelopeReadState(pending, result, {
    loading: 'reading the graph slice',
    transport: 'the graph slice could not be read',
  });
  if (state.kind !== 'ready') {
    return (
      <div className="flex min-h-0 flex-1 flex-col" data-cortex-field={state.state}>
        <CenteredState title="Code graph" kind={state.state} detail={state.detail} />
      </div>
    );
  }
  const payload = state.value.payload;
  if (payload.nodes.length === 0) {
    // An answered read that returned no symbols, said as the measurement it
    // is. The slice route reports its own failures with a non-2xx the boundary
    // renders instead of this, so reaching here means the graph holds nothing
    // for this seed.
    return (
      <div className="flex min-h-0 flex-1 flex-col" data-cortex-field="complete_zero_findings">
        <CenteredState
          title={
            payload.mode === 'seeded'
              ? 'No neighbourhood is indexed for this symbol'
              : 'No symbols are indexed for this project'
          }
          kind="complete_zero_findings"
        />
      </div>
    );
  }
  return (
    <PaintedField
      renderer={renderer}
      payload={payload}
      totalNodes={totalNodes}
      seedLabel={seedLabel}
      selectedId={selectedId}
      inspectedId={inspectedId}
      onSelect={onSelect}
      onInspect={onInspect}
      activation={activation}
    />
  );
}

function PaintedField({
  renderer,
  payload,
  totalNodes,
  seedLabel,
  selectedId,
  inspectedId,
  onSelect,
  onInspect,
  activation,
}: {
  renderer: CortexRenderer;
  payload: GraphSubgraphPayloadV1;
  totalNodes: number | null;
  seedLabel: string | null;
  selectedId: string | null;
  inspectedId: string | null;
  onSelect: (id: string | null) => void;
  onInspect: (id: string | null) => void;
  activation: ActivationField;
}) {
  const scene = useMemo(() => sceneFromSlice(payload.nodes, payload.edges), [payload]);
  const caption = describeSubgraph(payload, totalNodes, seedLabel);
  const kinds = kindLegend(payload.nodes);
  const relations = relationLegend(payload.edges);
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-1.5 p-3" data-cortex-field="ready">
      <CanvasRenderer
        renderer={renderer}
        scene={scene}
        selectedId={selectedId}
        inspectedId={inspectedId}
        onSelect={onSelect}
        onInspect={onInspect}
        activation={activation}
        ariaLabel={fieldDescription(payload, caption?.scale ?? null, kinds, seedLabel, renderer)}
      />
      <div className="flex shrink-0 flex-wrap items-start gap-x-6 gap-y-2 border-b border-edge-subtle/70 pb-1.5">
        {caption ? (
          <FieldReadout label="Slice">
            <span className="td-value text-2xs text-text-primary">{caption.scale}</span>
            {caption.capped ? <span className="td-legend text-state-partial">capped at the limit · more exists</span> : null}
            <span className="text-3xs text-text-muted">
              {payload.mode === 'seeded' ? 'seeded neighbourhood' : 'busiest connected region'}
            </span>
            {scene.unknownDegree > 0 ? (
              <span className="text-3xs text-state-unknown">degree absent for {scene.unknownDegree} · dashed, not zero</span>
            ) : null}
          </FieldReadout>
        ) : null}
        <FieldReadout label="Symbol kind"><KindShapeList entries={kinds} /></FieldReadout>
      </div>
      <div
        className="flex flex-wrap items-center gap-x-4 gap-y-1 border-y border-edge-subtle/70 py-1 text-3xs"
        aria-label="Field reading"
      >
        {CORTEX_RENDERER_DETAILS[renderer].readings.map(([term, value]) => (
          <span key={term} className="inline-flex items-baseline gap-1.5">
            <span className="td-legend">{term}</span>
            <span className="text-text-secondary">{value}</span>
          </span>
        ))}
        <StateKey />
        {relations.length > 0 ? <RelationList entries={relations} renderer={renderer} /> : null}
      </div>
      {caption ? <p className="text-3xs leading-relaxed text-text-muted">{caption.rule}</p> : null}
    </div>
  );
}

function CanvasRenderer({ renderer, ...props }: Omit<ComponentProps<typeof CortexSceneCanvas>, 'painter'> & { renderer: CortexRenderer }) {
  const strata = useStructure<StrataMeasurementV1>(
    ['graph', 'strata'],
    '/api/plugins/graph/strata',
    StrataReadSchema,
    { enabled: renderer === 'plate' },
  );
  const input = strata.isPending ? 'pending' : strata.data;
  const plate = useMemo(() => platePainter(input), [input]);
  const basis = useMemo(() => renderer === 'plate' ? describePlate(props.scene, input) : null, [renderer, props.scene, input]);
  // The optional basis gets its own slot; every mode keeps the same mounted
  // canvas host, observer and cancellation path in the second slot.
  return (
    <>
      {basis ? (
        <section aria-label="Plate layout basis" className="min-w-0 shrink-0 space-y-1 border border-edge-subtle bg-surface-1 px-2.5 py-2 text-3xs [overflow-wrap:anywhere]">
          <p className="font-medium text-text-primary">{basis.title}</p>
          <p className="text-text-secondary">{basis.detail}</p>
          <p className="text-text-muted">{basis.relations}</p>
        </section>
      ) : null}
      {renderer === 'plate' ? <CortexSceneCanvas {...props} painter={plate} />
        : renderer === 'luminous' ? <CortexSceneCanvas {...props} painter={luminousPainter} />
          : <CortexSceneCanvas {...props} painter={reliefPainterOffThread} />}
    </>
  );
}

/** Selection vocabulary. Pinning is a ring, not a hue: kinds share the cyan band. */
function StateKey() {
  return (
    <ul className="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-3xs text-text-secondary">
      <li className="flex items-center gap-1.5">
        <svg aria-hidden width="12" height="12" viewBox="0 0 12 12" className="shrink-0">
          <circle cx="6" cy="6" r="4.5" fill="none" strokeWidth="2" className="stroke-accent" />
        </svg>
        pinned · 2px ring, neighbourhood lifted
      </li>
      <li>hover or focus · dims the unrelated, inspects only</li>
      <li className="flex items-center gap-1.5">
        <svg aria-hidden width="12" height="12" viewBox="0 0 12 12" className="shrink-0">
          <circle cx="6" cy="6" r="5" fill="none" strokeWidth="0.8" className="stroke-accent opacity-60" />
        </svg>
        search hit · decays
      </li>
    </ul>
  );
}

function ShapeSwatch({ shape, kind }: { shape: KindShape; kind: string }) {
  const color = kindColor(kind, false);
  return (
    <svg aria-hidden width="10" height="10" viewBox="0 0 10 10" className="shrink-0">
      {shape === 'square' ? (
        <rect x="1.5" y="1.5" width="7" height="7" fill={color} />
      ) : shape === 'diamond' ? (
        <path d="M5 0.5 L9.5 5 L5 9.5 L0.5 5 Z" fill={color} />
      ) : shape === 'ring' ? (
        <circle cx="5" cy="5" r="3.3" fill="none" stroke={color} strokeWidth="1.6" />
      ) : (
        <circle cx="5" cy="5" r="3.8" fill={color} />
      )}
    </svg>
  );
}

function KindShapeList({ entries }: { entries: readonly LegendEntry[] }) {
  const folded = entries.slice(LEGEND_ROWS);
  return (
    <ul className="flex flex-wrap gap-x-3 gap-y-1 text-3xs" aria-label="drawn by kind">
      {entries.slice(0, LEGEND_ROWS).map((entry) => (
        <li key={entry.kind} className="flex items-center gap-1.5">
          <ShapeSwatch shape={kindShape(entry.kind)} kind={entry.kind} />
          <span className="td-value min-w-0 flex-1 truncate text-text-secondary">{entry.kind}</span>
          <span className="td-value shrink-0 text-text-muted" data-cell="numeric">
            {entry.count.toLocaleString()}
          </span>
        </li>
      ))}
      {folded.length > 0 ? (
        <li className="text-text-muted">
          + {folded.length} more ·{' '}
          {folded.reduce((sum, entry) => sum + entry.count, 0).toLocaleString()} drawn
        </li>
      ) : null}
    </ul>
  );
}

const RELATION_DASH: Record<string, string> = { references: '3 2', contains: '1 2' };

/** Relation kinds on the slice, each with the line style the field draws it in. */
function RelationList({ entries, renderer }: { entries: readonly LegendEntry[]; renderer: CortexRenderer }) {
  return (
    <ul className="flex flex-wrap gap-x-3 gap-y-0.5 text-3xs" aria-label="edges by kind">
      {entries.map((entry) => (
        <li key={entry.kind} className="flex items-center gap-1.5">
          {renderer !== 'plate' || DEPENDENCY_KINDS.has(entry.kind) ? <svg aria-hidden width="12" height="4" viewBox="0 0 12 4" className="shrink-0">
            <line
              x1="0"
              y1="2"
              x2="12"
              y2="2"
              strokeWidth="1"
              className="stroke-edge-strong"
              strokeDasharray={RELATION_DASH[entry.kind]}
            />
          </svg> : null}
          <span className="td-value text-text-secondary">{entry.kind}</span>
          {renderer === 'plate' && !DEPENDENCY_KINDS.has(entry.kind) ? <span className="text-text-muted">not routed</span> : null}
          <span className="td-value text-text-muted" data-cell="numeric">
            {entry.count.toLocaleString()}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** Readouts flow beside the chart, so narrow canvases keep their full data area. */
function FieldReadout({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <div role="group" aria-label={label} className="flex min-w-0 flex-1 basis-56 flex-col gap-1">
      <span className="td-legend">{label}</span>
      {children}
    </div>
  );
}

function fieldDescription(
  payload: GraphSubgraphPayloadV1,
  scale: string | null,
  kinds: readonly LegendEntry[],
  seedLabel: string | null,
  renderer: CortexRenderer,
): string {
  const composition = kinds
    .slice(0, LEGEND_ROWS)
    .map((entry) => `${entry.count} ${entry.kind}`)
    .join(', ');
  const rule =
    payload.mode === 'seeded'
      ? `the neighbourhood of ${seedLabel ?? 'the selected symbol'}, one edge deep`
      : "the graph's busiest connected region";
  return `Code cortex: ${scale ?? `${payload.nodes.length} symbols`} drawn as ${rule}. ${CORTEX_RENDERER_DETAILS[renderer].description} Size is degree, hue and shape are kind (${composition}). Hover inspects a symbol in the inspector; click pins it. The symbol list beside the field is the accessible equivalent.`;
}
