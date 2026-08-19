import type {
  ExecutionTopologyMetricsV1,
  ExecutionTopologyViewV1,
  WorkAttemptListCoverageV1,
  WorkAttemptListV1,
  WorkPlacementV1,
  WorkTopologyPlacementLaneV1,
} from '../../../contracts/index.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { Panel } from '../../../ui/instrument.tsx';
import type { WorkResult } from '../workApi.ts';
import type { WorkChannel } from '../workChannel.ts';
import type { WorkGraphReading } from '../workGraphModel.ts';
import type { WorkProductView } from '../workProductView.ts';
import {
  WORK_TOPOLOGY_DIMENSIONS,
  topologyDimensionLabel,
  workTopologyReading,
  type WorkTopologyDimension,
  type WorkTopologyReading,
} from '../workTopologyModel.ts';
import { WorkTopologyAccounting } from './WorkTopologyAccounting.tsx';
import { ChannelAbsence, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * Execution topology — the canonical structural view for Work.
 *
 * `operation.work.topology` publishes placement, branch, review, and
 * integration policy under one topology generation. This lens renders those
 * generated fields directly. It intentionally does not rebuild an executor
 * weave, worktree groups, or policy lanes by walking the attempt page: those
 * would be browser-owned alternatives to the application projection.
 *
 * The accounting ledger below still reads the separate attempt and graph
 * projections for measurements the structural topology view does not contain.
 * It binds snapshot titles, attempt-derived figures, and graph runtime figures
 * to this canonical generation before drawing them, so independently refreshed
 * reads cannot silently form one population.
 */

function coverageSentence(coverage: WorkAttemptListCoverageV1): string {
  switch (coverage.coverage) {
    case 'complete':
      return `${coverage.returned} ${coverage.returned === 1 ? 'attempt' : 'attempts'} returned · complete`;
    case 'capped':
      return `${coverage.returned} returned · capped with ${coverage.remaining} remaining; every page count is a floor`;
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

export function WorkTopologyView({
  snapshot,
  attemptList,
  topology,
  graph,
  metrics,
  selected,
  onSelect,
}: {
  snapshot: WorkProductView;
  /** The separate attempt page is solely for the accounting facts not present
   * in `ExecutionTopologyViewV1`; structural lanes always use `topology`. */
  attemptList: WorkResult<WorkAttemptListV1> | undefined;
  topology: WorkResult<ExecutionTopologyViewV1> | undefined;
  graph: WorkGraphReading;
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = workTopologyReading(topology);
  const titleJoin = snapshotTitlesBoundToTopology(snapshot, reading);

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-view="topology">
      <Panel legend="Execution topology" actions={<TopologyBindingChip reading={reading} />} elevation="well">
        <div className="flex min-w-0 flex-col gap-3">
          <TopologyCaption reading={reading} />
          <p className="text-3xs leading-snug text-text-muted">
            Placement lanes, branch policy, review policy, and integration strategy are decoded
            from the canonical topology page. A lane is one task/run identity with the durable
            placement state the authority published; no browser tally substitutes for it.
          </p>
          {titleJoin.channel === null ? null : <SnapshotTitleAbsence channel={titleJoin.channel} />}
          <PlacementLanes reading={reading} titles={titleJoin.titles} selected={selected} onSelect={onSelect} />
          <ChannelAbsence measure="wall-clock spans and durations" channel={reading.wallClock} />
          <DimensionLedger reading={reading} />
        </div>
      </Panel>

      <WorkTopologyAccounting
        attemptList={attemptList}
        topology={topology}
        graph={graph}
        metrics={metrics}
      />
    </div>
  );
}

interface SnapshotTitleJoin {
  readonly titles: ReadonlyMap<string, string>;
  readonly channel: WorkChannel<never> | null;
}

function SnapshotTitleAbsence({ channel }: { channel: WorkChannel<never> }) {
  if (channel.available) return null;
  return (
    <div data-work-snapshot-title-join={channel.state}>
      <ChannelAbsence measure="snapshot task titles" channel={channel} />
    </div>
  );
}

/**
 * Titles belong to the projection snapshot, not the topology response. They
 * can decorate canonical lanes only when both independently refreshed reads
 * identify the same topology population.
 */
function snapshotTitlesBoundToTopology(
  snapshot: WorkProductView,
  reading: WorkTopologyReading,
): SnapshotTitleJoin {
  if (!reading.binding.available) {
    return { titles: new Map(), channel: null };
  }

  const topologyGeneration = reading.binding.value.generation;
  if (snapshot.generation_id !== topologyGeneration) {
    return {
      titles: new Map(),
      channel: {
        available: false,
        state: 'conflicting',
        detail:
          `the Work snapshot is pinned to topology generation ${snapshot.generation_id}, but the canonical ` +
          `topology page is pinned to ${topologyGeneration}; their task titles are unbound, so canonical lanes use durable task identities`,
      },
    };
  }

  return {
    titles: new Map(snapshot.projections.map((projection) => [projection.task_id, projection.title])),
    channel: null,
  };
}

function TopologyBindingChip({ reading }: { reading: WorkTopologyReading }) {
  const binding = reading.binding;
  if (!binding.available) return <StateChip kind={binding.state} detail="topology generation" />;
  return (
    <span
      className="td-value text-3xs text-text-muted"
      data-work-topology-generation={binding.value.generation}
      data-cell="numeric"
    >
      generation {binding.value.generation} · {binding.value.task_count} tasks
    </span>
  );
}

function TopologyCaption({ reading }: { reading: WorkTopologyReading }) {
  return (
    <ViewCaption
      population="canonical execution-topology page"
      note={reading.coverage.available ? coverageSentence(reading.coverage.value) : undefined}
    />
  );
}

function PlacementLanes({
  reading,
  titles,
  selected,
  onSelect,
}: {
  reading: WorkTopologyReading;
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const placement = reading.executionPlacement;
  if (!placement.available) {
    return <ChannelAbsence measure="execution placement" channel={placement} />;
  }
  if (placement.value.lanes.length === 0) {
    return (
      <EmptyReading>
        The canonical topology page is authorized and contains no placement lanes. This is an
        empty page, not an inferred absence of execution.
      </EmptyReading>
    );
  }
  const capped = reading.coverage.available && reading.coverage.value.coverage === 'capped';
  return (
    <section aria-label="Canonical placement lanes" className="flex min-w-0 flex-col gap-2">
      <p className="text-3xs text-text-muted">
        placement mode: {placementMode(placement.value.mode)}
      </p>
      <ol className="flex min-w-0 flex-col gap-2" data-work-topology-lanes={placement.value.lanes.length}>
        {placement.value.lanes.map((lane) => (
          <li key={`${lane.task_id}\u0000${lane.run_id}`} className="min-w-0">
            <PlacementLane
              lane={lane}
              title={titles.get(lane.task_id) ?? lane.task_id}
              selected={selected === lane.task_id}
              capped={capped}
              onSelect={onSelect}
            />
          </li>
        ))}
      </ol>
    </section>
  );
}

function placementMode(mode: { kind: string; root_id?: string }): string {
  return mode.kind === 'configured_root' && mode.root_id !== undefined
    ? `${mode.kind} · ${mode.root_id}`
    : mode.kind;
}

function PlacementLane({
  lane,
  title,
  selected,
  capped,
  onSelect,
}: {
  lane: WorkTopologyPlacementLaneV1;
  title: string;
  selected: boolean;
  capped: boolean;
  onSelect: (taskId: string) => void;
}) {
  const identity = `${lane.task_id} · ${lane.run_id}`;
  return (
    <div
      className="flex min-w-0 flex-col gap-1.5 border border-edge-subtle bg-surface-1 p-2"
      data-work-topology-lane={`${lane.task_id}:${lane.run_id}`}
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <button
          type="button"
          onClick={() => onSelect(lane.task_id)}
          aria-pressed={selected}
          data-work-task={lane.task_id}
          className="min-h-[44px] min-w-0 flex-1 truncate text-left text-2xs text-text-secondary underline-offset-2 hover:underline"
        >
          {title}
        </button>
        <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {lane.attempt_count} {lane.attempt_count === 1 ? 'attempt' : 'attempts'}
          {capped ? ' · page floor' : ''}
        </span>
      </div>
      <span className="font-mono text-3xs text-text-muted">{identity}</span>
      {lane.placement.state === 'absent' ? (
        <span className="flex min-w-0 flex-col gap-1">
          <StateChip kind="complete_zero_findings" detail="no managed placement was admitted" />
          <span className="text-3xs text-text-muted">
            The canonical lane is retained even when its durable placement is absent.
          </span>
        </span>
      ) : (
        <PlacementDetail placement={lane.placement.placement} />
      )}
    </div>
  );
}

function PlacementDetail({ placement }: { placement: WorkPlacementV1 }) {
  return (
    <dl className="flex min-w-0 flex-col gap-1 text-3xs text-text-muted">
      <div className="flex min-w-0 gap-1.5">
        <dt className="shrink-0 uppercase tracking-[0.08em]">placement</dt>
        <dd className="min-w-0 break-words text-text-secondary">
          {placement.state} · {placement.target.kind} · {placement.target.root ?? 'no managed root'}
        </dd>
      </div>
      <div className="flex min-w-0 gap-1.5">
        <dt className="shrink-0 uppercase tracking-[0.08em]">blockers</dt>
        <dd className="min-w-0 break-words text-text-secondary">
          {placement.blockers.length === 0 ? 'none recorded' : placement.blockers.join(', ')}
        </dd>
      </div>
    </dl>
  );
}

function DimensionLedger({ reading }: { reading: WorkTopologyReading }) {
  return (
    <section
      aria-label="Topology dimensions"
      className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-2 p-2.5"
      data-work-topology-dimensions={WORK_TOPOLOGY_DIMENSIONS.length}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate text-text-secondary">Topology dimensions</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <ul className="flex min-w-0 flex-col gap-2">
        {WORK_TOPOLOGY_DIMENSIONS.map((dimension) => (
          <li key={dimension} className="min-w-0" data-work-dimension={dimension}>
            <DimensionRow reading={reading} dimension={dimension} />
          </li>
        ))}
      </ul>
    </section>
  );
}

function DimensionRow({ reading, dimension }: { reading: WorkTopologyReading; dimension: WorkTopologyDimension }) {
  switch (dimension) {
    case 'execution_placement':
      return dimensionValue(
        topologyDimensionLabel(dimension),
        reading.executionPlacement,
        reading.executionPlacement.available
          ? `mode ${placementMode(reading.executionPlacement.value.mode)} · ${reading.executionPlacement.value.lanes.length} canonical lanes`
          : null,
      );
    case 'branch_topology':
      return dimensionValue(
        topologyDimensionLabel(dimension),
        reading.branchTopology,
        reading.branchTopology.available
          ? `allowed: ${reading.branchTopology.value.allowed.join(', ')}`
          : null,
      );
    case 'review_topology':
      return dimensionValue(
        topologyDimensionLabel(dimension),
        reading.reviewTopology,
        reading.reviewTopology.available
          ? `allowed: ${reading.reviewTopology.value.allowed.join(', ')} · GitHub stacked PRs: ${reading.reviewTopology.value.github_stacked_prs}`
          : null,
      );
    case 'integration_strategy':
      return dimensionValue(
        topologyDimensionLabel(dimension),
        reading.integrationStrategy,
        reading.integrationStrategy.available
          ? `default cross-merge: ${reading.integrationStrategy.value.cross_merge.default_mode} · cross-repository: ${reading.integrationStrategy.value.cross_merge.allow_cross_repository ? 'allowed' : 'not allowed'}`
          : null,
      );
    default: {
      const unhandled: never = dimension;
      return unhandled;
    }
  }
}

function dimensionValue(
  label: string,
  channel: WorkChannel<unknown>,
  detail: string | null,
) {
  if (!channel.available) return <ChannelAbsence measure={label} channel={channel} />;
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <StateChip kind="ready" detail={label} />
      <p className="text-3xs leading-snug text-text-muted">{detail}</p>
    </div>
  );
}
