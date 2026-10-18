import {
  useCallback,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react';
import { EvidenceGrade } from '../../../ui/EvidenceGrade.tsx';
import { StateChip } from '../../../ui/StateChip.tsx';
import { cn } from '../../../ui/cn.ts';
import {
  dagNeighborhood,
  dagPathToOutcome,
  dagPathToRoot,
  workDagLayout,
  type WorkDagLayout,
  type WorkDagLayoutNode,
  type WorkDagRelationKind,
} from '../workDagLayout.ts';
import { laneReading } from '../workLaneModel.ts';
import type { WorkProductView, WorkTaskView } from '../workProductView.ts';
import type { WorkDagReading } from '../workViewsModel.ts';
import { DagFittedField } from './DagFittedField.tsx';
import { DagMatrixField } from './DagMatrixField.tsx';
import { RelationSample, relationGrade, relationLabel, relationMarker } from './DagRelationLayer.tsx';
import type { CardWiring, DagFieldProps, Emphasis, WorkDagFieldView } from './dagField.ts';
import { EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * The task dependency board, the hero of channel thirteen.
 *
 * Hybrid on purpose. The DOM owns every card: each is a real button with the
 * task's exact identity, title, and the authority's projected lane as text, so
 * focus order, accessible names, and selection live where assistive
 * technology can reach them. The SVG beneath owns only the relation paths and
 * is hidden from the tree; the exact table under the field restates every
 * card and relation for a reader who never sees the drawing.
 *
 * Coordinates come from `workDagLayout`, a pure function of the reading, so the
 * cards and the paths agree without either measuring the other, and the same
 * graph version lays out identically on every reload.
 *
 * Interaction follows the design system's grammar:
 *
 *   hover / focus   inspects, isolates the one-hop neighbourhood and dims the
 *                   rest. It never moves the selection.
 *   click / Enter   selects, and the selection lives in the address.
 *   arrows          traverse the visible graph: left/right along a stratum,
 *                   up toward a gating dependency, down toward a dependent.
 *   focus path      isolates the declared path to root or to outcome for the
 *                   selected task; a reading of gating edges, not a guess.
 *   critical path   emphasises the authority's effort-weighted chain when it
 *                   was served, and is disabled with the channel's own reason
 *                   when it was not.
 *   zoom            pointer-independent −/100%/+/Fit over the field only; the
 *                   controls, legend, and table never scale with it.
 *
 * Nothing here animates a value. Dimming is a static opacity and the one
 * transition it uses collapses to zero under reduced motion.
 */

const ZOOM_MIN = 0.4;
const ZOOM_MAX = 2;
const ZOOM_STEP = 1.25;
const FITTED_FLOOR = 0.8;

type FocusPath = 'none' | 'root' | 'outcome';

/** The gating edges that join consecutive tasks of the authority's chain. The
 * chain is over the whole graph version, so a consecutive pair with no drawn
 * edge between them is simply not emphasised, never invented. */
function criticalEmphasis(layout: WorkDagLayout, chain: readonly string[]): Emphasis {
  const tasks = new Set(chain);
  const edges = new Set<string>();
  for (let index = 1; index < chain.length; index += 1) {
    const id = `gating:${chain[index - 1]}->${chain[index]}`;
    if (layout.edges.some((edge) => edge.id === id)) edges.add(id);
  }
  return { tasks, edges };
}

export function WorkDagBoard({
  snapshot,
  reading,
  selected,
  onSelect,
  view = 'graph',
}: {
  snapshot: WorkProductView;
  reading: WorkDagReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
  /** The fitted graph (the DAG camera) or the matrix (the Matrix camera). */
  view?: WorkDagFieldView;
}) {
  const layout = useMemo(
    () => workDagLayout(reading, snapshot.projections),
    [reading, snapshot.projections],
  );
  const tasks = useMemo(
    () => new Map(snapshot.projections.map((projection) => [projection.task_id, projection])),
    [snapshot.projections],
  );
  const [inspected, setInspected] = useState<string | null>(null);
  const [showLabels, setShowLabels] = useState(true);
  const [criticalOn, setCriticalOn] = useState(true);
  const [focusPath, setFocusPath] = useState<FocusPath>('none');
  const [zoom, setZoom] = useState(1);
  const field = useRef<HTMLDivElement | null>(null);
  const cards = useRef(new Map<string, HTMLButtonElement>());
  const controlsId = useId();

  const chain = reading.effort.available ? reading.effort.value.taskIds : [];
  const criticalAvailable = reading.effort.available && chain.length > 0;
  const critical = useMemo(
    () => (criticalOn && criticalAvailable ? criticalEmphasis(layout, chain) : null),
    [criticalOn, criticalAvailable, layout, chain],
  );

  // The inspect set wins over the focus path, which wins over nothing: a hover
  // is a question about one card, and answering it must not be blocked by a
  // standing focus. Both are derived, never stored.
  const isolation: Emphasis | null = useMemo(() => {
    if (inspected !== null && layout.byId.has(inspected)) {
      return dagNeighborhood(layout, inspected);
    }
    if (focusPath !== 'none' && selected !== null && layout.byId.has(selected)) {
      return focusPath === 'root'
        ? dagPathToRoot(reading, selected)
        : dagPathToOutcome(reading, selected);
    }
    return null;
  }, [inspected, focusPath, selected, layout, reading]);

  const fit = useCallback(() => {
    const width = field.current?.clientWidth ?? 0;
    if (width <= 0 || layout.width <= 0) {
      setZoom(1);
      return;
    }
    // The fitted renderer frames the graph with its own 24px margin and
    // stops at a readable floor; wider graphs scroll rather than shrink the
    // 11px identities below legibility.
    switch (view) {
      case 'graph':
        setZoom(Math.max(FITTED_FLOOR, Math.min(1, (width - 48) / layout.width)));
        return;
      case 'matrix':
        setZoom(1);
        return;
      default: {
        const unhandled: never = view;
        return unhandled;
      }
    }
  }, [layout, view]);

  // A graph wider than its field opens fitted, the 200%-zoom and narrow-
  // viewport focus mode, and re-fits when the graph version changes shape.
  // A field that cannot be measured leaves the zoom at 100%.
  useLayoutEffect(() => {
    fit();
  }, [fit]);

  const focusCard = (taskId: string | undefined) => {
    if (taskId === undefined) return;
    cards.current.get(taskId)?.focus();
  };

  const traverse = (event: KeyboardEvent<HTMLButtonElement>, node: WorkDagLayoutNode) => {
    const stratum = layout.strata.find((candidate) => candidate.depth === node.depth);
    if (stratum === undefined) return;
    const row = layout.strata.indexOf(stratum);
    const source = reading.nodes.get(node.taskId);
    switch (event.key) {
      case 'ArrowRight':
        event.preventDefault();
        focusCard(stratum.taskIds[node.column + 1]);
        break;
      case 'ArrowLeft':
        event.preventDefault();
        focusCard(stratum.taskIds[node.column - 1]);
        break;
      case 'Home':
        event.preventDefault();
        focusCard(stratum.taskIds[0]);
        break;
      case 'End':
        event.preventDefault();
        focusCard(stratum.taskIds[stratum.taskIds.length - 1]);
        break;
      case 'ArrowUp': {
        event.preventDefault();
        const dependency = source?.dependencies.find(
          (candidate) => (layout.byId.get(candidate)?.depth ?? node.depth) < node.depth,
        );
        focusCard(dependency ?? nearestInRow(layout, row - 1, node));
        break;
      }
      case 'ArrowDown': {
        event.preventDefault();
        const dependent = source?.dependents.find(
          (candidate) => (layout.byId.get(candidate)?.depth ?? node.depth) > node.depth,
        );
        focusCard(dependent ?? nearestInRow(layout, row + 1, node));
        break;
      }
      default:
        break;
    }
  };

  // Roving tabindex: one stop in the page's tab order, the selected card, or
  // the first card when nothing is selected, and the arrows move within.
  const tabStop =
    selected !== null && layout.byId.has(selected) ? selected : layout.nodes[0]?.taskId ?? null;

  const card = (taskId: string): CardWiring => ({
    tabIndex: tabStop === taskId ? 0 : -1,
    ref: (element) => {
      if (element === null) cards.current.delete(taskId);
      else cards.current.set(taskId, element);
    },
    onClick: () => onSelect(taskId),
    onFocus: () => setInspected(taskId),
    onBlur: () => setInspected(null),
    onPointerEnter: () => setInspected(taskId),
    onPointerLeave: () => setInspected(null),
    onKeyDown: (event) => {
      const node = layout.byId.get(taskId);
      if (node !== undefined) traverse(event, node);
    },
  });

  if (snapshot.projections.length === 0) {
    return (
      <div className="flex min-w-0 flex-col gap-3" data-work-dag-board="empty">
        <EmptyReading>
          The snapshot returned no tasks, so there is no graph to layer. This is the daemon
          reporting an empty board, not a projection that failed to draw.
        </EmptyReading>
      </div>
    );
  }

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-dag-board="drawn">
      <GraphControls
        id={controlsId}
        showLabels={showLabels}
        onShowLabels={setShowLabels}
        criticalOn={criticalOn}
        onCritical={setCriticalOn}
        criticalReading={reading}
        focusPath={focusPath}
        onFocusPath={setFocusPath}
        focusAvailable={selected !== null && layout.byId.has(selected)}
        zoom={zoom}
        onZoom={setZoom}
        onFit={fit}
        view={view}
      />

      <div
        ref={field}
        role="group"
        aria-label="Dependency graph field"
        aria-describedby={`${controlsId}-legend`}
        className={cn(
          'td-optic td-grain td-scanlines relative max-h-[62vh] min-h-48 overflow-auto',
          view === 'graph' && 'flex min-h-[22rem]',
        )}
        data-work-dag-field
        data-work-dag-view={view}
        data-work-dag-zoom={zoom.toFixed(2)}
        data-work-dag-inspected={inspected ?? undefined}
        onPointerLeave={() => setInspected(null)}
      >
        <DagField
          view={view}
          props={{
            layout,
            reading,
            tasks,
            selected,
            inspected,
            isolation,
            critical,
            showLabels,
            zoom,
            onSelect,
            onInspect: setInspected,
            card,
          }}
        />
      </div>

      <Legend id={`${controlsId}-legend`} layout={layout} reading={reading} snapshot={snapshot} />

      <ExactTable
        layout={layout}
        reading={reading}
        tasks={tasks}
        selected={selected}
        onSelect={onSelect}
        critical={critical}
      />
    </div>
  );
}

function DagField({ view, props }: { view: WorkDagFieldView; props: DagFieldProps }) {
  switch (view) {
    case 'graph':
      return <DagFittedField {...props} />;
    case 'matrix':
      return <DagMatrixField {...props} />;
    default: {
      const unhandled: never = view;
      return unhandled;
    }
  }
}

function nearestInRow(
  layout: WorkDagLayout,
  row: number,
  from: WorkDagLayoutNode,
): string | undefined {
  const stratum = layout.strata[row];
  if (stratum === undefined) return undefined;
  const centre = from.x + from.width / 2;
  let best: string | undefined;
  let bestDistance = Number.POSITIVE_INFINITY;
  for (const taskId of stratum.taskIds) {
    const node = layout.byId.get(taskId);
    if (node === undefined) continue;
    const distance = Math.abs(node.x + node.width / 2 - centre);
    if (distance < bestDistance) {
      bestDistance = distance;
      best = taskId;
    }
  }
  return best;
}

function GraphControls({
  id,
  showLabels,
  onShowLabels,
  criticalOn,
  onCritical,
  criticalReading,
  focusPath,
  onFocusPath,
  focusAvailable,
  zoom,
  onZoom,
  onFit,
  view,
}: {
  id: string;
  showLabels: boolean;
  onShowLabels: (value: boolean) => void;
  criticalOn: boolean;
  onCritical: (value: boolean) => void;
  criticalReading: WorkDagReading;
  focusPath: FocusPath;
  onFocusPath: (value: FocusPath) => void;
  focusAvailable: boolean;
  zoom: number;
  onZoom: (value: number) => void;
  onFit: () => void;
  view: WorkDagFieldView;
}) {
  const zoomable = view === 'graph';
  const effort = criticalReading.effort;
  const criticalDisabled = !effort.available || effort.value.taskIds.length === 0;
  const criticalNote = !effort.available
    ? effort.detail
    : effort.value.taskIds.length === 0
      ? 'the authority weighted an empty critical path for this graph version'
      : `${effort.value.taskIds.length} tasks · ${effort.value.totalEffort} effort`;
  return (
    <div
      className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-2 py-1"
      role="group"
      aria-label="Graph controls"
      data-work-dag-controls
    >
      <span className="td-legend text-text-secondary">graph controls</span>
      <span aria-hidden className="td-rule max-sm:hidden" />

      {/* The one layout this build has. Printed rather than offered as a
        * select with one option, which would promise a second layout that
        * does not exist. */}
      <span className="flex items-center gap-1.5 text-3xs text-text-muted">
        <span className="td-legend">layout</span>
        <span className="td-value text-3xs text-text-secondary">
          {view === 'graph' ? 'hierarchical · longest-path strata · fitted' : 'dependency matrix · strata order on both axes'}
        </span>
      </span>

      <label className="flex items-center text-2xs text-text-secondary">
        <input
          type="checkbox"
          className="td-check"
          checked={showLabels}
          onChange={(event) => onShowLabels(event.target.checked)}
        />
        show labels
      </label>

      <label
        className={cn('flex items-center text-2xs', criticalDisabled ? 'text-text-muted' : 'text-text-secondary')}
        title={criticalNote}
      >
        <input
          type="checkbox"
          className="td-check"
          checked={criticalOn && !criticalDisabled}
          disabled={criticalDisabled}
          aria-describedby={`${id}-critical`}
          onChange={(event) => onCritical(event.target.checked)}
          data-work-dag-critical={criticalDisabled ? 'absent' : criticalOn ? 'on' : 'off'}
        />
        critical path
      </label>
      <span id={`${id}-critical`} className="sr-only">
        {criticalNote}
      </span>

      <label className="flex items-center gap-1.5 text-2xs text-text-secondary">
        <span className="td-legend">focus</span>
        <select
          aria-label="Focus path"
          className="min-h-[44px] border border-edge-subtle bg-surface-2 px-2 text-2xs text-text-primary disabled:text-text-muted"
          value={focusPath}
          disabled={!focusAvailable}
          onChange={(event) => onFocusPath(event.target.value as FocusPath)}
          data-work-dag-focus={focusPath}
        >
          <option value="none">{focusAvailable ? 'whole graph' : 'select a task'}</option>
          <option value="root">path to root</option>
          <option value="outcome">path to outcome</option>
        </select>
      </label>

      <span className="ml-auto flex items-center gap-0.5" role="group" aria-label="Zoom">
        <ZoomButton label="Zoom out" onClick={() => onZoom(Math.max(ZOOM_MIN, zoom / ZOOM_STEP))} disabled={!zoomable || zoom <= ZOOM_MIN}>
          −
        </ZoomButton>
        <ZoomButton label="Reset zoom to 100%" onClick={() => onZoom(1)} disabled={!zoomable || zoom === 1}>
          <span className="td-value text-3xs" data-cell="numeric">
            {Math.round(zoom * 100)}%
          </span>
        </ZoomButton>
        <ZoomButton label="Zoom in" onClick={() => onZoom(Math.min(ZOOM_MAX, zoom * ZOOM_STEP))} disabled={!zoomable || zoom >= ZOOM_MAX}>
          +
        </ZoomButton>
        <ZoomButton label="Fit the graph to the field" onClick={onFit} disabled={!zoomable}>
          fit
        </ZoomButton>
      </span>
    </div>
  );
}

function ZoomButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      disabled={disabled}
      className="td-hit border border-edge-subtle px-2 text-2xs text-text-secondary hover:bg-surface-2 hover:text-text-primary disabled:cursor-not-allowed disabled:text-text-muted"
    >
      {children}
    </button>
  );
}

function Legend({
  id,
  layout,
  reading,
  snapshot,
}: {
  id: string;
  layout: WorkDagLayout;
  reading: WorkDagReading;
  snapshot: WorkProductView;
}) {
  const counts = layout.edges.reduce(
    (acc, edge) => {
      acc[edge.kind] += 1;
      return acc;
    },
    { gating: 0, informational: 0, causal: 0 } as Record<WorkDagRelationKind, number>,
  );
  return (
    <div id={id} className="flex min-w-0 flex-col gap-1.5" data-work-dag-legend>
      <ViewCaption
        population={`${snapshot.projections.length} tasks · ${layout.strata.length} strata · ${layout.edges.length} drawn relations`}
        note={
          reading.longestChain.length > 0
            ? `deepest declared chain ${reading.longestChain.length} deep, unweighted`
            : undefined
        }
      />
      <ul className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1 text-3xs text-text-muted">
        {(['gating', 'informational', 'causal'] as const).map((kind) => (
          <LegendItem key={kind} sample={<RelationSample kind={kind} />} grade={relationGrade(kind)} source="GRAPH">
            {relationMarker(kind)} · {relationLabel(kind)} · <span className="td-value">{counts[kind]}</span>
          </LegendItem>
        ))}
        <LegendItem sample={<span className="h-[2px] w-5 bg-alert" />} grade={reading.effort.available ? 'exact' : 'unavailable'} source="GRAPH">
          amber · effort-weighted critical path
        </LegendItem>
        <LegendItem sample={<span className="h-px w-5 bg-state-conflicting" />} grade="exact" source="GRAPH">
          conflicting hue · declared cycle · <span className="td-value">{reading.cycles.length}</span>
        </LegendItem>
        {layout.unresolved.length > 0 ? (
          <li className="flex items-center gap-1.5">
            <StateChip kind="partial" detail={`${layout.unresolved.length} soft relations name a task outside this page`} />
          </li>
        ) : null}
      </ul>
    </div>
  );
}

function LegendItem({
  sample,
  grade,
  source,
  children,
}: {
  sample: ReactNode;
  grade: 'exact' | 'explicit' | 'unavailable';
  source: string;
  children: ReactNode;
}) {
  return (
    <li className="flex items-center gap-1.5">
      <span aria-hidden className="flex w-6 items-center text-text-secondary">
        {sample}
      </span>
      <span>{children}</span>
      <EvidenceGrade grade={grade} source={source} />
    </li>
  );
}

/**
 * The complete accessible fallback: every card and every relation the field
 * draws, as one captioned table with the same selection control. A reader who
 * never sees the drawing loses nothing the drawing states.
 */
function ExactTable({
  layout,
  reading,
  tasks,
  selected,
  onSelect,
  critical,
}: {
  layout: WorkDagLayout;
  reading: WorkDagReading;
  tasks: ReadonlyMap<string, WorkTaskView>;
  selected: string | null;
  onSelect: (taskId: string) => void;
  critical: Emphasis | null;
}) {
  return (
    <details className="group min-w-0 border border-edge-subtle bg-surface-1" data-work-dag-table>
      <summary className="flex min-h-[44px] cursor-pointer items-center gap-2 px-2.5 text-2xs text-text-secondary hover:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent">
        <span className="td-legend">exact table</span>
        <span aria-hidden className="td-rule" />
        <span className="td-value text-3xs text-text-muted" data-cell="numeric">
          {layout.nodes.length} tasks · {layout.edges.length} relations
        </span>
      </summary>
      <div role="region" aria-label="Exact task and relation table" tabIndex={0} className="min-w-0 overflow-x-auto">
        <table className="w-full min-w-0 border-collapse text-2xs">
          <caption className="sr-only">
            Every task on the dependency graph with its identity, the authority's projected lane,
            its stratum depth, its gating and soft relation counts, declared effort, and the
            milestone it belongs to. Selecting a row selects the same task the graph card
            selects.
          </caption>
          <thead>
            <tr className="border-b border-edge-subtle text-text-muted">
              <th scope="col" className="px-2 py-1 text-left font-medium">Task</th>
              <th scope="col" className="px-2 py-1 text-left font-medium">Identity</th>
              <th scope="col" className="px-2 py-1 text-left font-medium">Lane</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Depth</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Gating in</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Gating out</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Soft</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Effort</th>
              <th scope="col" className="px-2 py-1 text-left font-medium">Milestone</th>
              <th scope="col" className="px-2 py-1 text-left font-medium">Grade</th>
            </tr>
          </thead>
          <tbody>
            {layout.nodes.map((node) => {
              const task = tasks.get(node.taskId);
              const source = reading.nodes.get(node.taskId);
              if (task === undefined) return null;
              const lane = laneReading(task.lane);
              const isSelected = selected === node.taskId;
              return (
                <tr
                  key={node.taskId}
                  data-work-dag-row={node.taskId}
                  className={isSelected ? 'bg-surface-3 outline outline-1 -outline-offset-1 outline-accent' : 'hover:bg-surface-2'}
                >
                  <th scope="row" className="px-2 py-1 text-left font-medium text-text-primary">
                    <button
                      type="button"
                      onClick={() => onSelect(node.taskId)}
                      aria-pressed={isSelected}
                      className="flex min-h-[44px] w-full items-center text-left underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                      data-work-task={node.taskId}
                    >
                      {task.title}
                      {critical?.tasks.has(node.taskId) ? (
                        <span className="td-legend ml-2 text-alert">critical</span>
                      ) : null}
                    </button>
                  </th>
                  <td className="px-2 py-1 font-mono text-text-secondary">{node.taskId}</td>
                  <td className="px-2 py-1 text-text-secondary">
                    <span className="td-legend">{lane.label}</span>
                  </td>
                  <td className="px-2 py-1 text-right tabular-nums text-text-secondary">{node.depth}</td>
                  <td className="px-2 py-1 text-right tabular-nums text-text-secondary">{source?.dependencies.length ?? 0}</td>
                  <td className="px-2 py-1 text-right tabular-nums text-text-secondary">{source?.dependents.length ?? 0}</td>
                  <td className="px-2 py-1 text-right tabular-nums text-text-secondary">
                    {task.informational_relations.length + task.causal_candidates.length}
                  </td>
                  <td className="px-2 py-1 text-right tabular-nums text-text-secondary">{task.effort}</td>
                  <td className="px-2 py-1 font-mono text-text-secondary">{task.hierarchy.milestone_id}</td>
                  <td className="px-2 py-1">
                    <EvidenceGrade grade={task.lane.kind === 'projected' ? 'exact' : 'unavailable'} source="KANBAN" />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {layout.edges.length === 0 ? null : (
          <table className="mt-2 w-full min-w-0 border-collapse text-2xs">
            <caption className="sr-only">
              Every relation drawn on the graph: its kind, the task it leaves, the task it reaches,
              and whether it runs against the strata inside a declared cycle.
            </caption>
            <thead>
              <tr className="border-b border-edge-subtle text-text-muted">
                <th scope="col" className="px-2 py-1 text-left font-medium">Relation</th>
                <th scope="col" className="px-2 py-1 text-left font-medium">From</th>
                <th scope="col" className="px-2 py-1 text-left font-medium">To</th>
                <th scope="col" className="px-2 py-1 text-left font-medium">Direction</th>
              </tr>
            </thead>
            <tbody>
              {layout.edges.map((edge) => (
                <tr key={edge.id} data-work-dag-relation-row={edge.id}>
                  <td className="px-2 py-1 text-text-secondary">{relationLabel(edge.kind)}</td>
                  <td className="px-2 py-1 font-mono text-text-secondary">{edge.from}</td>
                  <td className="px-2 py-1 font-mono text-text-secondary">{edge.to}</td>
                  <td className="px-2 py-1 text-text-muted">{edge.climb ? 'climbs inside a declared cycle' : 'down the strata'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </details>
  );
}
