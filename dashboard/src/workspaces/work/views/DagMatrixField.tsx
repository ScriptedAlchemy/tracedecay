import { useId, useMemo, useState, type PointerEvent } from 'react';
import { cn } from '../../../ui/cn.ts';
import { hatchBackground, laneTreatment } from '../workLaneTreatment.ts';
import { laneReading } from '../workLaneModel.ts';
import { dsmStep, workDsm } from '../workDsmModel.ts';
import type { DagFieldProps } from './dagField.ts';

/**
 * The dependency structure matrix: every task on both axes in the layered
 * reading order, one mark per declared relation. Dense by construction, so the
 * rows are a single listbox, one tab stop with arrow keys moving the active
 * row, rather than a column of undersized buttons. Hover over a cell or a row
 * inspects that row's task; click or Enter selects it.
 */

const CELL = 26;
const HEADER = 288;
const STRIP = 16;
const AXIS = 22;

export function DagMatrixField({
  layout,
  tasks,
  selected,
  inspected,
  isolation,
  critical,
  onSelect,
  onInspect,
}: DagFieldProps) {
  const dsm = useMemo(() => workDsm(layout), [layout]);
  const listId = useId();
  const [hover, setHover] = useState<{ row: number; column: number } | null>(null);
  const [focused, setFocused] = useState(false);
  const count = dsm.order.length;
  const activeRow = inspected !== null ? (dsm.index.get(inspected) ?? null) : null;
  const top = AXIS + STRIP;
  const width = HEADER + count * CELL + 12;
  const height = top + count * CELL + 64;
  const optionId = (row: number) => `${listId}-row-${row}`;
  const cellAt = new Map(dsm.cells.map((cell) => [`${cell.row}:${cell.column}`, cell]));
  const hovered = hover === null ? undefined : cellAt.get(`${hover.row}:${hover.column}`);
  const crossRow = hover?.row ?? activeRow;
  const crossColumn = hover?.column ?? activeRow;

  const onGridPointer = (event: PointerEvent<SVGSVGElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    const scale = box.width / (count * CELL);
    const column = Math.floor((event.clientX - box.left) / (CELL * scale));
    const row = Math.floor((event.clientY - box.top) / (CELL * scale));
    if (row < 0 || row >= count || column < 0 || column >= count) return;
    setHover({ row, column });
    onInspect(dsm.order[row] ?? null);
  };

  return (
    <div
      role="listbox"
      aria-label="Dependency structure matrix, one row per task"
      aria-activedescendant={activeRow === null ? undefined : optionId(activeRow)}
      tabIndex={0}
      className="relative z-[1] m-2 shrink-0 outline-offset-2"
      style={{ width, height }}
      data-work-dag-matrix={count}
      data-work-dag-matrix-back={dsm.backEdges}
      onFocus={() => {
        setFocused(true);
        if (inspected === null) onInspect(selected ?? dsm.order[0] ?? null);
      }}
      onBlur={() => {
        setFocused(false);
        onInspect(null);
      }}
      onPointerLeave={() => {
        setHover(null);
        onInspect(null);
      }}
      onKeyDown={(event) => {
        if ((event.key === 'Enter' || event.key === ' ') && activeRow !== null) {
          event.preventDefault();
          onSelect(dsm.order[activeRow]!);
          return;
        }
        const next = dsmStep(event.key, activeRow, count);
        if (next === null) return;
        event.preventDefault();
        onInspect(dsm.order[next] ?? null);
      }}
    >
      <span className="td-legend absolute left-2" style={{ top: 4 }}>
        row needs column · {count} × {count} · {dsm.cells.length} relations · {dsm.backEdges} back-edges
      </span>
      {/* Column axis: the index and, beneath it, the compact node strip, one
        * typed-state swatch per task in reading order. */}
      {dsm.order.map((taskId, column) => {
        const task = tasks.get(taskId);
        const treatment = task === undefined ? null : laneTreatment(task.lane);
        const lane = task === undefined ? null : laneReading(task.lane);
        return (
          <span key={taskId} aria-hidden>
            <span
              className={cn('td-value absolute text-center text-3xs', column === crossColumn ? 'text-accent' : 'text-text-muted')}
              style={{ left: HEADER + column * CELL, top: AXIS - 4, width: CELL }}
            >
              {String(column + 1).padStart(2, '0')}
            </span>
            <span
              className={cn('absolute', lane?.swatch ?? 'border border-dashed border-text-muted')}
              style={{
                left: HEADER + column * CELL + 7,
                top: AXIS + 9,
                width: CELL - 14,
                height: 5,
                backgroundImage: treatment?.hatched && treatment.tone !== null ? hatchBackground(treatment.tone, 90) : undefined,
              }}
            />
          </span>
        );
      })}
      {dsm.order.map((taskId, row) => {
        const task = tasks.get(taskId);
        if (task === undefined) return null;
        const lane = laneReading(task.lane);
        const treatment = laneTreatment(task.lane);
        const isSelected = selected === taskId;
        const isActive = activeRow === row;
        const dimmed = isolation !== null && !isolation.tasks.has(taskId);
        return (
          <div
            key={taskId}
            id={optionId(row)}
            role="option"
            aria-selected={isSelected}
            aria-label={`${String(row + 1).padStart(2, '0')}, ${task.title}, ${taskId}, ${lane.label.toLowerCase()}`}
            tabIndex={-1}
            className={cn(
              'absolute flex cursor-pointer items-center gap-2 pl-2 pr-3 text-2xs transition-opacity duration-[var(--dur-state)]',
              isActive ? 'bg-surface-2' : 'hover:bg-surface-2/50',
              isActive && focused && 'outline outline-2 -outline-offset-2 outline-accent',
              dimmed && 'opacity-40',
            )}
            style={{ left: 0, top: top + row * CELL, width: HEADER - 4, height: CELL }}
            onPointerEnter={() => onInspect(taskId)}
            onClick={() => onSelect(taskId)}
            data-work-task={taskId}
            data-work-lane-family={treatment.family}
          >
            {isSelected ? <span aria-hidden className="absolute inset-y-0 left-0 w-[3px] bg-accent" /> : null}
            <span className={cn('td-value w-5 shrink-0 text-3xs', row === crossRow ? 'text-accent' : 'text-text-muted')}>
              {String(row + 1).padStart(2, '0')}
            </span>
            <span
              aria-hidden
              className={cn('size-2 shrink-0', lane.swatch ?? 'border border-dashed border-text-muted bg-transparent')}
              style={treatment.hatched && treatment.tone !== null ? { backgroundImage: hatchBackground(treatment.tone, 90) } : undefined}
            />
            <span className={cn('td-value min-w-0 flex-1 truncate', treatment.family === 'disconnected' ? 'text-text-muted' : 'text-text-primary')}>
              {taskId}
            </span>
            <span className="td-legend shrink-0">{lane.label}</span>
          </div>
        );
      })}
      <svg
        aria-hidden
        className="absolute"
        style={{ left: HEADER, top }}
        width={count * CELL}
        height={count * CELL}
        onPointerMove={onGridPointer}
        onClick={() => {
          if (hover !== null) onSelect(dsm.order[hover.row]!);
        }}
        data-work-dag-matrix-grid
      >
        <path d={`M0 0 H${count * CELL} V${count * CELL} Z`} fill="var(--raw-state-conflicting)" fillOpacity={0.04} />
        {dsm.blocks.map((block) => (
          <rect
            key={block.depth}
            x={block.start * CELL}
            y={block.start * CELL}
            width={(block.end - block.start + 1) * CELL}
            height={(block.end - block.start + 1) * CELL}
            fill="var(--raw-graph-accent)"
            fillOpacity={0.05}
            stroke="var(--raw-graph-edge)"
            strokeOpacity={0.6}
          />
        ))}
        {Array.from({ length: count + 1 }, (_, line) => (
          <g key={line}>
            <line x1={0} x2={count * CELL} y1={line * CELL} y2={line * CELL} stroke="var(--raw-graph-edge)" strokeOpacity={0.18} />
            <line y1={0} y2={count * CELL} x1={line * CELL} x2={line * CELL} stroke="var(--raw-graph-edge)" strokeOpacity={0.18} />
          </g>
        ))}
        {crossRow !== null ? <rect x={0} y={crossRow * CELL} width={count * CELL} height={CELL} fill="var(--raw-graph-accent)" fillOpacity={0.08} /> : null}
        {crossColumn !== null ? <rect x={crossColumn * CELL} y={0} width={CELL} height={count * CELL} fill="var(--raw-graph-accent)" fillOpacity={0.08} /> : null}
        {dsm.order.map((taskId, position) => (
          <rect key={taskId} x={position * CELL + 9} y={position * CELL + 9} width={CELL - 18} height={CELL - 18} fill="var(--raw-graph-text)" fillOpacity={0.35} />
        ))}
        {dsm.cells.map((cell) => {
          const x = cell.column * CELL;
          const y = cell.row * CELL;
          const lit = isolation?.edges.has(cell.id) ?? false;
          const dimmed = isolation !== null && !lit;
          const onCritical = critical?.edges.has(cell.id) ?? false;
          const tone = cell.back ? 'var(--raw-state-conflicting)' : lit ? 'var(--raw-graph-accent)' : 'var(--raw-graph-text)';
          return (
            <g key={cell.id} opacity={dimmed ? 0.25 : 1} data-work-dsm-cell={cell.id} data-work-dsm-back={cell.back ? 'true' : undefined} data-work-dsm-intensity={cell.intensity}>
              {cell.kind === 'gating' ? (
                <rect x={x + 5} y={y + 5} width={CELL - 10} height={CELL - 10} fill={tone} fillOpacity={cell.back ? 0.9 : cell.intensity} />
              ) : cell.kind === 'informational' ? (
                <rect x={x + 6} y={y + 6} width={CELL - 12} height={CELL - 12} fill="none" stroke={tone} strokeOpacity={cell.intensity} strokeDasharray="3 2" />
              ) : (
                <circle cx={x + CELL / 2} cy={y + CELL / 2} r={3.5} fill={tone} fillOpacity={cell.intensity} />
              )}
              {cell.back ? <rect x={x + 2} y={y + 2} width={CELL - 4} height={CELL - 4} fill="none" stroke="var(--raw-state-conflicting)" strokeWidth={1.5} /> : null}
              {onCritical ? <rect x={x + 1.5} y={y + 1.5} width={CELL - 3} height={CELL - 3} fill="none" stroke="var(--raw-graph-alert)" strokeWidth={1.5} /> : null}
            </g>
          );
        })}
      </svg>
      <ul
        aria-hidden
        className="absolute flex flex-wrap items-center gap-x-4 gap-y-1 text-3xs text-text-muted"
        style={{ left: 8, top: top + count * CELL + 10, width: width - 16 }}
      >
        <li className="flex items-center gap-1.5"><span className="size-2.5 bg-text-secondary" />gating · row needs column</li>
        <li className="flex items-center gap-1.5"><span className="size-2.5 border border-dashed border-text-secondary" />informational</li>
        <li className="flex items-center gap-1.5"><span className="size-1.5 rounded-full bg-text-secondary" />causal candidate</li>
        <li className="flex items-center gap-1.5"><span className="size-2.5 bg-state-conflicting outline outline-1 outline-offset-1 outline-state-conflicting" />back-edge · above the diagonal, inside a declared cycle</li>
        <li className="flex items-center gap-1.5"><span className="size-2.5 border border-alert" />amber ring · effort-weighted critical path</li>
        <li className="flex items-center gap-1.5"><span className="size-2.5 border border-edge-strong bg-accent/10" />diagonal block · one stratum</li>
        <li>brighter · more relations name that column&apos;s task</li>
      </ul>
      {hover !== null ? (
        <div
          aria-hidden
          className="pointer-events-none absolute z-10 border border-edge-strong bg-surface-2 px-2 py-1 text-2xs shadow-lg rounded-[var(--radius-panel)]"
          style={{ left: HEADER + (hover.column + 1) * CELL + 6, top: top + hover.row * CELL - 4 }}
          data-work-dsm-hover={hovered?.id ?? 'empty'}
        >
          <span className="td-value">{dsm.order[hover.row]}</span>
          <span className="text-text-muted">
            {hovered === undefined
              ? ' declares nothing on '
              : hovered.kind === 'gating'
                ? hovered.back
                  ? ' needs (back-edge) '
                  : ' needs '
                : hovered.kind === 'informational'
                  ? ' relates to '
                  : ' names as cause '}
          </span>
          <span className="td-value">{dsm.order[hover.column]}</span>
        </div>
      ) : null}
    </div>
  );
}
