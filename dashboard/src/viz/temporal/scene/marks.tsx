/**
 * The SVG marks inside the host's accessible buttons, and the
 * non-interactive overlay: grade tags on causal links, engraved rail legends,
 * the selection gutter, the playback cursor and `NOW`.
 *
 * Loaded-page language only: `NOW` names the newest record in this page and
 * never a live connection. A recorded-order cursor spans only its own lane,
 * because an x in the undated gutter is not a time.
 */
import type { CSSProperties, JSX } from 'react';
import { formatMoment } from '../../../workspaces/loom/tracks.ts';
import { EventGlyph } from '../glyphs.tsx';
import { xToTime } from '../layout.ts';
import { gradeColorVar, gradeDashArray } from '../palette.ts';
import type { EvidenceGrade, SceneCluster, SceneLane, SceneNode, ScenePath } from '../types.ts';
import { focusAlpha, isLifted, pathEnds, recencyAlpha, resolvedLanes, type SceneFrame } from './frame.ts';

/** Focus is a stable 2px cyan ring, shown only for keyboard focus. */
export const NODE_CLASS = 'cursor-pointer outline-none [&:focus-visible>.td-focus-ring]:opacity-100';

/** Above this many drawn links, non-EXACT tags print only on the lifted chain. */
const TAGGED_LINKS_MAX = 40;

const ENGRAVED: CSSProperties = { fontFamily: 'var(--font-display)', fontStretch: '112%' };
const MONO: CSSProperties = { fontFamily: 'var(--font-mono)' };

const GRADE_TAG: Readonly<Record<EvidenceGrade, string>> = {
  exact: 'EXACT',
  explicit: 'EXPLICIT',
  inferred: 'INFERRED',
  ambiguous: 'AMBIGUOUS',
  stale: 'STALE',
  unavailable: 'UNAVAILABLE',
};

function isLink(path: ScenePath): boolean {
  return path.kind === 'spawn' || path.kind === 'handoff' || path.kind === 'rejoin' || path.kind === 'result';
}

function FocusRing({ x, y, r }: { x: number; y: number; r: number }): JSX.Element {
  return <circle className="td-focus-ring" cx={x} cy={y} r={r} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={2} opacity={0} pointerEvents="none" />;
}

export function NodeMark({ node, hovered, frame }: { node: SceneNode; hovered: boolean; frame: SceneFrame }): JSX.Element {
  const color = gradeColorVar(node.grade);
  const luminance = isLifted(node.focus) ? 1 : recencyAlpha(frame, node.x);
  if (!resolvedLanes(frame).has(node.laneId) && !node.selected) {
    return (
      <>
        <FocusRing x={node.x} y={node.y} r={6} />
        {hovered && <line x1={node.x} x2={node.x} y1={node.y - 8} y2={node.y + 8} stroke="var(--raw-graph-accent)" strokeWidth={1} pointerEvents="none" />}
      </>
    );
  }
  return (
    <>
      {node.selected && <circle cx={node.x} cy={node.y} r={15} fill="var(--raw-graph-accent)" opacity={0.14} pointerEvents="none" />}
      {(node.selected || hovered) && (
        <rect x={node.x - 10} y={node.y - 10} width={20} height={20} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={node.selected ? 2 : 1} opacity={node.selected ? 1 : 0.6} pointerEvents="none" />
      )}
      <FocusRing x={node.x} y={node.y} r={13} />
      <g opacity={luminance} pointerEvents="none">
        <rect x={node.x - 7} y={node.y - 7} width={14} height={14} fill="var(--raw-graph-substrate)" stroke={color} strokeWidth={1} strokeDasharray={gradeDashArray(node.grade) || undefined} />
        <g transform={`translate(${node.x} ${node.y})`} color={color}>
          <EventGlyph kind={node.kind} size={11} />
        </g>
      </g>
    </>
  );
}

export function ClusterMark({ cluster }: { cluster: SceneCluster }): JSX.Element {
  const x1 = Math.max(cluster.x1, cluster.x0 + 2);
  const top = cluster.y - cluster.height / 2 + 1;
  const h = cluster.height - 3;
  return (
    <path
      d={`M ${cluster.x0 + 4} ${top} H ${cluster.x0} V ${top + h} H ${cluster.x0 + 4} M ${x1 - 4} ${top} H ${x1} V ${top + h} H ${x1 - 4}`}
      fill="none"
      stroke="var(--raw-graph-edge)"
      strokeWidth={1}
    />
  );
}

function CursorAndTail({ frame }: { frame: SceneFrame }): JSX.Element {
  const { model, fieldX0, fieldX1, top, height, density } = frame;
  const cursor = model.cursor;
  const cursorLane = cursor === null ? undefined : model.lanes.find((lane) => lane.id === cursor.laneId);
  const tailX = density?.tailX ?? null;
  const tailTime = density?.tailTime ?? null;
  const tailLater = tailTime !== null && tailTime > model.viewport.window.end;
  const cursorText =
    cursor === null
      ? null
      : cursor.xBasis === 'time'
        ? `CURSOR ${formatMoment(xToTime(model.viewport, cursor.x))}`
        : 'CURSOR · RECORDED ORDER';
  const nearRight = cursor !== null && cursor.x > fieldX1 - 180;
  return (
    <g pointerEvents="none">
      {tailX !== null && (
        <g data-tail-marker data-tail-x={Math.round(tailX)}>
          <title>{`NOW = newest record in this loaded page${tailTime === null ? '' : ` · ${formatMoment(tailTime)}`} · not a live stream`}</title>
          <line x1={tailX + 0.5} x2={tailX + 0.5} y1={top - 6} y2={height} stroke="var(--raw-graph-accent)" strokeWidth={1} opacity={0.55} strokeDasharray="2 3" />
          <path d={`M ${tailX - 4} ${top - 9} H ${tailX + 4} L ${tailX} ${top - 3} Z`} fill="var(--raw-graph-accent)" />
          <text x={Math.min(tailX, fieldX1 - 2)} y={11} fontSize={10} letterSpacing="0.16em" textAnchor={tailX > fieldX1 - 30 ? 'end' : 'middle'} fill="var(--raw-graph-accent)" style={ENGRAVED}>
            NOW
          </text>
        </g>
      )}
      {tailX === null && tailLater && (
        <g data-tail-marker data-tail-x="later">
          <title>NOW lies after this window</title>
          <text x={fieldX1 - 2} y={11} fontSize={10} letterSpacing="0.16em" textAnchor="end" fill="var(--raw-graph-accent)" style={ENGRAVED}>
            NOW →
          </text>
        </g>
      )}
      {cursor && cursor.x >= fieldX0 && cursor.x <= fieldX1 && (
        <g data-cursor-mark data-cursor-basis={cursor.xBasis}>
          {cursor.xBasis === 'time' || !cursorLane ? (
            <>
              <line data-cursor x1={cursor.x} x2={cursor.x} y1={top - 10} y2={height} stroke="var(--raw-graph-accent)" strokeWidth={1} />
              <rect x={cursor.x - 1.5} y={top - 12} width={3} height={6} fill="var(--raw-graph-accent)" />
            </>
          ) : (
            <line data-cursor x1={cursor.x} x2={cursor.x} y1={cursorLane.y - cursorLane.height / 2} y2={cursorLane.y + cursorLane.height / 2} stroke="var(--raw-graph-accent)" strokeWidth={1} />
          )}
          {cursorText && (
            <text
              x={nearRight ? cursor.x - 5 : cursor.x + 5}
              y={cursor.xBasis === 'time' || !cursorLane ? top + 11 : cursorLane.y + cursorLane.height / 2 - 4}
              fontSize={9}
              letterSpacing="0.16em"
              textAnchor={nearRight ? 'end' : 'start'}
              fill="var(--raw-graph-accent)"
              style={ENGRAVED}
            >
              {cursorText}
            </text>
          )}
        </g>
      )}
    </g>
  );
}

export function FieldOverlay({ frame }: { frame: SceneFrame }): JSX.Element {
  const { model, fieldX1 } = frame;
  const links = model.paths.filter(isLink);
  const tagAll = links.length <= TAGGED_LINKS_MAX;
  const gutterX = fieldX1 + (model.viewport.right + 8) / 2;
  return (
    <g pointerEvents="none">
      {model.rails.map((rail) => {
        const text = `${rail.label.toUpperCase()} · ${rail.lanes}`;
        if (rail.y1 - rail.y0 < text.length * 7 + 8) return null;
        const cy = (rail.y0 + rail.y1) / 2;
        return (
          <g key={rail.id} data-rail-legend={rail.label}>
            <line x1={fieldX1 + 4} x2={fieldX1 + 4} y1={rail.y0 + 2} y2={rail.y1 - 2} stroke="var(--raw-graph-edge)" strokeWidth={1} />
            <text x={gutterX} y={cy} transform={`rotate(-90 ${gutterX} ${cy})`} fontSize={10} letterSpacing="0.16em" textAnchor="middle" dominantBaseline="central" fill="var(--raw-graph-text)" opacity={0.6} style={ENGRAVED}>
              {text}
            </text>
          </g>
        );
      })}
      {model.lanes
        .filter((lane) => lane.focus === 'selected')
        .map((lane) => (
          <rect key={lane.id} data-selected-gutter x={0} y={lane.y - lane.height / 2} width={2} height={lane.height} fill="var(--raw-graph-accent)" />
        ))}
      {links.map((path) => {
        const { x0, y0, x1, y1 } = pathEnds(path.controls);
        if (Math.abs(y1 - y0) < 20) return null;
        // Solid EXACT is the default reading; every other grade is printed,
        // and the lifted chain prints all of its grades.
        if (!isLifted(path.focus) && (path.grade === 'exact' || !tagAll)) return null;
        return (
          <text
            key={path.id}
            data-link-grade={path.grade}
            x={(x0 + x1) / 2 - 4}
            y={(y0 + y1) / 2 + 3}
            fontSize={8}
            letterSpacing="0.1em"
            textAnchor="end"
            fill={gradeColorVar(path.grade)}
            opacity={focusAlpha(path.focus)}
            style={MONO}
          >
            {GRADE_TAG[path.grade]}
          </text>
        );
      })}
      <CursorAndTail frame={frame} />
    </g>
  );
}

/** The lane column's second line: exact totals over the page, never the window. */
export function laneDetail(lane: SceneLane, frame: SceneFrame): string | null {
  const density = frame.density?.lanes.get(lane.id);
  if (!density) return null;
  const { totals, peak } = density;
  if (lane.kind === 'bundle') {
    return `1+${totals.sessions - 1} sess · ${totals.messages.toLocaleString()} msg · peak ${peak.active}${peak.open > 0 ? ` · ${peak.open} open` : ''}`;
  }
  const commits = totals.commits > 0 ? ` · ${totals.commits} ${totals.commits === 1 ? 'commit' : 'commits'}` : '';
  return `${lane.provider} · ${totals.messages.toLocaleString()} msg${commits}`;
}

export function LegendEncodings(): JSX.Element {
  return (
    <p className="text-3xs text-text-muted" data-legend-encodings>
      brightness = recency within the loaded page, dimmest at the oldest start and full at NOW · rail weight = messages (log) · bundle bars = member sessions with a measured extent (√, one scale for every bundle) · dots = begun, extent unknown · floor ticks = dated events per bin where glyphs would not fit · link tags print every non-EXACT grade · the selected chain lifts over the recency veil
    </p>
  );
}
