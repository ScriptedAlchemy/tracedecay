import { useId, useMemo, useRef } from 'react';
import { cn } from '../../ui/cn';
import {
  OpenedStrip,
  Swatch,
  TopologyPopulation,
  useApertureWidth,
  type TopologyInteraction,
} from './DelegationTopology.tsx';
import { neighbourhood, type FittedTopology, type TopologyMark } from './delegationTopology.ts';
import {
  layoutDelegationTimeline,
  timelineTickLabel,
  timelineTicks,
  type TimelineRow,
} from './delegationTimeline.ts';
import { subagentElapsedSeconds } from './subagentTree.ts';
import { markHandlers } from './topologyVariant.tsx';

/**
 * The delegation timeline: recorded time across, the delegation hierarchy
 * down. Each row is one drawn mark in pre-order and is itself the 44px
 * control, so the field keeps the disc field's operability without a second
 * tab order. Bars, brackets and density are SVG decoration of those rows.
 */

const GUTTER = 232;
const PAD_RIGHT = 40;
const ROW = 44;
const LANE_HEADER = 24;
const LANE_GAP = 10;
const AXIS = 30;
const INDENT = 12;
const MIN_WIDTH = 640;

export function DelegationTimeline({
  fit,
  interaction,
}: {
  fit: FittedTopology;
  interaction: TopologyInteraction;
}) {
  const { model } = fit;
  const { inspectedId, selectedId } = interaction;
  const apertureRef = useRef<HTMLDivElement | null>(null);
  const width = Math.max(MIN_WIDTH, useApertureWidth(apertureRef) ?? 960);
  const hatchId = useId();
  const timeline = useMemo(() => layoutDelegationTimeline(model), [model]);
  const keep = useMemo(
    () =>
      inspectedId !== null && model.marks.some((mark) => mark.id === inspectedId)
        ? neighbourhood(model, inspectedId)
        : null,
    [model, inspectedId],
  );

  const laneTop: number[] = [];
  let cursor = AXIS;
  for (const lane of timeline.lanes) {
    laneTop.push(cursor);
    cursor += LANE_HEADER + lane.rows * ROW + LANE_GAP;
  }
  const height = cursor;
  const rowY = (row: TimelineRow) => {
    const lane = timeline.lanes[row.lane]!;
    return laneTop[row.lane]! + LANE_HEADER + (row.index - lane.firstRow) * ROW + ROW / 2;
  };
  const plot = width - GUTTER - PAD_RIGHT;
  const domain = timeline.domain;
  const x = (at: number) =>
    domain === null ? GUTTER : GUTTER + ((at - domain.start) / (domain.end - domain.start)) * plot;
  const ticks = domain === null ? null : timelineTicks(domain, plot);
  const peak = timeline.lanes.reduce((max, lane) => Math.max(max, lane.peak), 0);

  return (
    <div className="flex min-w-0 flex-col gap-2" data-delegation-timeline={timeline.rows.length}>
      <div
        ref={apertureRef}
        role="group"
        aria-label="Delegation timeline field"
        tabIndex={0}
        className="td-optic td-grain td-graticule relative max-h-[34rem] min-h-[16rem] overflow-auto"
        onMouseLeave={() => interaction.onInspect(null)}
        onBlur={(event) => {
          const next = event.relatedTarget;
          if (!(next instanceof Node) || !event.currentTarget.contains(next)) interaction.onInspect(null);
        }}
      >
        <div className="relative" style={{ width, height }}>
          <svg aria-hidden width={width} height={height} className="absolute inset-0">
            <defs>
              <pattern id={hatchId} width="5" height="5" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
                <line x1="0" y1="0" x2="0" y2="5" stroke="var(--raw-graph-text)" strokeOpacity={0.6} strokeWidth="1.2" />
              </pattern>
            </defs>
            {ticks?.ticks.map((at) => (
              <g key={at}>
                <line x1={x(at)} x2={x(at)} y1={AXIS - 6} y2={height} stroke="var(--raw-graph-edge)" strokeOpacity={0.28} strokeDasharray="1 5" />
                <text x={x(at)} y={AXIS - 12} textAnchor="middle" fill="var(--raw-graph-text)" fillOpacity={0.8} fontSize={11} fontFamily="var(--font-mono)">
                  {timelineTickLabel(at, ticks.step)}
                </text>
              </g>
            ))}
            <text x={12} y={AXIS - 12} fill="var(--raw-graph-text)" fillOpacity={0.6} fontSize={10} fontFamily="var(--font-mono)" letterSpacing="0.12em">
              UTC · RECORDED START → END
            </text>
            {timeline.lanes.map((lane) => {
              const top = laneTop[lane.index]!;
              const bandHeight = LANE_HEADER - 8;
              const points =
                domain === null || peak === 0
                  ? ''
                  : lane.density
                      .map((step, index) => {
                        const next = lane.density[index + 1];
                        const y = top + 4 + bandHeight - (step.count / peak) * bandHeight;
                        return `L${x(step.at)},${y} L${next ? x(next.at) : x(step.at)},${y}`;
                      })
                      .join(' ');
              return (
                <g key={lane.topId} data-timeline-lane={lane.index} data-timeline-peak={lane.peak}>
                  <rect x={0} y={top} width={width} height={LANE_HEADER + lane.rows * ROW} fill="var(--raw-graph-dim)" fillOpacity={lane.index % 2 === 0 ? 0.18 : 0.08} />
                  <line x1={0} x2={width} y1={top} y2={top} stroke="var(--raw-graph-edge)" strokeOpacity={0.5} />
                  {points === '' ? null : (
                    <path
                      d={`M${x(lane.density[0]!.at)},${top + 4 + bandHeight} ${points} L${x(lane.density[lane.density.length - 1]!.at)},${top + 4 + bandHeight} Z`}
                      fill="var(--raw-graph-accent)"
                      fillOpacity={0.22}
                      stroke="var(--raw-graph-accent)"
                      strokeOpacity={0.7}
                      strokeWidth={1}
                    />
                  )}
                </g>
              );
            })}
            {timeline.brackets.map((bracket) => {
              const parent = timeline.rows[bracket.parentRow]!;
              const child = timeline.rows[bracket.childRow]!;
              const lit = keep !== null && keep.has(parent.mark.id) && keep.has(child.mark.id);
              const dim = keep !== null && !lit;
              const at = x(bracket.at);
              const y1 = rowY(parent);
              const y2 = rowY(child);
              return bracket.kind === 'spawn' ? (
                <path
                  key={`spawn:${child.mark.id}`}
                  d={`M${at - 5},${y1} H${at} V${y2} H${at + 5}`}
                  fill="none"
                  stroke={lit ? 'var(--raw-graph-accent)' : 'var(--raw-graph-edge)'}
                  strokeWidth={lit ? 1.6 : 1.1}
                  strokeOpacity={dim ? 0.2 : 0.9}
                  data-timeline-bracket="spawn"
                />
              ) : (
                <path
                  key={`join:${child.mark.id}`}
                  d={`M${at - 5},${y2} H${at} V${y1 + 6} M${at - 3},${y1 + 10} L${at},${y1 + 5} L${at + 3},${y1 + 10}`}
                  fill="none"
                  stroke={lit ? 'var(--raw-graph-accent)' : 'var(--raw-graph-edge)'}
                  strokeWidth={1}
                  strokeDasharray="3 3"
                  strokeOpacity={dim ? 0.15 : 0.7}
                  data-timeline-bracket="join"
                />
              );
            })}
            {timeline.rows.map((row) => (
              <TimelineBar
                key={row.mark.id}
                row={row}
                y={rowY(row)}
                x={x}
                domainEnd={domain?.end ?? null}
                hatchId={hatchId}
                inspected={row.mark.id === inspectedId}
                selected={row.mark.id === selectedId}
                dim={keep !== null && !keep.has(row.mark.id)}
              />
            ))}
          </svg>
          {timeline.lanes.map((lane) => (
            <span
              key={lane.topId}
              className="td-legend pointer-events-none absolute left-3"
              style={{ top: laneTop[lane.index]! + 7, color: 'var(--raw-graph-text)' }}
            >
              lane {lane.index + 1} · peak {lane.peak}
              {lane.unmeasured > 0 ? ` · ${lane.unmeasured} unmeasured` : ''}
            </span>
          ))}
          <ul aria-label="Sessions and bundles in delegation order" className="absolute inset-0 m-0 list-none p-0">
            {timeline.rows.map((row) => (
              <li key={row.mark.id} className="contents">
                <TimelineRowControl
                  row={row}
                  top={rowY(row) - ROW / 2}
                  width={width}
                  interaction={interaction}
                  dim={keep !== null && !keep.has(row.mark.id)}
                />
              </li>
            ))}
          </ul>
        </div>
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1.5 px-1 text-3xs text-text-muted">
        <TopologyPopulation model={model} fit={fit} />
        <Swatch label="bar · recorded start → end">
          <rect x={2} y={5} width={20} height={6} fill="var(--raw-graph-accent)" fillOpacity={0.2} stroke="var(--raw-graph-accent)" />
        </Swatch>
        <Swatch label="open · end absent">
          <rect x={2} y={5} width={20} height={6} fill="none" stroke="var(--raw-graph-accent)" strokeDasharray="3 2" />
        </Swatch>
        <Swatch label="spawn · exact parent, child's start">
          <path d="M3 3 H8 V13 H13" fill="none" stroke="var(--raw-graph-edge)" strokeWidth={1.2} />
        </Swatch>
        <Swatch label="join · inferred from child's end">
          <path d="M13 13 H8 V4" fill="none" stroke="var(--raw-graph-edge)" strokeDasharray="3 3" />
        </Swatch>
        <Swatch label="cut edge · parent not in reading">
          <path d="M2 8 H9" stroke="var(--raw-graph-alert)" strokeWidth={1.2} strokeDasharray="3 2" />
          <rect x={10} y={5} width={12} height={6} fill="var(--raw-graph-alert)" fillOpacity={0.2} stroke="var(--raw-graph-alert)" />
        </Swatch>
        <Swatch label="parent cycle">
          <rect x={4} y={5} width={16} height={6} fill="var(--raw-state-conflicting)" fillOpacity={0.2} stroke="var(--raw-state-conflicting)" />
        </Swatch>
        <Swatch label="lane density · concurrent sessions">
          <path d="M2 13 V9 H8 V5 H14 V9 H22 V13 Z" fill="var(--raw-graph-accent)" fillOpacity={0.22} stroke="var(--raw-graph-accent)" strokeWidth={0.8} />
        </Swatch>
        <span data-timeline-unplaced={timeline.unplaced}>
          {timeline.unplaced} unplaced · start absent
        </span>
        <span data-timeline-open={timeline.open}>{timeline.open} open · end absent</span>
      </div>
      <OpenedStrip model={model} onToggleExpanded={interaction.onToggleExpanded} />
    </div>
  );
}

function barTone(mark: TopologyMark): { stroke: string; fill: string } {
  if (mark.kind === 'bundle') return { stroke: 'var(--raw-graph-text)', fill: 'hatch' };
  switch (mark.node.link) {
    case 'missing_parent':
      return { stroke: 'var(--raw-graph-alert)', fill: 'var(--raw-graph-alert)' };
    case 'cycle':
      return { stroke: 'var(--raw-state-conflicting)', fill: 'var(--raw-state-conflicting)' };
    case 'root':
    case 'linked':
      return { stroke: 'var(--raw-graph-accent)', fill: 'var(--raw-graph-accent)' };
    default: {
      const unhandled: never = mark.node.link;
      return unhandled;
    }
  }
}

function TimelineBar({
  row,
  y,
  x,
  domainEnd,
  hatchId,
  inspected,
  selected,
  dim,
}: {
  row: TimelineRow;
  y: number;
  x: (at: number) => number;
  domainEnd: number | null;
  hatchId: string;
  inspected: boolean;
  selected: boolean;
  dim: boolean;
}) {
  const tone = barTone(row.mark);
  const stub = row.mark.kind === 'session' && row.mark.parentId === null && row.mark.node.link !== 'root';
  if (row.start === null || domainEnd === null) {
    return (
      <text x={GUTTER + 8} y={y + 4} fill="var(--raw-graph-text)" fillOpacity={dim ? 0.3 : 0.7} fontSize={11} fontFamily="var(--font-mono)" data-timeline-bar="unplaced">
        start absent · not placed on time
      </text>
    );
  }
  const x1 = x(row.start);
  const open = row.end === null;
  const x2 = Math.max(x1 + 3, x(row.end ?? domainEnd));
  return (
    <g opacity={dim ? 0.3 : 1} data-timeline-bar={open ? 'open' : 'closed'}>
      {selected ? <rect x={x1 - 4} y={y - 10} width={x2 - x1 + 8} height={20} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={2} /> : null}
      {inspected && !selected ? <rect x={x1 - 3} y={y - 9} width={x2 - x1 + 6} height={18} fill="none" stroke="var(--raw-graph-text)" strokeOpacity={0.5} /> : null}
      {stub ? (
        <line x1={x1 - 28} x2={x1 - 2} y1={y} y2={y} stroke={tone.stroke} strokeWidth={1.2} strokeDasharray="3 2" />
      ) : null}
      <rect
        x={x1}
        y={y - 5}
        width={x2 - x1}
        height={10}
        fill={tone.fill === 'hatch' ? `url(#${hatchId})` : tone.fill}
        fillOpacity={tone.fill === 'hatch' ? 1 : open ? 0.08 : inspected || selected ? 0.34 : 0.2}
        stroke={tone.stroke}
        strokeWidth={1}
        strokeDasharray={open || row.mark.kind === 'bundle' ? '3 2' : undefined}
      />
      {open ? (
        <text x={x2 + 6} y={y + 4} fill="var(--raw-graph-text)" fillOpacity={0.7} fontSize={10} fontFamily="var(--font-mono)">
          open
        </text>
      ) : null}
    </g>
  );
}

function rowDetail(mark: TopologyMark): string {
  if (mark.kind === 'bundle') {
    return `${mark.sessions} sessions${mark.descendants > 0 ? ` · ${mark.descendants} beneath` : ''} · open bundle`;
  }
  const elapsed = subagentElapsedSeconds(mark.node);
  return [
    mark.node.provider,
    mark.node.descendants > 0 ? `${mark.node.descendants} beneath` : null,
    mark.foldedDescendants > 0 ? `${mark.foldedDescendants} folded` : null,
    elapsed === null ? 'span absent' : `${elapsed.toLocaleString()}s`,
  ]
    .filter(Boolean)
    .join(' · ');
}

function TimelineRowControl({
  row,
  top,
  width,
  interaction,
  dim,
}: {
  row: TimelineRow;
  top: number;
  width: number;
  interaction: TopologyInteraction;
  dim: boolean;
}) {
  const selected = row.mark.id === interaction.selectedId;
  return (
    <button
      type="button"
      className={cn(
        'absolute left-0 flex items-center text-left transition-opacity duration-[var(--dur-state)] hover:bg-surface-2/30',
        dim && 'opacity-40',
      )}
      style={{ top, height: ROW, width, color: 'var(--raw-graph-text)' }}
      {...markHandlers(row.mark, interaction)}
    >
      {selected ? <span aria-hidden className="absolute inset-y-1 left-0 w-[3px] bg-accent" /> : null}
      <span
        className="flex min-w-0 flex-col"
        style={{ paddingLeft: 12 + row.mark.generation * INDENT, width: GUTTER - 8 }}
      >
        <span className="truncate font-mono text-2xs tabular-nums">
          {row.mark.kind === 'bundle' ? `${row.mark.sessions} × ${row.mark.label}` : row.mark.label}
        </span>
        <span className="truncate font-mono text-3xs tabular-nums opacity-70">{rowDetail(row.mark)}</span>
      </span>
    </button>
  );
}
