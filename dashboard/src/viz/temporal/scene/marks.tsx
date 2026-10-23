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
import { gradeColorVar, gradeDashArray } from '../palette.ts';
import type { EvidenceGrade, SceneCluster, SceneLane, SceneNode } from '../types.ts';
import { linkKind, type SceneAnnotations } from './annotations.ts';
import { focusAlpha, isLifted, recencyAlpha, resolvedLanes, type SceneFrame, type TemporalSceneVariant } from './frame.ts';

/** Focus is a stable 2px cyan ring, shown only for keyboard focus. */
export const NODE_CLASS = 'cursor-pointer outline-none [&:focus-visible>.td-focus-ring]:opacity-100';

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

function FocusRing({ x, y, r }: { x: number; y: number; r: number }): JSX.Element {
  return <circle className="td-focus-ring" cx={x} cy={y} r={r} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={2} opacity={0} pointerEvents="none" />;
}

export function NodeMark({ node, frame }: { node: SceneNode; frame: SceneFrame }): JSX.Element {
  const color = frame.model.spindle && frame.highContrast ? 'var(--raw-graph-text)'
    : node.color && node.grade !== 'ambiguous' && node.grade !== 'stale' && node.grade !== 'unavailable'
    ? node.color : gradeColorVar(node.grade);
  const luminance = frame.highContrast || isLifted(node.focus) ? 1 : recencyAlpha(frame, node.x);
  const resolved = frame.model.spindle ? node.glyphVisible !== false : resolvedLanes(frame).has(node.laneId);
  if (!resolved && !node.selected) {
    return (
      <>
        <FocusRing x={node.x} y={node.y} r={6} />
        {frame.variant === 'weave' && (
          <circle cx={node.x} cy={node.y} r={frame.model.spindle && (node.kind === 'session_start' || node.kind === 'session_end' || node.kind.startsWith('message_')) ? 1.2 : 2.2} fill={color} stroke="var(--raw-graph-substrate)" strokeWidth={frame.model.spindle ? .5 : 1} opacity={luminance} pointerEvents="none" />
        )}
      </>
    );
  }
  return (
    <>
      {node.selected && <circle cx={node.x} cy={node.y} r={15} fill="var(--raw-graph-accent)" opacity={0.14} pointerEvents="none" />}
      {node.selected && (
        <rect x={node.x - 10} y={node.y - 10} width={20} height={20} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={2} pointerEvents="none" />
      )}
      <FocusRing x={node.x} y={node.y} r={13} />
      <g opacity={luminance} pointerEvents="none">
        {frame.variant === 'weave' ? (
          <circle cx={node.x} cy={node.y} r={7} fill="var(--raw-graph-substrate)" stroke={color} strokeWidth={1} strokeDasharray={gradeDashArray(node.grade) || undefined} />
        ) : (
          <rect x={node.x - 7} y={node.y - 7} width={14} height={14} fill="var(--raw-graph-substrate)" stroke={color} strokeWidth={1} strokeDasharray={gradeDashArray(node.grade) || undefined} />
        )}
        <g transform={`translate(${node.x} ${node.y})`} color={color}>
          <EventGlyph kind={node.kind} size={11} />
        </g>
      </g>
    </>
  );
}

export function ClusterMark({ cluster, frame }: { cluster: SceneCluster; frame: SceneFrame }): JSX.Element {
  const x1 = Math.max(cluster.x1, cluster.x0 + 2);
  const top = cluster.y - cluster.height / 2 + 1;
  const h = cluster.height - 3;
  return (
    <>
      <path
        d={`M ${cluster.x0 + 4} ${top} H ${cluster.x0} V ${top + h} H ${cluster.x0 + 4} M ${x1 - 4} ${top} H ${x1} V ${top + h} H ${x1 - 4}`}
        fill="none"
        stroke="var(--raw-graph-edge)"
        strokeWidth={1}
      />
      {frame.variant === 'weave' && cluster.height >= 28 && x1 - cluster.x0 >= 100 ? (
        <text x={cluster.x0 + 6} y={top + 9} fontSize={8} letterSpacing="0.1em" fill="var(--raw-graph-text)" opacity={0.75} style={MONO} pointerEvents="none">
          {frame.density?.lanes.get(cluster.laneId)?.totals.sessions ?? cluster.counts.sessions} THREADS
        </text>
      ) : null}
    </>
  );
}

function CursorAndTail({ frame, annotations }: { frame: SceneFrame; annotations: SceneAnnotations }): JSX.Element {
  const { model, fieldX0, fieldX1, top, height, density } = frame;
  const cursor = model.cursor;
  const cursorLane = cursor === null ? undefined : model.lanes.find((lane) => lane.id === cursor.laneId);
  const cursorCenter = cursor?.y ?? cursorLane?.y ?? top;
  const cursorRadius = cursor?.y !== undefined ? 14 : (cursorLane?.height ?? 28) / 2;
  const tailX = density?.tailX ?? null;
  const tailTime = density?.tailTime ?? null;
  const tailLater = tailTime !== null && tailTime > model.viewport.window.end;
  const { cursorText, cursorLabel } = annotations;
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
          {cursor.xBasis === 'time' || (cursor.y === undefined && !cursorLane) ? (
            <>
              <line data-cursor x1={cursor.x} x2={cursor.x} y1={top - 10} y2={height} stroke="var(--raw-graph-accent)" strokeWidth={1} />
              <rect x={cursor.x - 1.5} y={top - 12} width={3} height={6} fill="var(--raw-graph-accent)" />
            </>
          ) : (
            <line data-cursor x1={cursor.x} x2={cursor.x} y1={cursorCenter - cursorRadius} y2={cursorCenter + cursorRadius} stroke="var(--raw-graph-accent)" strokeWidth={1} />
          )}
          {cursorText && cursorLabel && (
            <text
              x={cursorLabel.x}
              y={cursorLabel.y}
              fontSize={9}
              letterSpacing="0.16em"
              textAnchor={cursorLabel.anchor}
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

export function FieldOverlay({ frame, annotations }: { frame: SceneFrame; annotations: SceneAnnotations }): JSX.Element {
  const { model, fieldX1 } = frame;
  const gutterX = fieldX1 + (model.viewport.right + 8) / 2;
  return (
    <g pointerEvents="none">
      {annotations.tally && annotations.tallyInRuler && (
        <text data-link-tally x={frame.fieldX0 + 4} y={11} fontSize={9} letterSpacing="0.12em" fill="var(--raw-graph-text)" opacity={0.7} style={MONO}>
          {annotations.tally}
        </text>
      )}
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
      {annotations.tags.map(({ path, x, y }) => {
        return (
          <text
            key={path.id}
            data-link-grade={path.grade}
            x={x}
            y={y + 10}
            fontSize={10}
            letterSpacing="0.1em"
            textAnchor="start"
            fill={gradeColorVar(path.grade)}
            opacity={focusAlpha(path.focus)}
            style={MONO}
          >
            {GRADE_TAG[path.grade]}
          </text>
        );
      })}
      {model.spindle && frame.density?.tailX != null && (
        <g data-spindle-tail>
          <title>Loaded tail · newest recorded time in this page. The converging fibers retain their individual session identities.</title>
          <circle cx={frame.density.tailX} cy={model.spindle.top + model.spindle.height * .515} r={4} fill="var(--raw-graph-accent)" opacity={0.8} />
          <text x={Math.min(fieldX1 - 4, frame.density.tailX - 8)} y={model.spindle.top + model.spindle.height * .515 - 13} textAnchor="end" fontSize={9} letterSpacing="0.12em" fill="var(--raw-graph-accent)" style={ENGRAVED}>
            {fieldX1 - frame.fieldX0 > 250 ? 'LOADED TAIL' : 'TAIL'}
          </text>
        </g>
      )}
      <CursorAndTail frame={frame} annotations={annotations} />
    </g>
  );
}

export function OverflowReadouts({ annotations }: { annotations: SceneAnnotations }): JSX.Element | null {
  const tally = annotations.tally && !annotations.tallyInRuler ? annotations.tally : null;
  const cursor = annotations.cursorText && !annotations.cursorLabel ? annotations.cursorText : null;
  if (!tally && !cursor) return null;
  return (
    <div className="flex flex-wrap gap-x-4 gap-y-1 border-b border-edge-subtle px-2 py-1 text-3xs text-text-secondary">
      {tally ? <p data-link-tally className="min-w-0 break-words td-value">{tally}</p> : null}
      {cursor ? <p data-cursor-readout className="min-w-0 break-words td-value text-accent">{cursor}</p> : null}
    </div>
  );
}

/** Touch and keyboard readers can open the full evidence without relying on
 * a tiny inline tag or a hover title. Only already-drawn links are included. */
export function RelationEvidence({ frame, annotations }: { frame: SceneFrame; annotations: SceneAnnotations }): JSX.Element | null {
  if (annotations.links.length === 0) return null;
  const labels = new Map([
    ...(frame.model.spindle?.fibers.map((fiber) => [fiber.id, fiber.label] as const) ?? []),
    ...frame.model.lanes.map((lane) => [lane.id, lane.label] as const),
    ...frame.model.nodes.map((node) => [node.id, node.label] as const),
  ]);
  return (
    <details aria-label="Relation evidence" className="border-t border-edge-subtle px-2 text-3xs text-text-secondary">
      <summary className="min-h-[var(--touch-target-min)] cursor-pointer py-2">Fork and join evidence · {annotations.links.length} relations</summary>
      <ul tabIndex={0} className="max-h-64 space-y-2 overflow-auto pb-2" aria-label="Drawn relation evidence">
        {annotations.links.map((path) => (
          <li key={path.id} data-link-evidence={path.id} className="break-words">
            <span style={{ color: gradeColorVar(path.grade) }}>{linkKind(path).toLowerCase()} · {GRADE_TAG[path.grade]}</span>
            {' · '}{labels.get(path.fromId) ?? path.fromId} → {labels.get(path.toId) ?? path.toId}
            {path.basis ? <span className="block text-text-muted">{path.basis}</span> : null}
          </li>
        ))}
      </ul>
    </details>
  );
}

const COUNT_FORMAT = new Intl.NumberFormat();

/** The lane column's second line: exact totals over the page, never the window. */
export function laneDetail(lane: SceneLane, frame: SceneFrame): string | null {
  const density = frame.density?.lanes.get(lane.id);
  if (!density) return null;
  const { totals, peak } = density;
  if (lane.kind === 'bundle') {
    return `1+${totals.sessions - 1} sess · ${COUNT_FORMAT.format(totals.messages)} msg · peak ${peak.active}${peak.open > 0 ? ` · ${peak.open} open` : ''}`;
  }
  const commits = totals.commits > 0 ? ` · ${totals.commits} ${totals.commits === 1 ? 'commit' : 'commits'}` : '';
  return `${lane.provider} · ${COUNT_FORMAT.format(totals.messages)} msg${commits}`;
}

export function LegendEncodings({ variant }: { variant: TemporalSceneVariant }): JSX.Element {
  const treatment = variant === 'weave'
    ? 'one filament = one recorded session, including collapsed members · color = recorded parent family · fan and convergence = presentation grouping · graded cross-links = source relations · dotted stub = extent unknown · beads = crowded events'
    : variant === 'strata'
      ? 'bands = session extents · solid/pale = exact/explicit · diagonal/crosshatch = inferred/ambiguous · sparse hatch/dots = stale/unavailable · junction plates = graded forks and joins · bundle bars = measured member sessions (√, shared scale)'
      : 'rail weight = messages (log) · bundle bars = member sessions with a measured extent (√, one scale for every bundle) · dots = begun, extent unknown';
  return (
    <p className="text-3xs text-text-muted" data-legend-encodings={variant}>
      {treatment} · brightness = recency within the loaded page, dimmest at the oldest start and full at NOW{variant === 'weave' ? ' · select a filament or event to inspect its exact source' : ' · floor ticks = dated events per bin where glyphs would not fit'} · the relation disclosure retains every grade and basis · the selected chain lifts over the recency veil
    </p>
  );
}
