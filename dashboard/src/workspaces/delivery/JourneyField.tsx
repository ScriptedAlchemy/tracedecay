import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type RefObject } from 'react';
import { Corners } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { gradeDash, gradeLabel } from './evidence.ts';
import { microsToIso } from './deliveryChrome.tsx';
import {
  laneLabel,
  laneServes,
  layoutJourney,
  type JourneyBreak,
  type JourneyEpisode,
  type JourneyLaneId,
  type JourneyLayout,
  type JourneyModel,
  type JourneyPoint,
} from './journey.ts';

/**
 * The PR journey field: time is X, source lane is Y. Every mark here is a
 * projection of a DOM row in the exact table and the inspector — the field is
 * never the only place a time, grade or destination can be read. Undated
 * records sit in the hatched gutter, never on the axis; observation time is
 * shaped differently from event time; a lane whose authority is not served is
 * drawn dashed and named as such in the DOM label column.
 */

const RULER_HEIGHT = 24;
const FALLBACK_WIDTH = 900;

function useMeasuredWidth(): [RefObject<HTMLDivElement | null>, number] {
  const ref = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState<number | null>(null);
  useEffect(() => {
    const element = ref.current;
    if (element === null || typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry !== undefined) setWidth(entry.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width === null || width <= 0 ? FALLBACK_WIDTH : width];
}

export function JourneyField({
  model,
  selectedEpisodeId,
  onSelect,
  className,
}: {
  model: JourneyModel;
  selectedEpisodeId: string | null;
  onSelect: (episode: JourneyEpisode) => void;
  className?: string;
}) {
  const [containerRef, width] = useMeasuredWidth();
  const layout = useMemo(() => layoutJourney(model, { width }), [model, width]);
  const total = layout.height + RULER_HEIGHT;

  return (
    <div
      className={cn('td-optic td-grain relative overflow-hidden', className)}
      data-field="delivery-journey"
    >
      <Corners tone="signal" />
      <div className="relative z-[1]" style={{ height: total }}>
        <div className="absolute inset-y-0 left-0 w-28 border-r border-edge-subtle">
          <div className="td-legend flex items-center px-2" style={{ height: RULER_HEIGHT }}>
            time · UTC
          </div>
          <ol aria-label="Journey lanes">
            {layout.rows.map((row) => (
              <li
                key={row.lane.id}
                className="absolute inset-x-0 flex flex-wrap items-center gap-x-1.5 px-2"
                style={{
                  top: RULER_HEIGHT + row.y - layout.laneHeight / 2,
                  height: layout.laneHeight,
                }}
              >
                <span className="td-legend">{laneLabel(row.lane.id)}</span>
                {laneServes(row.lane.state) ? null : (
                  <span className="font-mono text-[10px] text-state-offline">unavailable</span>
                )}
              </li>
            ))}
          </ol>
        </div>
        <div ref={containerRef} className="ml-28 h-full">
          <svg
            viewBox={`0 0 ${layout.width} ${total}`}
            width="100%"
            height={total}
            preserveAspectRatio="xMinYMin meet"
            className="block"
            role="group"
            aria-label="PR journey field"
          >
            <defs>
              <pattern id="journey-graticule" width="32" height="32" patternUnits="userSpaceOnUse">
                <path d="M 32 0 L 0 0 0 32" fill="none" stroke="var(--raw-grid-minor)" strokeWidth="0.5" />
              </pattern>
              <pattern
                id="journey-hatch"
                width="6"
                height="6"
                patternUnits="userSpaceOnUse"
                patternTransform="rotate(45)"
              >
                <line x1="0" y1="0" x2="0" y2="6" stroke="var(--raw-graph-edge)" strokeWidth="0.8" strokeOpacity="0.7" />
              </pattern>
              <radialGradient id="journey-node-glow">
                <stop offset="0%" stopColor="var(--raw-graph-accent)" stopOpacity="0.55" />
                <stop offset="100%" stopColor="var(--raw-graph-accent)" stopOpacity="0" />
              </radialGradient>
            </defs>
            <rect width={layout.width} height={total} fill="url(#journey-graticule)" aria-hidden />
            <Ruler layout={layout} />
            <g transform={`translate(0 ${RULER_HEIGHT})`}>
              <Gutter layout={layout} />
              <Lanes layout={layout} />
              {layout.breaks.map((axisBreak) => (
                <AxisBreak key={axisBreak.x} axisBreak={axisBreak} layout={layout} />
              ))}
              <Spine layout={layout} />
              {layout.points.map((point) => (
                <EpisodeNode
                  key={point.episode.id}
                  point={point}
                  selected={point.episode.id === selectedEpisodeId}
                  onSelect={onSelect}
                />
              ))}
            </g>
          </svg>
        </div>
      </div>
    </div>
  );
}

function Ruler({ layout }: { layout: JourneyLayout }) {
  return (
    <g aria-hidden>
      <line
        x1={layout.gutterWidth}
        y1={20}
        x2={layout.width}
        y2={20}
        stroke="var(--raw-graph-edge)"
        strokeWidth="1"
      />
      {layout.ticks.map((tick) => (
        <g key={tick.at}>
          <line x1={tick.x} y1={16} x2={tick.x} y2={24} stroke="var(--raw-graph-text)" strokeOpacity="0.7" strokeWidth="1" />
          <text
            x={tick.x}
            y={12}
            textAnchor="middle"
            fontSize="9"
            fontFamily="var(--font-mono)"
            fill="var(--raw-graph-text)"
            letterSpacing="0.06em"
          >
            {tick.label}
          </text>
        </g>
      ))}
      {layout.gutterWidth > 0 ? (
        <text
          x={layout.gutterWidth / 2}
          y={14}
          textAnchor="middle"
          fontSize="8"
          fontFamily="var(--font-mono)"
          fill="var(--raw-graph-text)"
          fillOpacity="0.8"
          letterSpacing="0.14em"
        >
          UNDATED
        </text>
      ) : null}
    </g>
  );
}

function Gutter({ layout }: { layout: JourneyLayout }) {
  if (layout.gutterWidth === 0) return null;
  return (
    <g aria-hidden>
      <rect x={0} y={0} width={layout.gutterWidth} height={layout.height} fill="url(#journey-hatch)" />
      <line
        x1={layout.gutterWidth}
        y1={0}
        x2={layout.gutterWidth}
        y2={layout.height}
        stroke="var(--raw-graph-edge)"
        strokeWidth="1"
      />
    </g>
  );
}

function Lanes({ layout }: { layout: JourneyLayout }) {
  return (
    <g aria-hidden>
      {layout.rows.map((row) => {
        const served = laneServes(row.lane.state);
        return (
          <line
            key={row.lane.id}
            x1={0}
            y1={row.y}
            x2={layout.width}
            y2={row.y}
            stroke="var(--raw-graph-edge)"
            strokeWidth="1"
            strokeOpacity={served ? 0.85 : 0.4}
            strokeDasharray={served ? undefined : '2 4'}
          />
        );
      })}
    </g>
  );
}

function zigPath(x: number, top: number, bottom: number): string {
  const step = 6;
  let path = `M ${x - 2} ${top}`;
  let y = top;
  let side = 1;
  while (y < bottom) {
    y = Math.min(bottom, y + step);
    path += ` L ${x + 2 * side} ${y}`;
    side = -side;
  }
  return path;
}

/** Compressed empty time: the lane lines are cut and two zig lines mark the
 * seam from the ruler down through every lane. */
function AxisBreak({ axisBreak, layout }: { axisBreak: JourneyBreak; layout: JourneyLayout }) {
  const top = -RULER_HEIGHT + 10;
  return (
    <g
      role="img"
      aria-label={`Compressed time · ${microsToIso(axisBreak.fromMicros)} → ${microsToIso(axisBreak.toMicros)}`}
    >
      <rect
        x={axisBreak.x - 4}
        y={top}
        width={8}
        height={layout.height - top}
        fill="var(--raw-graph-substrate)"
      />
      <path d={zigPath(axisBreak.x - 3, top, layout.height)} fill="none" stroke="var(--raw-graph-text)" strokeOpacity="0.6" strokeWidth="1" />
      <path d={zigPath(axisBreak.x + 3, top, layout.height)} fill="none" stroke="var(--raw-graph-text)" strokeOpacity="0.6" strokeWidth="1" />
      <title>
        compressed · {microsToIso(axisBreak.fromMicros)} → {microsToIso(axisBreak.toMicros)}
      </title>
    </g>
  );
}

/** The delivery spine: the pull request lane between its first and last dated
 * observation, so the reader sees where the provider record begins and ends. */
function Spine({ layout }: { layout: JourneyLayout }) {
  const xs = layout.points
    .filter((point) => point.episode.lane === 'pull_request' && point.episode.at !== null)
    .map((point) => point.x);
  const row = layout.rows.find((candidate) => candidate.lane.id === 'pull_request');
  if (xs.length < 2 || row === undefined) return null;
  return (
    <line
      aria-hidden
      x1={Math.min(...xs)}
      y1={row.y}
      x2={Math.max(...xs)}
      y2={row.y}
      stroke="var(--raw-graph-accent)"
      strokeWidth="1.5"
      strokeOpacity="0.7"
    />
  );
}

function laneFill(lane: JourneyLaneId): string {
  switch (lane) {
    case 'commits':
    case 'pull_request':
    case 'releases':
      return 'var(--raw-graph-accent)';
    case 'reviews':
    case 'checks':
      return 'var(--raw-graph-alert)';
    case 'objective':
    case 'sessions':
    case 'agents':
      return 'var(--raw-graph-text)';
    default: {
      const unhandled: never = lane;
      return unhandled;
    }
  }
}

/** Shape carries time kind; dash carries grade; fill carries lane. No mark
 * relies on hue alone. */
function NodeShape({ point, strokeWidth }: { point: JourneyPoint; strokeWidth: number }) {
  const { x, y, episode } = point;
  const colour = laneFill(episode.lane);
  const common = {
    fill: colour,
    fillOpacity: episode.grade === 'exact' ? 1 : 0.2,
    stroke: colour,
    strokeWidth,
    strokeDasharray: gradeDash(episode.grade),
  };
  switch (episode.timeKind) {
    case 'event':
      return <circle cx={x} cy={y} r={5} {...common} />;
    case 'observed':
      return <rect x={x - 4.5} y={y - 4.5} width={9} height={9} transform={`rotate(45 ${x} ${y})`} {...common} />;
    case 'undated':
      return <rect x={x - 4} y={y - 4} width={8} height={8} rx={1.5} {...common} />;
    default: {
      const unhandled: never = episode.timeKind;
      return unhandled;
    }
  }
}

function EpisodeNode({
  point,
  selected,
  onSelect,
}: {
  point: JourneyPoint;
  selected: boolean;
  onSelect: (episode: JourneyEpisode) => void;
}) {
  const { x, y, episode } = point;
  const activate = (event: KeyboardEvent<SVGGElement>) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      onSelect(episode);
    }
  };
  return (
    <g
      role="button"
      tabIndex={0}
      aria-label={`${laneLabel(episode.lane)} · ${episode.label} · ${gradeLabel(episode.grade)}`}
      aria-pressed={selected}
      className="cursor-pointer outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
      onClick={() => onSelect(episode)}
      onKeyDown={activate}
    >
      {selected ? <circle cx={x} cy={y} r={14} fill="url(#journey-node-glow)" /> : null}
      {episode.status === 'error' ? (
        <circle cx={x} cy={y} r={8} fill="none" stroke="var(--raw-state-error)" strokeWidth="1.5" />
      ) : null}
      <NodeShape point={point} strokeWidth={selected ? 2 : 1.2} />
      <title>
        {laneLabel(episode.lane)} · {episode.label} · {episode.detail} · {gradeLabel(episode.grade)}
      </title>
    </g>
  );
}
