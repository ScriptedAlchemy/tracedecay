/**
 * Woven loom: every session is a warp thread and time is the weft. A child
 * thread leaves its parent at the recorded spawn instant and bends into its
 * own row along the layout's curve, so delegation reads as one continuous
 * strand. Thickness is the session's measured message count (log scale);
 * brightness is recency within the loaded page, dimmest at the oldest start
 * and full at `NOW`. A collapsed branch is a ribbon whose width at each bin
 * is the count of member sessions with a measured extent there. Nothing
 * animates; the picture is the same with reduced motion.
 */
import type { JSX } from 'react';
import { timeToX } from '../layout.ts';
import { EventGlyph } from '../glyphs.tsx';
import { gradeColorVar, gradeDashArray, gradeStroke, type TemporalPalette } from '../palette.ts';
import type { SceneLane, ScenePath } from '../types.ts';
import type { ClusterMarkProps, NodeMarkProps, SceneFrame, SceneRenderer } from './contract.ts';
import { CursorAndTail, FOCUS_NODE_CLASS, FocusRing, paintUnrevealed } from './marks.tsx';
import { focusAlpha } from './paint.ts';

const RIBBON_PX_PER_SQRT = 3.2;
/** Veil over the oldest end of the page; `NOW` carries none. */
const OLDEST_VEIL = 0.45;
/** Below this gap between marks, events become knots rather than glyph discs. */
const KNOT_PX = 13;

function threadWidth(path: ScenePath): number {
  return 1.1 + (path.weight ?? 0) * 3.6;
}

/** One strand per lane: from the parent through the spawn bend when the
 * layout drew one, otherwise from the lane's own start. */
function traceThread(ctx: CanvasRenderingContext2D, lane: SceneLane, spawn: ScenePath | undefined): void {
  const x1 = Math.max(lane.x1, lane.x0 + 1);
  const c = spawn?.controls;
  if (c && c.length >= 8) {
    ctx.moveTo(c[0]!, c[1]!);
    ctx.bezierCurveTo(c[2]!, c[3]!, c[4]!, c[5]!, c[6]!, c[7]!);
    ctx.lineTo(Math.max(x1, c[6]!), lane.y);
    return;
  }
  ctx.moveTo(lane.x0, lane.y);
  ctx.lineTo(x1, lane.y);
}

function paintRibbon(ctx: CanvasRenderingContext2D, frame: SceneFrame, lane: SceneLane, palette: TemporalPalette, alpha: number): void {
  const density = frame.density?.lanes.get(lane.id);
  if (!density) return;
  const maxHalf = lane.height / 2 - 2;
  const half = (count: number): number => (count > 0 ? Math.min(maxHalf, 0.8 + Math.sqrt(count) * RIBBON_PX_PER_SQRT) : 0);
  const bins = density.bins;
  // Measured body.
  ctx.globalAlpha = 0.28 * alpha;
  ctx.fillStyle = palette.signal;
  ctx.beginPath();
  bins.forEach((bin, index) => {
    const h = half(bin.active);
    if (index === 0) ctx.moveTo(bin.x0, lane.y - h);
    ctx.lineTo((bin.x0 + bin.x1) / 2, lane.y - h);
  });
  for (let index = bins.length - 1; index >= 0; index -= 1) {
    const bin = bins[index]!;
    ctx.lineTo((bin.x0 + bin.x1) / 2, lane.y + half(bin.active));
  }
  ctx.closePath();
  ctx.fill();
  // Edges, so the envelope reads crisply against the veil.
  ctx.globalAlpha = 0.8 * alpha;
  ctx.strokeStyle = palette.signal;
  ctx.lineWidth = 1;
  for (const sign of [-1, 1]) {
    ctx.beginPath();
    let drawing = false;
    for (const bin of bins) {
      const h = half(bin.active);
      if (h === 0) {
        drawing = false;
        continue;
      }
      const x = (bin.x0 + bin.x1) / 2;
      if (drawing) ctx.lineTo(x, lane.y + sign * h);
      else ctx.moveTo(x, lane.y + sign * h);
      drawing = true;
    }
    ctx.stroke();
  }
  // Begun with no recorded end: a dotted centre strand, never body.
  ctx.globalAlpha = 0.7 * alpha;
  ctx.strokeStyle = palette.dim;
  ctx.setLineDash([1, 3]);
  ctx.beginPath();
  let run = false;
  for (const bin of bins) {
    if (bin.open > 0 && bin.active === 0) {
      if (!run) ctx.moveTo(bin.x0, lane.y);
      ctx.lineTo(bin.x1, lane.y);
      run = true;
    } else run = false;
  }
  ctx.stroke();
  ctx.setLineDash([]);
}

function paint(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX0, fieldX1, top, height, density } = frame;
  const width = Math.max(1, fieldX1 - fieldX0);
  ctx.save();
  ctx.beginPath();
  ctx.rect(fieldX0, top, width, height - top);
  ctx.clip();

  ctx.globalAlpha = 0.22;
  ctx.strokeStyle = palette.grid;
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (const tick of model.ticks) {
    ctx.moveTo(Math.round(tick.x) + 0.5, top);
    ctx.lineTo(Math.round(tick.x) + 0.5, height);
  }
  ctx.stroke();
  for (const rail of model.rails.slice(1)) {
    ctx.globalAlpha = 0.6;
    ctx.fillStyle = palette.grid;
    ctx.fillRect(fieldX0, Math.round(rail.y0 - 5), width, 1);
  }

  const spawnInto = new Map<string, ScenePath>();
  const links: ScenePath[] = [];
  for (const path of model.paths) {
    if (path.kind === 'spawn' && !spawnInto.has(path.toId)) spawnInto.set(path.toId, path);
    else if (path.kind === 'handoff' || path.kind === 'rejoin' || path.kind === 'result') links.push(path);
  }
  const lanePaths = new Map(model.paths.filter((path) => path.kind === 'lane').map((path) => [path.fromId, path] as const));
  // Context threads first; the selected thread is pulled forward, drawn last.
  const order = [...model.lanes].sort((a, b) => focusAlpha(a.focus) - focusAlpha(b.focus) || (a.focus === 'selected' ? 1 : 0) - (b.focus === 'selected' ? 1 : 0) || a.row - b.row);
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  for (const lane of order) {
    const lanePath = lanePaths.get(lane.id);
    if (!lanePath || !lane.revealed) continue;
    const alpha = focusAlpha(lane.focus);
    if (lane.kind === 'bundle') {
      paintRibbon(ctx, frame, lane, palette, alpha);
      // The root's own thread runs through its ribbon.
      const stroke = gradeStroke(lanePath.grade, palette);
      ctx.globalAlpha = alpha;
      ctx.strokeStyle = stroke.color;
      ctx.lineWidth = threadWidth(lanePath);
      ctx.setLineDash([...stroke.dash]);
      ctx.beginPath();
      ctx.moveTo(lane.x0, lane.y);
      ctx.lineTo(Math.max(lane.x1, lane.x0 + 1), lane.y);
      ctx.stroke();
      ctx.setLineDash([]);
      continue;
    }
    const spawn = spawnInto.get(lane.id);
    const w = threadWidth(lanePath) + (lane.focus === 'selected' ? 1.2 : 0);
    // Casing: the strand passes over what it crosses.
    ctx.globalAlpha = 1;
    ctx.strokeStyle = palette.substrate;
    ctx.lineWidth = w + (lane.focus === 'selected' ? 5 : 3);
    ctx.setLineDash([]);
    ctx.beginPath();
    traceThread(ctx, lane, spawn);
    ctx.stroke();
    // The bend carries the spawn's grade; the run carries the extent's.
    ctx.beginPath();
    if (spawn && spawn.controls.length >= 8) {
      const c = spawn.controls;
      const spawnStroke = gradeStroke(spawn.grade, palette);
      ctx.globalAlpha = focusAlpha(spawn.focus);
      ctx.strokeStyle = lane.focus === 'selected' ? palette.signalHot : spawnStroke.color;
      ctx.lineWidth = Math.max(1.2, w * 0.8);
      ctx.setLineDash([...spawnStroke.dash]);
      ctx.moveTo(c[0]!, c[1]!);
      ctx.bezierCurveTo(c[2]!, c[3]!, c[4]!, c[5]!, c[6]!, c[7]!);
      ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(c[6]!, lane.y);
      ctx.lineTo(Math.max(lane.x1, c[6]!), lane.y);
    } else {
      ctx.moveTo(lane.x0, lane.y);
      ctx.lineTo(Math.max(lane.x1, lane.x0 + 1), lane.y);
    }
    const stroke = gradeStroke(lanePath.grade, palette);
    ctx.globalAlpha = alpha;
    ctx.strokeStyle = lane.focus === 'selected' ? palette.signalHot : stroke.color;
    ctx.lineWidth = w;
    ctx.setLineDash([...stroke.dash]);
    ctx.stroke();
    ctx.setLineDash([]);
  }

  for (const path of links) {
    const stroke = gradeStroke(path.grade, palette);
    ctx.globalAlpha = focusAlpha(path.focus);
    ctx.strokeStyle = stroke.color;
    ctx.lineWidth = 1.2;
    ctx.setLineDash([...stroke.dash]);
    ctx.beginPath();
    ctx.moveTo(path.controls[0]!, path.controls[1]!);
    ctx.bezierCurveTo(path.controls[2]!, path.controls[3]!, path.controls[4]!, path.controls[5]!, path.controls[6]!, path.controls[7]!);
    ctx.stroke();
  }
  ctx.setLineDash([]);
  for (const path of model.paths) {
    if (path.kind !== 'sequence') continue;
    ctx.globalAlpha = 0.5 * focusAlpha(path.focus);
    ctx.strokeStyle = palette.text;
    ctx.lineWidth = 1;
    ctx.setLineDash([1, 3]);
    ctx.beginPath();
    ctx.moveTo(path.controls[0]!, path.controls[1]!);
    ctx.lineTo(path.controls[2]!, path.controls[3]!);
    ctx.stroke();
  }
  ctx.setLineDash([]);

  // Recency: a veil from the oldest start in the page to NOW. Marks in the
  // overlay are not veiled, so dim never means illegible.
  const headX = density?.headTime != null ? timeToX(model.viewport, density.headTime) : null;
  const tailX = density?.tailTime != null ? timeToX(model.viewport, density.tailTime) : null;
  if (headX !== null && tailX !== null && tailX > headX) {
    const veil = ctx.createLinearGradient(headX, 0, tailX, 0);
    veil.addColorStop(0, palette.substrate);
    veil.addColorStop(1, 'transparent');
    ctx.globalAlpha = OLDEST_VEIL;
    ctx.fillStyle = veil;
    ctx.fillRect(fieldX0, top, width, height - top);
  }
  ctx.restore();
  paintUnrevealed(ctx, frame, palette);
}

function NodeMark({ node, hovered, frame }: NodeMarkProps): JSX.Element {
  const color = gradeColorVar(node.grade);
  const crowded = (frame.density?.lanes.get(node.laneId)?.minGap ?? Infinity) < KNOT_PX;
  if (crowded && !node.selected) {
    // Knots: where glyph discs would overlap, each event is a bead on the thread.
    return (
      <>
        {hovered && <circle cx={node.x} cy={node.y} r={5} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={1} pointerEvents="none" />}
        <FocusRing x={node.x} y={node.y} r={6} />
        <circle cx={node.x} cy={node.y} r={2.2} fill={color} stroke="var(--raw-graph-substrate)" strokeWidth={1} pointerEvents="none" />
      </>
    );
  }
  const r = node.kind === 'session_start' || node.kind === 'session_end' ? 3.2 : 6;
  return (
    <>
      {(node.selected || hovered) && (
        <circle cx={node.x} cy={node.y} r={r + 4} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={node.selected ? 2 : 1} opacity={node.selected ? 1 : 0.6} pointerEvents="none" />
      )}
      <FocusRing x={node.x} y={node.y} r={r + 7} />
      {r > 4 ? (
        <>
          <circle cx={node.x} cy={node.y} r={r} fill="var(--raw-graph-substrate)" stroke={color} strokeWidth={1} strokeDasharray={gradeDashArray(node.grade) || undefined} pointerEvents="none" />
          <g transform={`translate(${node.x} ${node.y})`} color={color} pointerEvents="none">
            <EventGlyph kind={node.kind} size={9} />
          </g>
        </>
      ) : node.kind === 'session_start' ? (
        <circle cx={node.x} cy={node.y} r={r} fill={color} stroke="var(--raw-graph-substrate)" strokeWidth={1.5} pointerEvents="none" />
      ) : (
        <rect x={node.x - r} y={node.y - r} width={r * 2} height={r * 2} fill="var(--raw-graph-substrate)" stroke={color} strokeWidth={1.3} strokeDasharray={gradeDashArray(node.grade) || undefined} pointerEvents="none" />
      )}
    </>
  );
}

function ClusterMark({ cluster, frame }: ClusterMarkProps): JSX.Element {
  const totals = frame.density?.lanes.get(cluster.laneId)?.totals;
  return (
    <text
      x={cluster.x0 + 4}
      y={cluster.y - cluster.height / 2 + 7}
      fontSize={8}
      letterSpacing="0.12em"
      fill="var(--raw-graph-text)"
      opacity={0.75}
      pointerEvents="none"
      style={{ fontFamily: 'var(--font-mono)' }}
    >
      {totals
        ? `${totals.sessions} THREADS · ${totals.messages.toLocaleString()} MSG`
        : `ROOT + ${cluster.counts.sessions} THREADS`}
    </text>
  );
}

function laneDetail(lane: SceneLane, frame: SceneFrame): string | null {
  const totals = frame.density?.lanes.get(lane.id)?.totals;
  if (!totals || lane.kind !== 'bundle') return null;
  return `${lane.provider} · 1+${totals.sessions - 1} threads`;
}

function FieldOverlay({ frame }: { frame: SceneFrame }): JSX.Element {
  return (
    <g pointerEvents="none">
      {frame.model.lanes
        .filter((lane) => lane.focus === 'selected')
        .map((lane) => (
          <rect key={lane.id} data-selected-gutter x={0} y={lane.y - lane.height / 2} width={2} height={lane.height} fill="var(--raw-graph-accent)" />
        ))}
      <CursorAndTail frame={frame} />
    </g>
  );
}

function LegendEncodings(): JSX.Element {
  return (
    <p className="text-3xs text-text-muted" data-legend-encodings="weave">
      thread thickness = messages (log) · brightness = recency within the loaded page, dimmest at the oldest start and full at NOW · ribbon width = member sessions with a measured extent (√) · dotted strand = begun, extent unknown · beads = events where glyphs would overlap (hover or focus names each)
    </p>
  );
}

export const weaveRenderer: SceneRenderer = {
  id: 'weave',
  label: 'Woven loom',
  paint,
  NodeMark,
  ClusterMark,
  FieldOverlay,
  laneDetail,
  LegendEncodings,
  nodeClassName: FOCUS_NODE_CLASS,
};
