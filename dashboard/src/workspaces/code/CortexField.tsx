/**
 * The Cortex aperture: the graph slice as one relief field, with its HUD.
 *
 * The field is `CortexSceneCanvas` drawing `reliefPainter`: symbols inside
 * their directory's hull, a relief of measured coupling under each hull, and
 * relations across directories bundled into trunks. Everything drawn over it
 * is a reading of the same slice: the scale and the rule that chose it (top
 * left), the symbol kinds on the field with their counts (bottom left), and
 * the strip beneath naming what each mark measures. The legends list what is
 * DRAWN, not what the index holds; the register above the aperture states the
 * whole.
 *
 * Hover on the field inspects; click pins. Both are wired through the
 * canvas's own `onInspect` / `onSelect`, so the list beside it and the
 * inspector share one identity model with the picture and the picture never
 * owns a selection.
 */
import { useMemo, type ReactNode } from 'react';

import type { GraphSubgraphPayloadV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { CenteredState, envelopeReadState } from '../../ui/ReadSection.tsx';
import { cn } from '../../ui/cn';
import type { ActivationField } from '../../viz/graph/activation.ts';
import { kindColor } from '../../viz/graph/kindColor.ts';
import { CortexSceneCanvas } from './CortexSceneCanvas.tsx';
import { kindLegend, relationLegend, type LegendEntry } from './cortex.ts';
import { reliefPainter } from './cortexReliefField.ts';
import { kindShape, sceneFromSlice, type KindShape } from './cortexScene.ts';
import { describeSubgraph } from './hubs.ts';

/** The most kinds a legend prints before folding the rest into one line. */
const LEGEND_ROWS = 7;

/** What each mark on the field measures, printed beneath it. */
const READINGS: readonly (readonly [string, string])[] = [
  ['size', 'degree · area ∝ in + out'],
  ['hue + shape', 'kind'],
  ['relief', 'drawn relation endpoints, per directory'],
  ['hull', 'directory of the file'],
  ['trunk', 'relations across directories · brighter = more'],
];

export function CortexField({
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
    <ReliefField
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

function ReliefField({
  payload,
  totalNodes,
  seedLabel,
  selectedId,
  inspectedId,
  onSelect,
  onInspect,
  activation,
}: {
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
      <CortexSceneCanvas
        scene={scene}
        painter={reliefPainter}
        selectedId={selectedId}
        inspectedId={inspectedId}
        onSelect={onSelect}
        onInspect={onInspect}
        activation={activation}
        ariaLabel={fieldDescription(payload, caption?.scale ?? null, kinds, seedLabel)}
        overlay={
          <>
            {caption ? (
              <Hud className="left-3 top-3 max-w-[60%]" label="slice">
                <span className="td-value text-2xs text-text-primary">{caption.scale}</span>
                {caption.capped ? (
                  <span className="td-legend text-state-partial">capped at the limit · more exists</span>
                ) : null}
                <span className="td-legend normal-case tracking-normal text-text-muted">
                  {payload.mode === 'seeded' ? 'seeded neighbourhood' : 'busiest connected region'}
                </span>
                {scene.unknownDegree > 0 ? (
                  <span className="td-legend normal-case tracking-normal text-state-unknown">
                    degree absent for {scene.unknownDegree} · dashed, not zero
                  </span>
                ) : null}
              </Hud>
            ) : null}
            <Hud className="bottom-3 left-3" label="symbol kind">
              <KindShapeList entries={kinds} />
            </Hud>
          </>
        }
      />
      <div
        className="flex flex-wrap items-center gap-x-4 gap-y-1 border-y border-edge-subtle/70 py-1 text-3xs"
        aria-label="Field reading"
      >
        {READINGS.map(([term, value]) => (
          <span key={term} className="inline-flex items-baseline gap-1.5">
            <span className="td-legend">{term}</span>
            <span className="td-value text-text-secondary">{value}</span>
          </span>
        ))}
        <StateKey />
        {relations.length > 0 ? <RelationList entries={relations} /> : null}
      </div>
      {caption ? <p className="text-3xs leading-relaxed text-text-muted">{caption.rule}</p> : null}
    </div>
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
    <ul className="flex flex-col gap-0.5 text-3xs" aria-label="drawn by kind">
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
function RelationList({ entries }: { entries: readonly LegendEntry[] }) {
  return (
    <ul className="flex flex-wrap gap-x-3 gap-y-0.5 text-3xs" aria-label="edges by kind">
      {entries.map((entry) => (
        <li key={entry.kind} className="flex items-center gap-1.5">
          <svg aria-hidden width="12" height="4" viewBox="0 0 12 4" className="shrink-0">
            <line
              x1="0"
              y1="2"
              x2="12"
              y2="2"
              strokeWidth="1"
              className="stroke-edge-strong"
              strokeDasharray={RELATION_DASH[entry.kind]}
            />
          </svg>
          <span className="td-value text-text-secondary">{entry.kind}</span>
          <span className="td-value text-text-muted" data-cell="numeric">
            {entry.count.toLocaleString()}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** One HUD plate on the field: bracketed, translucent, pointer-transparent. */
function Hud({
  label,
  className,
  children,
}: {
  label: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      className={cn(
        'absolute flex max-w-[45%] flex-col gap-1 border border-edge-subtle/80 bg-surface-0/80 px-2.5 py-1.5 backdrop-blur-sm',
        'max-sm:hidden',
        className,
      )}
      data-hud={label}
    >
      <span aria-hidden className="pointer-events-none absolute -left-px -top-px size-1.5 border-l border-t border-accent/60" />
      <span aria-hidden className="pointer-events-none absolute -right-px -top-px size-1.5 border-r border-t border-accent/60" />
      <span aria-hidden className="pointer-events-none absolute -bottom-px -left-px size-1.5 border-b border-l border-accent/60" />
      <span aria-hidden className="pointer-events-none absolute -bottom-px -right-px size-1.5 border-b border-r border-accent/60" />
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
): string {
  const composition = kinds
    .slice(0, LEGEND_ROWS)
    .map((entry) => `${entry.count} ${entry.kind}`)
    .join(', ');
  const rule =
    payload.mode === 'seeded'
      ? `the neighbourhood of ${seedLabel ?? 'the selected symbol'}, one edge deep`
      : "the graph's busiest connected region";
  return `Code cortex: ${scale ?? `${payload.nodes.length} symbols`} drawn as ${rule}. Symbols sit in their directory's hull; size is degree, hue and shape are kind (${composition}). Hover inspects a symbol in the inspector; click pins it. The symbol list beside the field is the accessible equivalent.`;
}
