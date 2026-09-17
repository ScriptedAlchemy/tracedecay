import { useMemo, useState } from 'react';
import { cn } from '../../ui/cn';
import { Legend } from '../../ui/instrument.tsx';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import type {
  EvidenceGrade,
  JourneyLane,
  JourneyProjection,
  TemporalSceneModel,
} from '../../viz/temporal/types.ts';
import { formatDurationSeconds, formatMoment } from './tracks.ts';

/**
 * The field's exact tree: every loaded session as a real table row, in the
 * same deterministic order the layout allocates lanes, with the same
 * selection, the same collapse state and the same evidence words.
 *
 * It is also the branch navigator. Search narrows the rows; the toggle column
 * collapses or expands a subtree; the path-to-root of the selected session is
 * printed so a reader never has to infer ancestry from indentation alone.
 */
export function BranchNavigator({
  projection,
  model,
  selectedLaneId,
  onSelect,
  onToggle,
}: {
  projection: JourneyProjection;
  model: TemporalSceneModel;
  selectedLaneId: string | null;
  onSelect: (id: string | null) => void;
  onToggle: (id: string) => void;
}) {
  const [query, setQuery] = useState('');
  const laneById = useMemo(
    () => new Map(projection.lanes.map((lane) => [lane.id, lane])),
    [projection.lanes],
  );
  const descendants = useMemo(() => countDescendants(projection.lanes), [projection.lanes]);
  const visibleIds = useMemo(() => new Set(model.lanes.map((lane) => lane.id)), [model.lanes]);
  const collapsedIds = useMemo(
    () => new Set(model.lanes.filter((lane) => lane.kind === 'bundle').map((lane) => lane.id)),
    [model.lanes],
  );
  const needle = query.trim().toLowerCase();
  const rows = projection.lanes.filter((lane) => {
    if (needle.length === 0) return true;
    return (
      lane.label.toLowerCase().includes(needle) ||
      lane.sessionId.toLowerCase().includes(needle) ||
      (lane.agent?.toLowerCase().includes(needle) ?? false) ||
      lane.provider.toLowerCase().includes(needle)
    );
  });
  const pathToRoot = selectedLaneId ? ancestorsOf(selectedLaneId, laneById) : [];
  const unresolved = projection.gaps.filter((gap) => gap.laneId != null).length;

  return (
    <section aria-label="Branch navigator" className="flex min-w-0 flex-col gap-1.5">
      <Legend
        trailing={
          <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
            {rows.length} / {projection.lanes.length} sessions
          </span>
        }
      >
        branches · layout order
      </Legend>
      <div className="flex flex-wrap items-center gap-2">
        <label className="flex min-h-8 flex-1 items-center gap-1.5 text-3xs text-text-muted">
          <span className="td-legend">search</span>
          <input
            type="search"
            aria-label="Search branches"
            value={query}
            onChange={(event) => setQuery(event.currentTarget.value)}
            placeholder="session, agent, host"
            className="min-h-8 min-w-0 flex-1 border border-edge-subtle bg-surface-1 px-1.5 text-2xs text-text-primary"
          />
        </label>
        <span className="text-3xs text-text-muted">
          {model.counts.lanesCollapsed} collapsed · {unresolved} lanes with evidence gaps
        </span>
      </div>
      {pathToRoot.length > 0 && selectedLaneId ? (
        <p className="flex flex-wrap items-center gap-1 text-3xs text-text-muted" aria-label="Path to root">
          <span className="td-legend">path to root</span>
          {[...pathToRoot].reverse().map((id) => (
            <button
              key={id}
              type="button"
              className="td-hit border border-edge-subtle px-1 text-text-secondary"
              onClick={() => onSelect(id)}
            >
              {laneById.get(id)?.label ?? id}
            </button>
          ))}
          <span aria-hidden>→</span>
          <span className="text-text-primary">{laneById.get(selectedLaneId)?.label ?? selectedLaneId}</span>
        </p>
      ) : null}
      <div
        role="region"
        aria-label="Sessions table"
        tabIndex={0}
        className="max-h-80 overflow-auto border border-edge-subtle"
      >
        <table className="w-full border-collapse text-2xs">
          <caption className="sr-only">
            Every loaded session in the order the field allocates lanes, with
            its recorded parent depth, agent label, host, start, message count,
            measured extent and branch state.
          </caption>
          <thead className="sticky top-0 bg-surface-2">
            <tr className="text-left text-text-secondary">
              <th scope="col" className="px-2 py-1 font-medium">Session</th>
              <th scope="col" className="px-2 py-1 font-medium">Agent</th>
              <th scope="col" className="px-2 py-1 font-medium">Host</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Started</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Messages</th>
              <th scope="col" className="px-2 py-1 font-medium">Extent</th>
              <th scope="col" className="px-2 py-1 font-medium">Branch</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((lane) => {
              const children = descendants.get(lane.id) ?? 0;
              const hidden = !visibleIds.has(lane.id);
              const collapsed = collapsedIds.has(lane.id);
              const selected = selectedLaneId === lane.id;
              return (
                <tr
                  key={lane.id}
                  data-navigator-lane={lane.id}
                  data-navigator-hidden={hidden ? 'true' : undefined}
                  className={cn(
                    'border-t border-edge-subtle',
                    selected && 'bg-accent/10',
                    hidden && 'text-text-muted',
                  )}
                >
                  <td className="max-w-0 px-2 py-1">
                    <button
                      type="button"
                      onClick={() => onSelect(selected ? null : lane.id)}
                      aria-pressed={selected}
                      aria-label={`Select session ${lane.label}`}
                      className="flex min-h-[var(--touch-target-min)] w-full min-w-[var(--touch-target-min)] items-center gap-1.5 text-left"
                      style={{ paddingLeft: `${Math.min(lane.depth, 6) * 10}px` }}
                    >
                      <span
                        aria-hidden
                        style={kindColorVars(lane.provider)}
                        className="size-1.5 shrink-0 bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
                      />
                      <span className="truncate text-text-primary">{lane.label}</span>
                      {lane.isSubagent ? (
                        <span className="td-legend shrink-0 text-text-muted">sub</span>
                      ) : null}
                      {hidden ? (
                        <span className="td-legend shrink-0 text-text-muted">in bundle</span>
                      ) : null}
                    </button>
                  </td>
                  <td className="px-2 py-1 text-text-secondary">{lane.agent ?? 'unrecorded'}</td>
                  <td className="px-2 py-1 text-text-secondary">{lane.provider}</td>
                  <td className="px-2 py-1 text-right text-text-muted tabular-nums" data-cell="numeric">
                    {formatMoment(lane.start)}
                  </td>
                  <td className="px-2 py-1 text-right text-text-secondary tabular-nums" data-cell="numeric">
                    {lane.messages.toLocaleString()}
                  </td>
                  <td className="px-2 py-1 text-text-muted">
                    <span data-grade={extentGrade(lane)}>{extentText(lane)}</span>
                  </td>
                  <td className="px-2 py-1">
                    {children > 0 ? (
                      <button
                        type="button"
                        aria-label={`${collapsed ? 'Expand' : 'Collapse'} ${lane.label} in navigator`}
                        aria-expanded={!collapsed}
                        onClick={() => onToggle(lane.id)}
                        className="td-hit td-value text-3xs text-text-secondary"
                      >
                        {collapsed ? '+' : '−'}{children}
                      </button>
                    ) : (
                      <span className="text-3xs text-text-muted">—</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function countDescendants(lanes: readonly JourneyLane[]): Map<string, number> {
  const counts = new Map<string, number>();
  const byId = new Map(lanes.map((lane) => [lane.id, lane]));
  for (const lane of lanes) {
    for (const ancestor of ancestorsOf(lane.id, byId)) {
      counts.set(ancestor, (counts.get(ancestor) ?? 0) + 1);
    }
  }
  return counts;
}

function ancestorsOf(id: string, byId: ReadonlyMap<string, JourneyLane>): string[] {
  const out: string[] = [];
  const seen = new Set<string>([id]);
  let parent = byId.get(id)?.parentId ?? null;
  while (parent != null && !seen.has(parent)) {
    seen.add(parent);
    out.push(parent);
    parent = byId.get(parent)?.parentId ?? null;
  }
  return out;
}

function extentGrade(lane: JourneyLane): EvidenceGrade {
  switch (lane.endSource) {
    case 'session_end':
      return 'exact';
    case 'last_message':
      return 'inferred';
    case null:
      return 'unavailable';
    default: {
      const exhaustive: never = lane.endSource;
      return exhaustive;
    }
  }
}

function extentText(lane: JourneyLane): string {
  switch (lane.endSource) {
    case 'session_end':
      return formatDurationSeconds((lane.end ?? lane.start) - lane.start);
    case 'last_message':
      return `${formatDurationSeconds((lane.end ?? lane.start) - lane.start)} · last message`;
    case null:
      return 'unrecorded';
    default: {
      const exhaustive: never = lane.endSource;
      return exhaustive;
    }
  }
}
