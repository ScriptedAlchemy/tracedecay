import { cn } from '../../../ui/cn.ts';
import { hatchBackground, laneTreatment } from '../workLaneTreatment.ts';
import { laneReading } from '../workLaneModel.ts';
import type { WorkDagLayoutNode } from '../workDagLayout.ts';
import type { WorkTaskView } from '../workProductView.ts';
import type { WorkDagReading } from '../workViewsModel.ts';
import type { CardWiring, DagFieldProps } from './dagVariant.ts';
import { RelationLayer } from './DagRelationLayer.tsx';

/**
 * The fitted board: the layered layout unchanged, framed by the field. The
 * board fits the graph's width on load and on resize, never above 100% so the
 * 11px identity and 13px title stay the sizes they were set at, and centres it
 * on both axes, so a three-card graph sits in the middle of its aperture
 * rather than hugging the corner. Cards wear their lane's typed-state family:
 * hatched when degraded, dashed and muted when disconnected.
 */
export function DagFittedField({
  layout,
  reading,
  tasks,
  selected,
  isolation,
  critical,
  showLabels,
  zoom,
  card,
}: DagFieldProps) {
  return (
    <div
      className="relative z-[1] m-auto shrink-0 p-6"
      style={{ width: layout.width * zoom + 48, height: layout.height * zoom + 48 }}
      data-work-dag-fitted
    >
      <div
        className="absolute left-6 top-6 origin-top-left"
        style={{ width: layout.width, height: layout.height, transform: `scale(${zoom})` }}
      >
        <RelationLayer layout={layout} isolation={isolation} critical={critical} />
        {layout.nodes.map((node) => {
          const task = tasks.get(node.taskId);
          if (task === undefined) return null;
          return (
            <FittedCard
              key={node.taskId}
              node={node}
              task={task}
              reading={reading}
              selected={selected === node.taskId}
              dimmed={isolation !== null && !isolation.tasks.has(node.taskId)}
              onCritical={critical?.tasks.has(node.taskId) ?? false}
              showLabels={showLabels}
              wiring={card(node.taskId)}
            />
          );
        })}
      </div>
    </div>
  );
}

export function FittedCard({
  node,
  task,
  reading,
  selected,
  dimmed,
  onCritical,
  showLabels,
  wiring,
}: {
  node: WorkDagLayoutNode;
  task: WorkTaskView;
  reading: WorkDagReading;
  selected: boolean;
  dimmed: boolean;
  onCritical: boolean;
  showLabels: boolean;
  wiring: CardWiring;
}) {
  const lane = laneReading(task.lane);
  const treatment = laneTreatment(task.lane);
  const source = reading.nodes.get(task.task_id);
  const inbound = source?.dependencies.length ?? 0;
  const outbound = source?.dependents.length ?? 0;
  const label = [
    task.title,
    task.task_id,
    lane.label.toLowerCase(),
    `depth ${node.depth}`,
    `${inbound} gating in`,
    `${outbound} gating out`,
    node.cyclic ? 'in a declared dependency cycle' : null,
    onCritical ? 'on the effort-weighted critical path' : null,
  ]
    .filter((part) => part !== null)
    .join(', ');
  return (
    <button
      ref={wiring.ref}
      type="button"
      aria-label={label}
      aria-pressed={selected}
      tabIndex={wiring.tabIndex}
      onClick={wiring.onClick}
      onFocus={wiring.onFocus}
      onBlur={wiring.onBlur}
      onPointerEnter={wiring.onPointerEnter}
      onKeyDown={wiring.onKeyDown}
      className={cn(
        'absolute flex flex-col gap-0.5 border px-2.5 py-1.5 text-left rounded-[var(--radius-panel)]',
        'td-raised transition-opacity duration-[var(--dur-state)]',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent',
        selected ? 'border-accent' : 'border-edge-strong hover:border-text-muted',
        treatment.dashed && !selected && 'border-dashed',
        node.cyclic && !selected && 'border-state-conflicting/70',
        dimmed && 'opacity-40',
      )}
      style={{
        left: node.x,
        top: node.y,
        width: node.width,
        height: node.height,
        backgroundImage: treatment.hatched && treatment.tone !== null ? hatchBackground(treatment.tone) : undefined,
      }}
      data-work-task={task.task_id}
      data-work-depth={node.depth}
      data-work-dag-card={dimmed ? 'dimmed' : 'lit'}
      data-work-lane-family={treatment.family}
      data-work-dag-critical-node={onCritical ? 'true' : undefined}
    >
      {selected ? <span aria-hidden className="absolute inset-y-0 left-0 w-[3px] bg-accent" /> : null}
      {onCritical ? <span aria-hidden className="absolute inset-x-0 top-0 h-[2px] bg-alert" /> : null}
      <span className={cn('flex min-w-0 items-center justify-between gap-2', treatment.family === 'disconnected' && 'opacity-75')}>
        <span className="td-value truncate text-2xs text-text-secondary">{task.task_id}</span>
        <span className="flex shrink-0 items-center gap-1">
          <span
            aria-hidden
            className={cn('size-2', lane.swatch ?? 'border border-dashed border-text-muted bg-transparent')}
            style={treatment.hatched && treatment.tone !== null ? { backgroundImage: hatchBackground(treatment.tone, 80) } : undefined}
          />
          <span className="td-legend text-text-secondary">{lane.label}</span>
        </span>
      </span>
      {showLabels ? (
        <span
          className={cn(
            'line-clamp-2 min-w-0 text-sm leading-[1.2] text-text-primary',
            treatment.family === 'disconnected' && 'text-text-secondary',
          )}
        >
          {task.title}
        </span>
      ) : null}
      {showLabels ? (
        <span className="mt-auto flex min-w-0 items-center gap-2 text-2xs leading-none text-text-muted">
          <span className="td-value min-w-0 truncate text-2xs text-text-muted" title={task.hierarchy.milestone_id}>
            {task.hierarchy.milestone_id}
          </span>
          <span aria-hidden className="td-rule min-w-1" />
          <span className="td-value shrink-0 text-2xs" data-cell="numeric">
            e{task.effort}
          </span>
          <span className="td-value shrink-0 text-2xs" data-cell="numeric" title={`${inbound} gating in · ${outbound} gating out`}>
            ↑{inbound} ↓{outbound}
          </span>
        </span>
      ) : null}
    </button>
  );
}
