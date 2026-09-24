import { useId, useMemo, useState } from 'react';
import { EvidenceGrade } from '../../../ui/EvidenceGrade.tsx';
import { cn } from '../../../ui/cn.ts';
import { hatchBackground, laneTreatment } from '../workLaneTreatment.ts';
import { laneReading } from '../workLaneModel.ts';
import { SWIMLANE_GEOMETRY, workSwimlaneLayout, type SwimlaneKey } from '../workSwimlaneLayout.ts';
import type { DagFieldProps } from './dagVariant.ts';

/**
 * The swimlane DAG: lanes by milestone or by the latest recorded holder, the
 * dependency depth across, tasks as compact 44px plates and dependencies as
 * routed hairlines. The deepest declared chain is drawn in cyan as a measured
 * signal and named for what it is: depth, not effort.
 */
export function DagSwimlaneField({
  layout,
  reading,
  tasks,
  selected,
  isolation,
  zoom,
  card,
}: DagFieldProps) {
  const [laneKey, setLaneKey] = useState<SwimlaneKey>('milestone');
  const lanes = useMemo(
    () => workSwimlaneLayout(layout, reading, tasks, laneKey),
    [layout, reading, tasks, laneKey],
  );
  const g = SWIMLANE_GEOMETRY;
  const markers = useId();
  const depths = [...new Set(lanes.plates.map((plate) => plate.depth))].sort((a, b) => a - b);

  return (
    <div className="relative z-[1] flex min-w-0 flex-col" data-work-dag-swimlane={lanes.lanes.length}>
      <div className="sticky left-0 flex flex-wrap items-center gap-x-3 gap-y-1 px-2 pt-1.5">
        <span className="td-legend">lanes</span>
        <div role="radiogroup" aria-label="Swimlane key" className="flex items-center border border-edge-subtle rounded-[var(--radius-panel)]">
          {(
            [
              ['milestone', 'milestone'],
              ['holder', 'latest holder'],
            ] as const
          ).map(([value, text]) => (
            <button
              key={value}
              type="button"
              role="radio"
              aria-checked={laneKey === value}
              onClick={() => setLaneKey(value)}
              data-work-swimlane-key={value}
              className={cn(
                'td-hit px-2 text-2xs',
                laneKey === value ? 'bg-surface-2 text-text-primary shadow-[inset_0_-2px_0_var(--raw-accent)]' : 'text-text-muted hover:text-text-primary',
              )}
            >
              {text}
            </button>
          ))}
        </div>
        <EvidenceGrade grade={laneKey === 'milestone' ? 'exact' : 'explicit'} source={laneKey === 'milestone' ? 'GRAPH' : 'HANDOFF'} />
        <span className="flex items-center gap-1.5 text-3xs text-text-muted" data-work-swimlane-chain={lanes.chain.depth}>
          <span aria-hidden className="h-[2px] w-5 bg-accent" />
          cyan · deepest declared chain · {lanes.chain.depth} deep · unweighted
        </span>
      </div>
      <div className="relative m-2 shrink-0" style={{ width: lanes.width * zoom, height: lanes.height * zoom }}>
        <div className="absolute left-0 top-0 origin-top-left" style={{ width: lanes.width, height: lanes.height, transform: `scale(${zoom})` }}>
          <svg aria-hidden className="pointer-events-none absolute inset-0" width={lanes.width} height={lanes.height}>
            <defs>
              <marker id={`${markers}-edge`} viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto">
                <path d="M 0 0 L 8 4 L 0 8 z" fill="var(--raw-graph-edge)" />
              </marker>
              <marker id={`${markers}-lit`} viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto">
                <path d="M 0 0 L 8 4 L 0 8 z" fill="var(--raw-graph-accent)" />
              </marker>
            </defs>
            {lanes.lanes.map((lane, index) => (
              <rect key={lane.key} x={0} y={lane.y} width={lanes.width} height={lane.height} fill="var(--raw-graph-dim)" fillOpacity={index % 2 === 0 ? 0.2 : 0.07} />
            ))}
            {lanes.lanes.map((lane) => (
              <line key={`${lane.key}-rule`} x1={0} x2={lanes.width} y1={lane.y} y2={lane.y} stroke="var(--raw-graph-edge)" strokeOpacity={0.45} />
            ))}
            {depths.map((depth, column) => {
              const x = g.labelWidth + column * (g.plateWidth + g.columnGap);
              return (
                <g key={depth}>
                  <line x1={x - g.columnGap / 2} x2={x - g.columnGap / 2} y1={g.axis - 6} y2={lanes.height} stroke="var(--raw-graph-edge)" strokeOpacity={0.22} strokeDasharray="1 5" />
                  <text x={x} y={g.axis - 10} fill="var(--raw-graph-text)" fillOpacity={0.7} fontSize={10} fontFamily="var(--font-mono)" letterSpacing="0.12em">
                    STRATUM {String(depth).padStart(2, '0')}
                  </text>
                </g>
              );
            })}
            {lanes.edges.map((edge) => {
              const onChain = lanes.chain.edges.has(edge.id);
              const lit = isolation?.edges.has(edge.id) ?? false;
              const dimmed = isolation !== null && !lit;
              const tone = edge.climb ? 'var(--raw-state-conflicting)' : onChain || lit ? 'var(--raw-graph-accent)' : 'var(--raw-graph-edge)';
              return (
                <path
                  key={edge.id}
                  d={edge.path}
                  fill="none"
                  stroke={tone}
                  strokeWidth={onChain ? 1.8 : lit ? 1.4 : 1}
                  strokeDasharray={edge.kind === 'informational' ? '6 4' : edge.kind === 'causal' ? '2 3' : undefined}
                  markerEnd={edge.climb ? undefined : `url(#${markers}-${onChain || lit ? 'lit' : 'edge'})`}
                  opacity={dimmed ? 0.2 : onChain ? 1 : 0.8}
                  data-work-swimlane-edge={edge.id}
                  data-work-swimlane-chain-edge={onChain ? 'true' : undefined}
                />
              );
            })}
          </svg>
          {lanes.lanes.map((lane) => (
            <div
              key={lane.key}
              className="absolute left-0 flex flex-col justify-center gap-0.5 px-2.5"
              style={{ top: lane.y, height: lane.height, width: g.labelWidth - 12 }}
              data-work-swimlane-lane={lane.key}
            >
              <span className="td-value truncate text-2xs text-text-primary" title={lane.label}>
                {lane.label}
              </span>
              <span className="td-legend">
                {lane.taskIds.length} {lane.taskIds.length === 1 ? 'task' : 'tasks'}
              </span>
            </div>
          ))}
          {lanes.plates.map((plate) => {
            const task = tasks.get(plate.taskId);
            if (task === undefined) return null;
            const wiring = card(plate.taskId);
            const lane = laneReading(task.lane);
            const treatment = laneTreatment(task.lane);
            const isSelected = selected === plate.taskId;
            const onChain = lanes.chain.tasks.has(plate.taskId);
            const dimmed = isolation !== null && !isolation.tasks.has(plate.taskId);
            return (
              <button
                key={plate.taskId}
                ref={wiring.ref}
                type="button"
                aria-label={`${task.title}, ${task.task_id}, ${lane.label.toLowerCase()}, stratum ${plate.depth}${onChain ? ', on the deepest declared chain' : ''}`}
                aria-pressed={isSelected}
                tabIndex={wiring.tabIndex}
                onClick={wiring.onClick}
                onFocus={wiring.onFocus}
                onBlur={wiring.onBlur}
                onPointerEnter={wiring.onPointerEnter}
                onKeyDown={wiring.onKeyDown}
                className={cn(
                  'td-raised absolute flex flex-col justify-center gap-0.5 border px-2 text-left rounded-[var(--radius-panel)] transition-opacity duration-[var(--dur-state)]',
                  'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent',
                  isSelected ? 'border-accent' : onChain ? 'border-accent/60' : 'border-edge-strong hover:border-text-muted',
                  treatment.dashed && !isSelected && 'border-dashed',
                  dimmed && 'opacity-40',
                )}
                style={{
                  left: plate.x,
                  top: plate.y,
                  width: plate.width,
                  height: plate.height,
                  backgroundImage: treatment.hatched && treatment.tone !== null ? hatchBackground(treatment.tone) : undefined,
                }}
                data-work-task={plate.taskId}
                data-work-lane-family={treatment.family}
                data-work-swimlane-chain={onChain ? 'true' : undefined}
              >
                {isSelected ? <span aria-hidden className="absolute inset-y-0 left-0 w-[3px] bg-accent" /> : null}
                <span className="flex min-w-0 items-center justify-between gap-2">
                  <span className="td-value truncate text-2xs text-text-secondary">{task.task_id}</span>
                  <span
                    aria-hidden
                    title={lane.label}
                    className={cn('size-2 shrink-0', lane.swatch ?? 'border border-dashed border-text-muted bg-transparent')}
                  />
                </span>
                <span className={cn('truncate text-sm leading-tight', treatment.family === 'disconnected' ? 'text-text-secondary' : 'text-text-primary')}>
                  {task.title}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
