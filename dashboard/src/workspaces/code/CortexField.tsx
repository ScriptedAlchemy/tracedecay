/**
 * The Cortex aperture: the graph slice as one luminous field, with its HUD.
 *
 * The canvas is `GraphCanvas`, Sigma over Graphology, force-settled once,
 * and everything drawn over it is a reading of the same slice: the scale and
 * the rule that chose it (top left), the symbol kinds on the field with their
 * counts (bottom left), and the relation kinds the wire served (bottom right).
 * The legends list what is DRAWN, not what the index holds; the register
 * above the aperture states the whole.
 *
 * Hover on the field inspects; click pins. Both are wired through the canvas's
 * own `onInspect` / `onSelect`, so the list beside it and the inspector share
 * one identity model with the picture and the picture never owns a selection.
 */
import { useMemo, type ComponentProps, type ReactNode } from 'react';
import { useSearchParams } from 'react-router';

import {
  StructureReadV12Schema as StrataReadSchema,
  type GraphSubgraphPayloadV1,
  type StrataMeasurementV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { useStructure } from '../../data/query/structure.ts';
import { CenteredState, envelopeReadState } from '../../ui/ReadSection.tsx';
import { cn } from '../../ui/cn';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { CortexSceneCanvas } from './CortexSceneCanvas.tsx';
import { kindLegend, relationLegend, type LegendEntry } from './cortex.ts';
import { luminousPainter } from './cortexLuminous.ts';
import { platePainter } from './cortexPlate.ts';
import { reliefPainter } from './cortexReliefField.ts';
import {
  kindColorAt,
  kindStyle,
  readCortexRender,
  sceneFromSlice,
  type CortexRender,
  type KindShape,
} from './cortexScene.ts';
import { describeSubgraph } from './hubs.ts';

/** The most kinds a legend prints before folding the rest into one line. */
const LEGEND_ROWS = 7;

export function CortexField({
  pending,
  result,
  nodes,
  edges,
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
  nodes: ComponentProps<typeof GraphCanvas>['nodes'];
  edges: ComponentProps<typeof GraphCanvas>['edges'];
  selectedId: string | null;
  inspectedId: string | null;
  onSelect: (id: string | null) => void;
  onInspect: (id: string | null) => void;
  activation: ComponentProps<typeof GraphCanvas>['activation'];
  totalNodes: number | null;
  seedLabel: string | null;
}) {
  const [params] = useSearchParams();
  const render = readCortexRender(params);
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
  const caption = describeSubgraph(payload, totalNodes, seedLabel);
  const kinds = kindLegend(payload.nodes);
  const relations = relationLegend(payload.edges);
  if (render !== 'current') {
    return (
      <CortexCanvasField
        render={render}
        payload={payload}
        caption={caption}
        kinds={kinds}
        relations={relations}
        selectedId={selectedId}
        inspectedId={inspectedId}
        onSelect={onSelect}
        onInspect={onInspect}
        activation={activation}
        ariaLabel={fieldDescription(payload, caption?.scale ?? null, kinds, seedLabel)}
      />
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col p-3" data-cortex-field="ready">
      <GraphCanvas
        fill
        cameraControls
        nodes={nodes}
        edges={edges}
        selectedId={selectedId}
        inspectedId={inspectedId}
        onSelect={onSelect}
        onInspect={onInspect}
        activation={activation}
        canvasClassName="min-h-[52vw] md:min-h-[46vh] lg:min-h-[18rem]"
        ariaLabel={fieldDescription(payload, caption?.scale ?? null, kinds, seedLabel)}
        fallbackDescription="the symbol list and inspector beside this field remain available as a text alternative"
        encoding={{
          body: 'symbol',
          size: 'degree · in + out edges',
          hue: 'symbol kind',
          signal: 'search hit or click; never hover',
          relation: 'relation of any kind, drawn alike',
        }}
        caption={
          caption ? (
            <p className="text-3xs leading-relaxed text-text-muted">{caption.rule}</p>
          ) : null
        }
        overlay={
          <>
            {caption ? (
              <Hud className="left-3 top-3 max-w-[60%]" label="slice">
                <span className="td-value text-2xs text-text-primary">{caption.scale}</span>
                {caption.capped ? (
                  <span className="td-legend text-state-partial">capped at the limit</span>
                ) : null}
                <span className="td-legend normal-case tracking-normal text-text-muted">
                  {payload.mode === 'seeded' ? 'seeded neighbourhood' : 'busiest connected region'}
                </span>
              </Hud>
            ) : null}
            <Hud className="bottom-3 left-3" label="symbol kind">
              <LegendList entries={kinds} unit="drawn" />
            </Hud>
            <Hud className="bottom-3 right-3" label="reading">
              <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5 text-3xs">
                <dt className="td-legend">size</dt>
                <dd className="td-value text-text-secondary">degree</dd>
                <dt className="td-legend">hue</dt>
                <dd className="td-value text-text-secondary">kind</dd>
                <dt className="td-legend">glow</dt>
                <dd className="td-value text-text-secondary">search / click</dd>
              </dl>
              {relations.length > 0 ? (
                <div className="mt-1 border-t border-edge-subtle/70 pt-1">
                  <LegendList entries={relations} unit="edges" swatch="line" />
                </div>
              ) : null}
            </Hud>
          </>
        }
      />
    </div>
  );
}

type CanvasRender = Exclude<CortexRender, 'current'>;

/** What each canvas renderer's marks measure, printed beside the field. */
const RENDER_READINGS: Record<CanvasRender, readonly (readonly [string, string])[]> = {
  relief: [
    ['size', 'degree · area ∝ in + out'],
    ['hue + shape', 'kind'],
    ['relief', 'drawn relation endpoints'],
    ['hull', 'directory of the file'],
    ['bundle', 'relations across directories'],
  ],
  luminous: [
    ['size', 'degree'],
    ['brightness', 'degree'],
    ['hue', 'kind'],
    ['line', 'relation · overlap adds light'],
    ['labels', 'six highest degree'],
  ],
  plate: [
    ['band', 'dependency depth'],
    ['column', 'directory of the file'],
    ['station', 'symbol · size degree'],
    ['line', 'routed dependency'],
    ['error hue', 'cycle or climb'],
  ],
};

/** The canvas renderers under comparison; `?render=` selects one. */
function CortexCanvasField({
  render,
  payload,
  caption,
  kinds,
  relations,
  selectedId,
  inspectedId,
  onSelect,
  onInspect,
  activation,
  ariaLabel,
}: {
  render: CanvasRender;
  payload: GraphSubgraphPayloadV1;
  caption: ReturnType<typeof describeSubgraph>;
  kinds: readonly LegendEntry[];
  relations: readonly LegendEntry[];
  selectedId: string | null;
  inspectedId: string | null;
  onSelect: (id: string | null) => void;
  onInspect: (id: string | null) => void;
  activation: ComponentProps<typeof GraphCanvas>['activation'];
  ariaLabel: string;
}) {
  const scene = useMemo(() => sceneFromSlice(payload.nodes, payload.edges), [payload]);
  const strata = useStructure<StrataMeasurementV1>(
    ['graph', 'strata'],
    '/api/plugins/graph/strata',
    StrataReadSchema,
    { enabled: render === 'plate' },
  );
  const strataInput = strata.isPending ? 'pending' : strata.data;
  const plate = useMemo(() => platePainter(strataInput), [strataInput]);
  const shared = {
    scene,
    selectedId,
    inspectedId,
    onSelect,
    onInspect,
    activation,
    ariaLabel,
  };
  const slice = caption ? (
    <>
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
    </>
  ) : null;
  // One engraved strip under the field: what each mark measures, the
  // selection vocabulary, and the relation kinds with their line styles.
  const reading = (
    <div
      className="flex flex-wrap items-center gap-x-4 gap-y-1 border-y border-edge-subtle/70 py-1 text-3xs"
      aria-label="Field reading"
    >
      {RENDER_READINGS[render].map(([term, value]) => (
        <span key={term} className="inline-flex items-baseline gap-1.5">
          <span className="td-legend">{term}</span>
          <span className="td-value text-text-secondary">{value}</span>
        </span>
      ))}
      <StateKey />
      {relations.length > 0 ? (
        <LegendList entries={relations} unit="edges" swatch={render === 'luminous' ? 'line' : 'styled'} inline />
      ) : null}
    </div>
  );
  if (render === 'plate') {
    return (
      <div className="flex min-h-0 flex-1 flex-col gap-1.5 p-3" data-cortex-field="ready">
        <CortexSceneCanvas {...shared} painter={plate} />
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-3xs">
          {slice ? <span className="inline-flex flex-wrap items-baseline gap-x-2">{slice}</span> : null}
          <KindShapeList entries={kinds} inline />
        </div>
        {reading}
      </div>
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-1.5 p-3" data-cortex-field="ready">
      <CortexSceneCanvas
        {...shared}
        painter={render === 'relief' ? reliefPainter : luminousPainter}
        overlay={
          <>
            {slice ? (
              <Hud className="left-3 top-3 max-w-[60%]" label="slice">
                {slice}
              </Hud>
            ) : null}
            <Hud className="bottom-3 left-3" label="symbol kind">
              {render === 'luminous' ? (
                <LegendList entries={kinds} unit="drawn" swatch="kind" />
              ) : (
                <KindShapeList entries={kinds} />
              )}
            </Hud>
          </>
        }
      />
      {reading}
    </div>
  );
}

/** Selection vocabulary, identical across the canvas renderers. */
function StateKey() {
  return (
    <ul className="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-3xs text-text-secondary">
      <li className="flex items-center gap-1.5">
        <svg aria-hidden width="14" height="10" viewBox="0 0 14 10" className="shrink-0">
          <rect x="0" y="1" width="2" height="8" className="fill-accent" />
          <circle cx="9" cy="5" r="3.5" fill="none" strokeWidth="1.5" className="stroke-accent" />
        </svg>
        pinned
      </li>
      <li className="flex items-center gap-1.5">
        <svg aria-hidden width="14" height="10" viewBox="0 0 14 10" className="shrink-0">
          <circle cx="9" cy="5" r="3.5" fill="none" strokeWidth="1" className="stroke-text-primary" />
        </svg>
        hover or focus · inspects only
      </li>
      <li className="flex items-center gap-1.5">
        <svg aria-hidden width="14" height="10" viewBox="0 0 14 10" className="shrink-0">
          <circle cx="9" cy="5" r="4.5" fill="none" strokeWidth="0.8" strokeDasharray="0" className="stroke-accent opacity-60" />
        </svg>
        search hit · decays
      </li>
    </ul>
  );
}

function ShapeSwatch({ shape, kind }: { shape: KindShape; kind: string }) {
  const color = kindColorAt(kind, 0.76);
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

function KindShapeList({
  entries,
  inline = false,
}: {
  entries: readonly LegendEntry[];
  inline?: boolean;
}) {
  return (
    <ul
      className={cn('text-3xs', inline ? 'flex flex-wrap gap-x-3 gap-y-0.5' : 'flex flex-col gap-0.5')}
      aria-label="drawn by kind"
    >
      {entries.slice(0, inline ? entries.length : LEGEND_ROWS).map((entry) => (
        <li key={entry.kind} className="flex items-center gap-1.5">
          <ShapeSwatch shape={kindStyle(entry.kind).shape} kind={entry.kind} />
          <span className="td-value min-w-0 flex-1 truncate text-text-secondary">{entry.kind}</span>
          <span className="td-value shrink-0 text-text-muted" data-cell="numeric">
            {entry.count.toLocaleString()}
          </span>
        </li>
      ))}
      {!inline && entries.length > LEGEND_ROWS ? (
        <li className="text-text-muted">
          + {entries.length - LEGEND_ROWS} more ·{' '}
          {entries
            .slice(LEGEND_ROWS)
            .reduce((sum, entry) => sum + entry.count, 0)
            .toLocaleString()}{' '}
          drawn
        </li>
      ) : null}
    </ul>
  );
}

const RELATION_DASH: Record<string, string> = { references: '3 2', contains: '1 2' };

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

function LegendList({
  entries,
  unit,
  swatch = 'disc',
  inline = false,
}: {
  entries: readonly LegendEntry[];
  unit: string;
  swatch?: 'disc' | 'line' | 'kind' | 'styled';
  inline?: boolean;
}) {
  const shown = entries.slice(0, LEGEND_ROWS);
  const folded = entries.slice(LEGEND_ROWS);
  const foldedCount = folded.reduce((sum, entry) => sum + entry.count, 0);
  return (
    <ul
      className={cn('text-3xs', inline ? 'flex flex-wrap gap-x-3 gap-y-0.5' : 'flex flex-col gap-0.5')}
      aria-label={`${unit} by kind`}
    >
      {shown.map((entry) => (
        <li key={entry.kind} className="flex items-center gap-1.5">
          {swatch === 'disc' ? (
            <span
              aria-hidden
              className="size-1.5 shrink-0 rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
              style={kindColorVars(entry.kind)}
            />
          ) : swatch === 'kind' ? (
            <span
              aria-hidden
              className="size-1.5 shrink-0 rounded-full"
              style={{ background: kindColorAt(entry.kind, 0.8) }}
            />
          ) : swatch === 'styled' ? (
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
          ) : (
            <span aria-hidden className="h-px w-3 shrink-0 bg-edge-strong" />
          )}
          <span className="td-value min-w-0 flex-1 truncate text-text-secondary">{entry.kind}</span>
          <span className="td-value shrink-0 text-text-muted" data-cell="numeric">
            {entry.count.toLocaleString()}
          </span>
        </li>
      ))}
      {folded.length > 0 ? (
        <li className="text-text-muted">
          + {folded.length} more {folded.length === 1 ? 'kind' : 'kinds'} · {foldedCount.toLocaleString()} {unit}
        </li>
      ) : null}
    </ul>
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
  return `Code cortex: ${scale ?? `${payload.nodes.length} symbols`} drawn as ${rule}. Symbol size is degree, hue is kind (${composition}). Hover inspects a symbol in the inspector; click pins it. The symbol list beside the field is the accessible equivalent.`;
}
