import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { useSearchParams } from 'react-router';
import type { ReactNode } from 'react';
import { Waypoints } from 'lucide-react';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { envelopePayload, useEnvelope } from '../../data/query/useEnvelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import type { DashboardEnvelopeV1 } from '../../contracts/generated.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { Legend, Panel, ReadoutBar, WorkspaceHeader } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { formatCount } from '../../ui/format.ts';
import { ProximityPanel } from '../../viz/proximity/index.ts';
import { projectJourney } from '../../viz/temporal/journey.ts';
import {
  DEFAULT_DENSE_LANE_THRESHOLD,
  fittedWindowFor,
  layoutTemporalScene,
} from '../../viz/temporal/layout.ts';
import { densityIndex, layoutDensity, membershipKey } from '../../viz/temporal/density.ts';
import { glyphLabel } from '../../viz/temporal/glyphs.tsx';
import { TemporalScene } from '../../viz/temporal/TemporalScene.tsx';
import type {
  JourneyEventKind,
  JourneyProjection,
  SceneWindow,
  TemporalSceneModel,
} from '../../viz/temporal/types.ts';
import { JOURNEY_EVENT_KINDS } from '../../viz/temporal/types.ts';
import { useReducedMotion } from '../../viz/trace/reducedMotion.ts';
import { BranchNavigator } from './BranchNavigator.tsx';
import {
  LOOM_PARAMS,
  parseHiddenKinds,
  parseLaneSet,
  parseWindow,
  parseZoom,
  serializeHiddenKinds,
  serializeLaneSet,
  serializeWindow,
  toggleInSet,
} from './loomUrl.ts';
import { useLoomProximity, type LoomProximityState } from './loomProximity.ts';
import { ThreadChain } from './ThreadChain.tsx';
import { clampWindow, formatDurationSeconds, formatMoment, isFitted } from './tracks.ts';
import { useLoomPlayback } from './useLoomPlayback.ts';
import {
  AnalyticsSubagentTreePayloadV1Schema,
  type AnalyticsSubagentTreePayloadV1,
  LcmSessionPayloadV1Schema,
  LcmTimelinePayloadV1Schema,
  type LoomSourceStatusV1,
  type LoomTemporalPayloadV1,
  LoomTemporalPayloadV1Schema,
} from '../../contracts/generated.ts';

/**
 * Loom, the temporal execution field.
 *
 * Time runs left to right. Hierarchy runs down: provider rail, root session,
 * then subagents under their recorded parent. Every coordinate is produced by
 * the pure layout in `viz/temporal`, so the same loaded page draws the same
 * field on every reload and in every renderer; the scene only paints it.
 *
 * What is drawn is what an authority served: session extents from the store,
 * parentage from each session row's own parent columns cross-checked against
 * the subagent tree, commits, timed edits and branch spans from the durable
 * relation rows, the selected session's turns from its loaded transcript page.
 * Handoffs and results have no session-bound authority in this read; a join
 * is drawn only where a child ends inside its parent's measured extent, and
 * it is graded inferred.
 */
export function LoomPage() {
  const scope = useScope((state) => state.scope);
  const temporal = useQuery({
    queryKey: ['loom', 'temporal', scopeKey(scope)],
    queryFn: () =>
      fetchEnvelope<LoomTemporalPayloadV1>(
        scopedUrl(scope, '/api/loom/temporal?limit=200'),
        LoomTemporalPayloadV1Schema,
      ),
  });
  const timeline = useEnvelope(
    ['loom', 'timeline'],
    '/api/plugins/hermes-lcm/timeline',
    LcmTimelinePayloadV1Schema,
  );
  const hierarchy = useEnvelope(
    ['loom', 'hierarchy'],
    '/api/plugins/analytics/subagent-tree',
    AnalyticsSubagentTreePayloadV1Schema,
  );
  const proximity = useLoomProximity();
  const [params, setParams] = useSearchParams();
  const selectedId = params.get(LOOM_PARAMS.session);
  const setSelectedId = (id: string | null) => {
    const next = new URLSearchParams(params);
    if (id == null) next.delete(LOOM_PARAMS.session);
    else next.set(LOOM_PARAMS.session, id);
    next.delete(LOOM_PARAMS.event);
    setParams(next);
  };

  const busiestDay = useMemo(() => {
    const buckets = envelopePayload(timeline.data)?.buckets ?? [];
    return buckets.reduce<{ bucket: string; count: number } | null>(
      (max, bucket) => (max == null || bucket.count > max.count ? bucket : max),
      null,
    );
  }, [timeline.data]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorkspaceHeader
        path="loom"
        title="Loom"
        note="temporal execution field · time left to right, recorded hierarchy down"
      />
      <TemporalBoundary pending={temporal.isPending} result={temporal.data}>
        {(envelope) => (
          <TemporalBody
            envelope={envelope}
            hierarchy={envelopePayload(hierarchy.data)}
            hierarchyPending={hierarchy.isPending}
            busiestDay={busiestDay}
            timelinePending={timeline.isPending}
            timelineServed={timeline.data?.outcome === 'envelope'}
            selectedId={selectedId}
            selectedEncounterId={proximity.selectedEncounterId}
            proximity={proximity.result}
            proximityEncounters={proximity.encounters}
            onSelect={setSelectedId}
            onSelectEncounter={proximity.selectEncounter}
          />
        )}
      </TemporalBoundary>
    </div>
  );
}

const DEFAULT_WIDTH = 960;
const LABEL_COLUMN = 200;
const NARROW_LABEL_COLUMN = 28;
const RIGHT_GUTTER = 28;

function TemporalBody({
  envelope,
  hierarchy,
  hierarchyPending,
  busiestDay,
  timelinePending,
  timelineServed,
  selectedId,
  selectedEncounterId,
  proximity,
  proximityEncounters,
  onSelect,
  onSelectEncounter,
}: {
  envelope: DashboardEnvelopeV1<LoomTemporalPayloadV1>;
  hierarchy: AnalyticsSubagentTreePayloadV1 | undefined;
  hierarchyPending: boolean;
  busiestDay: { bucket: string; count: number } | null;
  timelinePending: boolean;
  timelineServed: boolean;
  selectedId: string | null;
  selectedEncounterId: string | null;
  proximity: LoomProximityState['result'];
  proximityEncounters: LoomProximityState['encounters'];
  onSelect: (id: string | null) => void;
  onSelectEncounter: (id: string | null) => void;
}) {
  const [params, setParams] = useSearchParams();
  const [width, setWidth] = useState(DEFAULT_WIDTH);
  const { reduced } = useReducedMotion();
  const data = envelope.payload;
  const rows = data.sessions ?? [];

  const selectedLane = useMemo(() => {
    const row = rows.find(
      (session) => JSON.stringify([session.provider || 'unknown', session.session_id]) === selectedId,
    );
    return row ? { id: selectedId!, sessionId: row.session_id } : null;
  }, [rows, selectedId]);

  const chain = useEnvelope(
    ['loom', 'chain', selectedLane?.id ?? 'none'],
    `/api/plugins/hermes-lcm/session/${encodeURIComponent(selectedLane?.sessionId ?? '')}?limit=200`,
    LcmSessionPayloadV1Schema,
    { enabled: selectedLane != null },
  );
  const chainPayload = envelopePayload(chain.data);
  const chainMessages = chainPayload?.exists === false ? undefined : chainPayload?.messages;
  const playback = useLoomPlayback(selectedLane?.id ?? null, chainMessages);

  const hierarchyState: 'loading' | 'loaded' | 'unavailable' = hierarchyPending
    ? 'loading'
    : hierarchy?.available && !hierarchy.error
      ? 'loaded'
      : 'unavailable';

  const projection = useMemo(
    () =>
      projectJourney({
        temporal: data,
        hierarchy: hierarchy ?? null,
        hierarchyState,
        selected:
          selectedLane && chainMessages
            ? { laneId: selectedLane.id, messages: chainMessages }
            : null,
        encounters: proximityEncounters,
      }),
    [data, hierarchy, hierarchyState, selectedLane, chainMessages, proximityEncounters],
  );

  const fullWindow = useMemo(() => fittedWindowFor(projection.extent), [projection.extent]);
  const requestedWindow = parseWindow(params.get(LOOM_PARAMS.window));
  const window: SceneWindow =
    requestedWindow && projection.extent
      ? clampWindow(requestedWindow, projection.extent)
      : fullWindow;
  const following = projection.extent ? isFitted(window, projection.extent) : true;
  const collapsed = parseLaneSet(params.get(LOOM_PARAMS.collapsed));
  const expanded = parseLaneSet(params.get(LOOM_PARAMS.expanded));
  const hiddenKinds = parseHiddenKinds(params.get(LOOM_PARAMS.hidden));
  const zoom = selectedLane ? 'event' : parseZoom(params.get(LOOM_PARAMS.zoom));

  const model = useMemo(
    () =>
      layoutTemporalScene(projection, {
        viewport: {
          width,
          left: width < 480 ? NARROW_LABEL_COLUMN : LABEL_COLUMN,
          right: RIGHT_GUTTER,
          window,
        },
        zoom,
        branches: { collapsed, expanded },
        selectedLaneId: selectedLane?.id ?? null,
        selectedEventId: playback.active && !playback.state.followLive
          ? `msg:${selectedLane?.id ?? ''}:${playback.active.id}`
          : null,
        reveal: playback.reveal,
        hiddenKinds,
        denseLaneThreshold: DEFAULT_DENSE_LANE_THRESHOLD,
      }),
    [projection, width, window, zoom, collapsed, expanded, selectedLane, playback.active, playback.state.followLive, playback.reveal, hiddenKinds],
  );
  // The index reads only the page, the bundle membership (keyed, since
  // `model` changes with every window), the filters and the cursor; a window
  // change re-bins it and nothing more.
  const membership = membershipKey(model);
  const index = useMemo(() => densityIndex(projection, model, { reveal: playback.reveal, hiddenKinds }), [projection, membership, playback.reveal, hiddenKinds]);
  const density = useMemo(() => layoutDensity(index, model), [index, model]);

  const update = (mutate: (next: URLSearchParams) => void) => {
    const next = new URLSearchParams(params);
    mutate(next);
    setParams(next, { replace: true });
  };
  const setWindow = (next: SceneWindow | null) =>
    update((search) => {
      if (next == null || (projection.extent && isFitted(next, projection.extent))) {
        search.delete(LOOM_PARAMS.window);
      } else {
        search.set(LOOM_PARAMS.window, serializeWindow(next));
      }
    });
  const toggleBranch = (laneId: string) =>
    update((search) => {
      const lane = model.lanes.find((candidate) => candidate.id === laneId);
      const depth = projection.lanes.find((candidate) => candidate.id === laneId)?.depth ?? 0;
      if (model.denseDepth !== null && depth === model.denseDepth) {
        // On a dense page this level starts collapsed; the explicit sets record
        // the reader's departure from that default in either direction.
        const currentlyCollapsed = lane?.kind === 'bundle';
        const nextExpanded = new Set(expanded);
        const nextCollapsed = new Set(collapsed);
        if (currentlyCollapsed) {
          nextExpanded.add(laneId);
          nextCollapsed.delete(laneId);
        } else {
          nextExpanded.delete(laneId);
          nextCollapsed.add(laneId);
        }
        writeSet(search, LOOM_PARAMS.expanded, nextExpanded);
        writeSet(search, LOOM_PARAMS.collapsed, nextCollapsed);
        return;
      }
      writeSet(search, LOOM_PARAMS.collapsed, toggleInSet(collapsed, laneId));
    });
  const toggleKind = (kind: JourneyEventKind) =>
    update((search) => {
      const next = serializeHiddenKinds(toggleInSet(hiddenKinds, kind));
      if (next == null) search.delete(LOOM_PARAMS.hidden);
      else search.set(LOOM_PARAMS.hidden, next);
    });
  const setZoom = (next: 'workstream' | 'agent') =>
    update((search) => {
      if (next === 'agent') search.delete(LOOM_PARAMS.zoom);
      else search.set(LOOM_PARAMS.zoom, next);
    });
  const selectEvent = (eventId: string) => {
    const node = model.nodes.find((candidate) => candidate.id === eventId);
    if (!node) return;
    switch (node.kind) {
      case 'message_user':
      case 'message_assistant':
      case 'message_other':
      case 'tool_call': {
        const index = playback.frames.findIndex((frame) => frame.id === node.ref);
        if (index >= 0) {
          playback.setState({ ...playback.state, cursor: index, playing: false, followLive: false });
        }
        return;
      }
      case 'spawn':
        onSelect(node.ref);
        return;
      case 'session_start':
      case 'session_end':
      case 'commit':
      case 'file_edit':
        if (node.laneId !== selectedLane?.id) onSelect(node.laneId);
        return;
      default: {
        const exhaustive: never = node.kind;
        return exhaustive;
      }
    }
  };

  if (data.available === false) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-8">
        <div className="flex max-w-sm flex-col items-center gap-3 text-center">
          <StateChip kind="unknown" detail="session store not readable" />
          <p className="text-xs leading-relaxed text-text-muted">
            The daemon answered but reported its session store unavailable, so
            there is no session to place on the axis.{' '}
            <span className="text-text-secondary">
              This is the store saying so, not an empty result.
            </span>
          </p>
        </div>
      </div>
    );
  }

  const commitStatus =
    data.source_statuses.find((source) => source.id === 'session_commit') ?? null;
  const branchStatus =
    data.source_statuses.find((source) => source.id === 'branch_worktree') ?? null;
  const measuredEnds = projection.stats.lanes - projection.stats.openEnded;
  const selectedJourneyLane = selectedLane
    ? projection.lanes.find((lane) => lane.id === selectedLane.id) ?? null
    : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {!selectedLane && (
        <ReadoutBar
          label="Field readings"
          elevation="raised"
          items={[
            {
              label: 'sessions',
              value: projection.stats.lanes.toLocaleString(),
              note: data.total ? `of ${formatCount(data.total)} in store` : undefined,
            },
            {
              label: 'agents',
              value: `${projection.stats.roots} + ${projection.stats.subagents}`,
              note: 'roots + subagents, unique within this loaded page',
            },
            { label: 'messages', value: formatCount(projection.stats.messages) },
            {
              label: 'measured extent',
              value: `${measuredEnds}/${projection.stats.lanes}`,
              note: 'recorded end or last-message observation',
              fraction: projection.stats.lanes > 0 ? measuredEnds / projection.stats.lanes : null,
            },
            {
              label: 'window',
              value: formatDurationSeconds(window.end - window.start),
              note: following ? 'fitted to the loaded page' : `${formatMoment(window.start)} – ${formatMoment(window.end)}`,
            },
            {
              label: 'busiest day',
              value: busiestDay ? formatCount(busiestDay.count) : '—',
              note: busiestDay
                ? busiestDay.bucket
                : timelinePending
                  ? 'timeline loading'
                  : timelineServed
                    ? 'no timeline activity recorded'
                    : 'timeline read failed',
            },
          ]}
        />
      )}

      <div
        role="region"
        aria-label="Loom content"
        tabIndex={0}
        className={cn('flex min-h-0 flex-1 flex-col gap-2 overflow-auto px-3 py-1 [scrollbar-gutter:stable]')}
      >
        {selectedId && !selectedLane ? (
          <StateChip
            kind="unavailable"
            detail="Selected session is outside this loaded page; choose a retained session."
          />
        ) : null}

        {projection.lanes.length === 0 ? (
          <EmptyField undated={projection.stats.undated} rows={rows.length} />
        ) : (
          <>
            <FieldControls
              following={following}
              onReturnToTail={() => setWindow(null)}
              zoom={zoom}
              onZoom={setZoom}
              projection={projection}
              model={model}
              hiddenKinds={hiddenKinds}
              onToggleKind={toggleKind}
              selected={selectedLane != null}
            />
            <TemporalScene
              model={model}
              ariaLabel={fieldDescription(projection, model)}
              density={density}
              fullWindow={fullWindow}
              reducedMotion={reduced}
              onMeasure={setWidth}
              onWindowChange={setWindow}
              onSelectLane={onSelect}
              onSelectEvent={selectEvent}
              onToggleBranch={toggleBranch}
              onSelectEncounter={onSelectEncounter}
            />
            <p className="text-3xs text-text-muted">
              Session hierarchy:{' '}
              {hierarchyPending
                ? 'loading'
                : !hierarchy?.available || hierarchy.error
                  ? 'unavailable'
                  : `${hierarchy.truncated ? 'partial' : 'loaded'} · ${hierarchy.missing_parent_count} missing parents · ${hierarchy.cycle_count} cycles`}
              . Forks leave the parent at the child session&apos;s recorded start, graded
              inferred because the loaded transcript carries no tool-use identity; a
              session row and the subagent tree that disagree are both drawn, ambiguous.
              A join is inferred only where a child ends inside its parent&apos;s measured
              extent; no handoff or result authority serves one. Only this temporal page
              is drawn.
            </p>

            {selectedJourneyLane ? (
              <ThreadChain
                thread={selectedJourneyLane}
                chainPending={chain.isPending}
                chain={chain.data}
                playback={playback}
                summaryNodes={chainPayload?.exists === false ? [] : chainPayload?.summary_nodes ?? []}
                totalMessages={chainPayload?.exists === false ? 0 : chainPayload?.counts.message_count ?? 0}
                hasMoreMessages={
                  chainPayload?.exists === false
                    ? false
                    : (chainPayload?.has_more_messages ?? false) || chainPayload?.next_cursor != null
                }
                hasMoreSummaryNodes={chainPayload?.exists === false ? false : chainPayload?.has_more_summary_nodes ?? false}
                relations={{
                  commits: data.commits.filter(
                    (commit) => commit.provider === selectedJourneyLane.provider && commit.session_id === selectedJourneyLane.sessionId,
                  ),
                  editedFiles: data.edited_files.filter(
                    (file) => file.provider === selectedJourneyLane.provider && file.session_id === selectedJourneyLane.sessionId,
                  ),
                  branchSpans: data.branch_spans.filter(
                    (span) => span.provider === selectedJourneyLane.provider && span.session_id === selectedJourneyLane.sessionId,
                  ),
                  commitStatus,
                  branchStatus,
                }}
                onReturn={() => onSelect(null)}
              />
            ) : null}

            <ProximityPanel
              result={proximity}
              selectedId={selectedEncounterId}
              onSelect={onSelectEncounter}
            />
            <FieldCaption projection={projection} model={model} />
            <BranchNavigator
              projection={projection}
              model={model}
              selectedLaneId={selectedLane?.id ?? null}
              onSelect={onSelect}
              onToggle={toggleBranch}
            />
          </>
        )}

        <aside className={cn('flex w-full shrink-0 flex-col gap-3', 'order-first')}>
          <details>
            <summary className="flex min-h-[var(--touch-target-min)] cursor-pointer items-center text-3xs text-text-muted">
              Source coverage · {envelope.freshness.state} ·{' '}
              {data.source_statuses.map((source) => `${source.label}: ${source.state}`).join(' · ')}
            </summary>
            <div className={cn('flex gap-3', 'flex-wrap [&>*]:min-w-64 [&>*]:flex-1')}>
              <Panel legend="Causal crossings">
                <div className="flex flex-col gap-2">
                  <p className="text-2xs leading-relaxed text-text-muted">
                    Counts below are the persisted causal rows returned for this
                    exact session page. Provider, granularity and coverage come
                    from the temporal response.
                  </p>
                  {data.source_statuses.map((source) => (
                    <div key={source.id} className="flex flex-col gap-1">
                      <span className="td-legend text-text-secondary">{source.label}</span>
                      <StateChip kind={source.state} detail={sourceDetail(source)} />
                      <span className="td-value truncate text-3xs text-text-muted">
                        {source.granularity}
                        {source.item_count == null ? '' : ` · ${source.item_count} rows`}
                      </span>
                    </div>
                  ))}
                </div>
              </Panel>

              <Panel legend="Read identity">
                <div className="flex flex-col gap-2">
                  <StateChip
                    kind={freshnessKind(envelope.freshness.state)}
                    detail={
                      envelope.freshness.observed_at_micros == null
                        ? 'observation time unrecorded'
                        : `observed ${formatMoment(envelope.freshness.observed_at_micros / 1_000_000)}`
                    }
                  />
                  <StateChip
                    kind={data.temporal_refresh.state}
                    detail={`${data.temporal_refresh.active_generations} active temporal generations · ${
                      data.temporal_refresh.latest_activated_at_micros == null
                        ? 'activation time unrecorded'
                        : `latest activation ${formatMoment(data.temporal_refresh.latest_activated_at_micros / 1_000_000)}`
                    } · ${data.temporal_refresh.authority}`}
                  />
                  <p className="text-3xs leading-relaxed text-text-muted">{coverageDetail(envelope)}</p>
                  <p className="text-3xs leading-relaxed text-text-muted">
                    {envelope.source_watermark
                      ? `${envelope.source_watermark.source} · ${envelope.source_watermark.watermark}`
                      : 'No temporal source watermark was recorded.'}
                  </p>
                </div>
              </Panel>
            </div>
          </details>
        </aside>
      </div>
    </div>
  );
}

function writeSet(search: URLSearchParams, key: string, ids: ReadonlySet<string>) {
  const serialized = serializeLaneSet(ids);
  if (serialized == null) search.delete(key);
  else search.set(key, serialized);
}

/**
 * The DOM controls the field never owns: follow/return-to-tail, semantic
 * zoom, and the event-kind filters. Filters change visibility, never source
 * truth, and the counts beside them come from the layout so the words and the
 * picture cannot drift.
 */
function FieldControls({
  following,
  onReturnToTail,
  zoom,
  onZoom,
  projection,
  model,
  hiddenKinds,
  onToggleKind,
  selected,
}: {
  following: boolean;
  onReturnToTail: () => void;
  zoom: 'workstream' | 'agent' | 'event';
  onZoom: (zoom: 'workstream' | 'agent') => void;
  projection: JourneyProjection;
  model: TemporalSceneModel;
  hiddenKinds: ReadonlySet<JourneyEventKind>;
  onToggleKind: (kind: JourneyEventKind) => void;
  selected: boolean;
}) {
  const kindCounts = useMemo(() => {
    const counts = new Map<JourneyEventKind, number>();
    for (const event of projection.events) counts.set(event.kind, (counts.get(event.kind) ?? 0) + 1);
    return counts;
  }, [projection.events]);
  const { counts } = model;
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border border-edge-subtle px-2 py-1 text-3xs">
      <div className="flex items-center gap-2" role="group" aria-label="Loaded tail">
        {following ? (
          <span className="border border-accent/40 px-1.5 py-0.5 td-legend text-accent" data-follow="following">
            following loaded tail
          </span>
        ) : (
          <button
            type="button"
            className="td-hit border border-edge-subtle px-1.5 td-legend text-text-secondary"
            onClick={onReturnToTail}
            aria-label="Return field to loaded tail"
          >
            RETURN TO LOADED TAIL
          </button>
        )}
        <span className="text-text-muted">NOW = newest record in this loaded page · not a live stream</span>
      </div>
      <div className="flex items-center gap-1" role="group" aria-label="Semantic zoom">
        <span className="td-legend">zoom</span>
        {(['workstream', 'agent'] as const).map((level) => (
          <button
            key={level}
            type="button"
            aria-pressed={zoom === level}
            disabled={selected}
            onClick={() => onZoom(level)}
            className={cn(
              'td-hit border px-1.5 td-legend',
              zoom === level ? 'border-accent/60 text-accent' : 'border-edge-subtle text-text-secondary',
              selected && 'text-text-muted',
            )}
          >
            {level}
          </button>
        ))}
        <span className="text-text-muted">{selected ? 'event · selected session expanded, others compressed' : zoom === 'workstream' ? 'bundles where the work first fans out' : 'one lane per session'}</span>
      </div>
      <fieldset className="flex flex-wrap items-center gap-1" aria-label="Event filters">
        <legend className="sr-only">Event filters</legend>
        <span className="td-legend">events</span>
        {JOURNEY_EVENT_KINDS.filter((kind) => (kindCounts.get(kind) ?? 0) > 0).map((kind) => (
          <label key={kind} className="flex min-h-8 items-center gap-1 text-text-secondary">
            <input
              type="checkbox"
              className="td-check"
              checked={!hiddenKinds.has(kind)}
              onChange={() => onToggleKind(kind)}
              aria-label={`Show ${glyphLabel(kind)} events`}
            />
            <span>{glyphLabel(kind)}</span>
            <span className="td-value text-text-muted" data-cell="numeric">{kindCounts.get(kind)}</span>
          </label>
        ))}
      </fieldset>
      <span className="td-value text-text-muted" data-scene-counts>
        {counts.eventsDrawn} drawn · {counts.eventsFiltered} filtered · {counts.eventsWithheld} withheld · {counts.eventsCulled} outside window · {counts.eventsFolded} folded
      </span>
    </div>
  );
}

function TemporalBoundary({
  pending,
  result,
  children,
}: {
  pending: boolean;
  result: EnvelopeResult<LoomTemporalPayloadV1> | undefined;
  children: (envelope: DashboardEnvelopeV1<LoomTemporalPayloadV1>) => ReactNode;
}) {
  const plate = (kind: DomainStateKind, detail: string) => (
    <div className="flex min-h-0 flex-1 items-center justify-center p-8">
      <StateChip kind={kind} detail={detail} />
    </div>
  );
  if (pending) return plate('loading', 'reading Loom temporal authorities');
  if (!result) return plate('offline', 'Loom temporal response unavailable');
  if (result.outcome === 'transport') {
    return plate(result.state, result.detail ?? 'Loom temporal response unavailable');
  }
  return children(result.envelope);
}

function sourceDetail(source: LoomSourceStatusV1): string {
  if (source.required_authority) return source.required_authority;
  const parts = [
    source.authority,
    source.providers.length > 0 ? `providers: ${source.providers.join(', ')}` : null,
    source.reason,
    source.coverage.eligible != null &&
    source.coverage.matched != null &&
    source.coverage.omitted != null
      ? `${source.coverage.matched}/${source.coverage.eligible} ${source.coverage.unit ?? 'items'} matched · ${source.coverage.omitted} omitted`
      : null,
    source.coverage.reason,
  ];
  return parts.filter((part): part is string => part != null && part.length > 0).join(' · ');
}

function coverageDetail(envelope: DashboardEnvelopeV1<LoomTemporalPayloadV1>): string {
  const { coverage } = envelope;
  const denominator =
    coverage.denominator == null
      ? 'denominator unrecorded'
      : `${formatCount(coverage.denominator)} ${coverage.unit ?? 'items'} eligible`;
  return `${coverage.completeness} coverage · ${formatCount(coverage.examined)} examined · ${formatCount(coverage.matched)} matched · ${denominator}`;
}

function freshnessKind(
  state: DashboardEnvelopeV1<LoomTemporalPayloadV1>['freshness']['state'],
): 'ready' | 'stale' | 'unknown' | 'unsupported' {
  switch (state) {
    case 'fresh':
      return 'ready';
    case 'stale':
      return 'stale';
    case 'unknown':
    case 'absent':
      return 'unknown';
    case 'unsupported':
      return 'unsupported';
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/** The field, printed. A reader cannot infer from the picture what each axis
 * and line style encodes, so both are stated in the same words the layout
 * uses, and the counts come from the same model, not a second tally. */
function FieldCaption({
  projection,
  model,
}: {
  projection: JourneyProjection;
  model: TemporalSceneModel;
}) {
  const { stats } = projection;
  return (
    <div className="flex flex-col gap-1.5">
      <Legend>time left to right · recorded hierarchy down · thickness = messages</Legend>
      <div className="flex flex-wrap border-y border-edge-subtle bg-surface-1">
        {stats.providers.map((provider) => (
          <div
            key={provider.id}
            className="min-w-0 flex-1 basis-28 border-l border-edge-subtle px-2.5 py-1.5 first:border-l-0"
          >
            <span className="td-legend text-text-secondary">{provider.id}</span>
            <div className="td-value text-xs text-text-primary">
              {provider.lanes} {provider.lanes === 1 ? 'session' : 'sessions'}
            </div>
            <span className="text-3xs text-text-muted">{formatCount(provider.messages)} messages</span>
          </div>
        ))}
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        Each lane is one session: horizontal position is its recorded start on the
        printed axis (exact); lanes sit under their provider rail, and a subagent
        sits under its recorded parent in preorder. Thickness is the session&apos;s
        message count on a log scale. A lane drawn solid to its right edge has a
        served end; a lane ending in a dashed segment ends at its last message
        observation; a lane ending in a dotted stub has no measured extent, {' '}
        <span className="text-text-secondary">
          {stats.openEnded} of {stats.lanes} sessions have no recorded end or later
          message observation.
        </span>{' '}
        {stats.hollow > 0
          ? `${stats.hollow} ${stats.hollow === 1 ? 'is a session the store reports' : 'are sessions the store reports'} at zero messages, a reading, not a gap. `
          : ''}
        {stats.undated > 0
          ? `${stats.undated} ${stats.undated === 1 ? 'row' : 'rows'} carried no usable start time and ${stats.undated === 1 ? 'is' : 'are'} not on the field at all. `
          : ''}
        A fork leaves a parent lane at the child&apos;s recorded start and is drawn
        only for a recorded parent identity in this page; a join is inferred only
        from a child ending inside its parent&apos;s measured extent, and handoffs
        and results are unavailable in this read. {model.counts.lanesCollapsed > 0
          ? `${model.counts.lanesCollapsed} ${model.counts.lanesCollapsed === 1 ? 'branch is' : 'branches are'} collapsed into bundles whose counts include every descendant session. `
          : ''}
        Lane spacing is presentation only.
      </p>
    </div>
  );
}

function fieldDescription(projection: JourneyProjection, model: TemporalSceneModel): string {
  const providers = projection.stats.providers
    .map((provider) => `${provider.lanes} on ${provider.id}`)
    .join(', ');
  return `Temporal execution field: ${projection.stats.lanes} sessions as horizontal lanes, time running left to right, hierarchy down by provider rail and recorded parent; providers ${providers || 'none'}. ${projection.stats.openEnded} have no recorded extent and are drawn open. ${projection.relations.filter((relation) => relation.kind === 'spawn').length} recorded forks are drawn at the child's start and ${projection.relations.filter((relation) => relation.kind === 'rejoin').length} inferred joins where a child ends inside its parent; handoff and result remain unavailable. ${model.counts.lanesCollapsed} branches are collapsed into bundles. The branch navigator table below is the accessible equivalent.`;
}

/** Composed empty state: the frame stays, so an empty field reads as an
 * answered question rather than a broken page. */
function EmptyField({ undated, rows }: { undated: number; rows: number }) {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-8">
      <div className="flex max-w-sm flex-col items-center gap-3 text-center">
        <span
          aria-hidden
          className="flex size-10 items-center justify-center border border-edge-subtle bg-surface-1 text-text-muted"
        >
          <Waypoints size={18} />
        </span>
        <h2 className="text-sm font-semibold tracking-tight">No thread to weave</h2>
        <p className="text-xs leading-relaxed text-text-muted">
          {rows === 0
            ? 'The session store answered and holds no sessions in this scope.'
            : `The store returned ${rows} ${rows === 1 ? 'session' : 'sessions'}, but ${undated} carried no usable start time, there is no honest position on the time axis for a session that never recorded when it began.`}{' '}
          <span className="text-text-secondary">
            Lanes appear as soon as the store records a start time.
          </span>
        </p>
      </div>
    </div>
  );
}
