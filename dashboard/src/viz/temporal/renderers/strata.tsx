/**
 * Stratified event field: aggregation first. Each row is a stratum; a
 * session's stratum is its extent band, a collapsed branch's stratum is a
 * histogram of member sessions with a measured extent, both on one shared
 * scale, with exact totals in the lane column. Where a stratum's marks are
 * far enough apart it resolves to individual glyphs; otherwise it stays a
 * field of per-bin event ticks. Spawns are junction plates bridging parent
 * and child strata. Evidence grade is a fill pattern, the same one the
 * legend shows, so it survives monochrome.
 */
import type { JSX } from 'react';
import { EventGlyph } from '../glyphs.tsx';
import { gradeColorVar, gradeStroke, type TemporalPalette } from '../palette.ts';
import type { EvidenceGrade, SceneLane, ScenePath } from '../types.ts';
import type { ClusterMarkProps, NodeMarkProps, SceneFrame, SceneRenderer } from './contract.ts';
import { CursorAndTail, FOCUS_NODE_CLASS, FocusRing, paintUnrevealed } from './marks.tsx';
import { focusAlpha, hatch, pathEnds } from './paint.ts';

/** A stratum resolves to glyphs once its marks sit this far apart. */
const RESOLVE_PX = 10;
const PLATE_W = 4;
const BAND_INSET = 3;

/** The typed-state hatch per grade: solid for source facts, diagonals for
 * correlations, crosshatch for unresolved candidates, dots for missing. */
type GradeFill = 'solid' | 'light' | 'diagonal' | 'cross' | 'sparse' | 'dots';
const GRADE_FILL: Readonly<Record<EvidenceGrade, GradeFill>> = {
  exact: 'solid',
  explicit: 'light',
  inferred: 'diagonal',
  ambiguous: 'cross',
  stale: 'sparse',
  unavailable: 'dots',
};

function resolved(frame: SceneFrame, lane: SceneLane): boolean {
  if (frame.model.zoom === 'workstream') return false;
  if (lane.expanded) return true;
  const density = frame.density?.lanes.get(lane.id);
  return !density || density.minGap >= RESOLVE_PX;
}

const resolvedByFrame = new WeakMap<SceneFrame, ReadonlySet<string>>();

function resolvedLanes(frame: SceneFrame): ReadonlySet<string> {
  const cached = resolvedByFrame.get(frame);
  if (cached) return cached;
  const ids = new Set(frame.model.lanes.filter((lane) => resolved(frame, lane)).map((lane) => lane.id));
  resolvedByFrame.set(frame, ids);
  return ids;
}

/** Fill a rectangle with the grade's pattern, clipped to it. */
function fillGrade(ctx: CanvasRenderingContext2D, grade: EvidenceGrade, x: number, y: number, w: number, h: number, palette: TemporalPalette, alpha: number): void {
  const color = gradeStroke(grade, palette).color;
  if (w <= 0 || h <= 0) return;
  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, w, h);
  ctx.clip();
  ctx.fillStyle = color;
  ctx.strokeStyle = color;
  ctx.lineWidth = 1;
  switch (GRADE_FILL[grade]) {
    case 'solid':
      ctx.globalAlpha = 0.5 * alpha;
      ctx.fillRect(x, y, w, h);
      break;
    case 'light':
      ctx.globalAlpha = 0.28 * alpha;
      ctx.fillRect(x, y, w, h);
      break;
    case 'diagonal':
      ctx.globalAlpha = 0.6 * alpha;
      hatch(ctx, x, y, x + w, y + h, 4, 1);
      break;
    case 'cross':
      ctx.globalAlpha = 0.7 * alpha;
      hatch(ctx, x, y, x + w, y + h, 4, 1);
      hatch(ctx, x, y, x + w, y + h, 4, -1);
      break;
    case 'sparse':
      ctx.globalAlpha = 0.7 * alpha;
      hatch(ctx, x, y, x + w, y + h, 8, 1);
      break;
    case 'dots':
      ctx.globalAlpha = 0.9 * alpha;
      ctx.beginPath();
      for (let dx = x + 1; dx < x + w; dx += 3) {
        for (let dy = y + 1; dy < y + h; dy += 3) ctx.rect(dx, dy, 1, 1);
      }
      ctx.fill();
      break;
    default: {
      const exhaustive: never = GRADE_FILL[grade];
      throw new Error(`unknown grade fill: ${String(exhaustive)}`);
    }
  }
  ctx.restore();
}

function isLink(path: ScenePath): boolean {
  return path.kind === 'spawn' || path.kind === 'handoff' || path.kind === 'rejoin' || path.kind === 'result';
}

function paint(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX0, fieldX1, top, height, density } = frame;
  const width = Math.max(1, fieldX1 - fieldX0);
  ctx.save();
  ctx.beginPath();
  ctx.rect(fieldX0, top, width, height - top);
  ctx.clip();

  ctx.strokeStyle = palette.grid;
  ctx.lineWidth = 1;
  ctx.globalAlpha = 0.3;
  ctx.beginPath();
  for (const tick of model.ticks) {
    ctx.moveTo(Math.round(tick.x) + 0.5, top);
    ctx.lineTo(Math.round(tick.x) + 0.5, height);
  }
  ctx.stroke();

  // One shared scale across every bundle stratum, so heights compare.
  let bundlePeak = 1;
  for (const lane of model.lanes) {
    if (lane.kind !== 'bundle') continue;
    const peak = density?.lanes.get(lane.id)?.peak;
    if (peak) bundlePeak = Math.max(bundlePeak, peak.active + peak.open);
  }
  const lanePaths = new Map(model.paths.filter((path) => path.kind === 'lane').map((path) => [path.fromId, path] as const));

  for (const lane of model.lanes) {
    const alpha = lane.offscreen || !lane.revealed ? 0.35 : focusAlpha(lane.focus);
    const y0 = lane.y - lane.height / 2 + 1;
    const h = lane.height - 2;
    // The stratum well.
    ctx.globalAlpha = lane.focus === 'selected' ? 0.1 : palette.light ? 0.035 : 0.03;
    ctx.fillStyle = lane.focus === 'selected' ? palette.signal : palette.text;
    ctx.fillRect(fieldX0, y0, width, h);
    if (!lane.revealed) continue;
    const laneDensity = density?.lanes.get(lane.id);
    const bandY = lane.y - Math.min(6, h / 2 - BAND_INSET);
    const bandH = Math.min(12, h - BAND_INSET * 2);
    if (lane.kind === 'bundle' && laneDensity) {
      const floor = y0 + h - BAND_INSET;
      const room = h - BAND_INSET * 2 - 4;
      ctx.beginPath();
      const openBins: { x: number; y: number; w: number; h: number }[] = [];
      for (const bin of laneDensity.bins) {
        const w = Math.max(1, bin.x1 - bin.x0 - 1);
        const activeH = Math.sqrt(bin.active / bundlePeak) * room;
        const openH = Math.sqrt((bin.active + bin.open) / bundlePeak) * room - activeH;
        if (activeH > 0) ctx.rect(bin.x0, floor - activeH, w, activeH);
        if (openH > 0.5) openBins.push({ x: bin.x0, y: floor - activeH - openH, w, h: openH });
      }
      ctx.globalAlpha = 0.6 * alpha;
      ctx.fillStyle = palette.signal;
      ctx.fill();
      if (openBins.length > 0) {
        // One clip over every open bin, one dot field through it.
        ctx.save();
        ctx.beginPath();
        for (const open of openBins) ctx.rect(open.x, open.y, open.w, open.h);
        ctx.clip();
        const first = openBins[0]!;
        const last = openBins[openBins.length - 1]!;
        fillGrade(ctx, 'unavailable', first.x, y0, last.x + last.w - first.x, h, palette, alpha);
        ctx.restore();
      }
    } else {
      const lanePath = lanePaths.get(lane.id);
      const grade: EvidenceGrade = lanePath?.grade ?? 'unavailable';
      const x1 = lane.endSource === null ? lane.x0 + 18 : Math.max(lane.x1, lane.x0 + 2);
      fillGrade(ctx, grade, lane.x0, bandY, x1 - lane.x0, bandH, palette, alpha);
      ctx.globalAlpha = 0.9 * alpha;
      ctx.fillStyle = lane.focus === 'selected' ? palette.signalHot : gradeStroke(grade, palette).color;
      ctx.fillRect(lane.x0, bandY, 1.5, bandH);
    }
    // Where marks do not resolve, a rug of dated event ticks along the floor.
    if (laneDensity && !resolved(frame, lane)) {
      const floor = y0 + h - 1;
      ctx.beginPath();
      for (const bin of laneDensity.bins) {
        if (bin.events === 0) continue;
        const tickH = Math.min(BAND_INSET + 2, 1 + Math.log2(1 + bin.events));
        ctx.rect(bin.x0 + (bin.x1 - bin.x0) / 2 - 0.5, floor - tickH, 1, tickH);
      }
      ctx.globalAlpha = 0.8 * alpha;
      ctx.fillStyle = palette.text;
      ctx.fill();
    }
  }

  // Junction plates: the spawn instant bridging parent and child strata.
  for (const path of model.paths) {
    if (!isLink(path)) continue;
    const { x0, y0, x1, y1 } = pathEnds(path.controls);
    const x = (x0 + x1) / 2;
    const yTop = Math.min(y0, y1);
    const yBottom = Math.max(y0, y1);
    const alpha = focusAlpha(path.focus);
    fillGrade(ctx, path.grade, x - PLATE_W / 2, yTop, PLATE_W, yBottom - yTop, palette, alpha);
    ctx.globalAlpha = alpha;
    ctx.strokeStyle = gradeStroke(path.grade, palette).color;
    ctx.lineWidth = 1;
    ctx.strokeRect(Math.round(x - PLATE_W / 2) + 0.5, yTop + 0.5, PLATE_W, yBottom - yTop - 1);
  }
  for (const path of model.paths) {
    if (path.kind !== 'sequence') continue;
    ctx.globalAlpha = 0.45 * focusAlpha(path.focus);
    ctx.strokeStyle = palette.text;
    ctx.setLineDash([1, 3]);
    ctx.beginPath();
    ctx.moveTo(path.controls[0]!, path.controls[1]!);
    ctx.lineTo(path.controls[2]!, path.controls[3]!);
    ctx.stroke();
  }
  ctx.setLineDash([]);
  ctx.restore();
  paintUnrevealed(ctx, frame, palette);
}

function NodeMark({ node, hovered, frame }: NodeMarkProps): JSX.Element {
  const color = gradeColorVar(node.grade);
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
      {(node.selected || hovered) && (
        <rect x={node.x - 9} y={node.y - 9} width={18} height={18} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={node.selected ? 2 : 1} opacity={node.selected ? 1 : 0.6} pointerEvents="none" />
      )}
      <FocusRing x={node.x} y={node.y} r={12} />
      <rect x={node.x - 6} y={node.y - 6} width={12} height={12} fill="var(--raw-graph-substrate)" stroke={color} strokeWidth={1} pointerEvents="none" />
      <g transform={`translate(${node.x} ${node.y})`} color={color} pointerEvents="none">
        <EventGlyph kind={node.kind} size={10} />
      </g>
    </>
  );
}

function ClusterMark({ cluster }: ClusterMarkProps): JSX.Element {
  const top = cluster.y - cluster.height / 2 + 2;
  const bottom = cluster.y + cluster.height / 2 - 2;
  return (
    <path
      d={`M ${cluster.x0 + 5} ${top} H ${cluster.x0} V ${bottom} H ${cluster.x0 + 5}`}
      fill="none"
      stroke="var(--raw-graph-edge)"
      strokeWidth={1}
    />
  );
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

function laneDetail(lane: SceneLane, frame: SceneFrame): string | null {
  const density = frame.density?.lanes.get(lane.id);
  if (!density) return null;
  const { totals, peak } = density;
  if (lane.kind === 'bundle') {
    return `1+${totals.sessions - 1} sess · ${totals.messages.toLocaleString()} msg · peak ${peak.active}${peak.open > 0 ? ` · ${peak.open} open` : ''}`;
  }
  return `${lane.provider} · ${totals.messages.toLocaleString()} msg · ${totals.events} ev${totals.undated > 0 ? ` (${totals.undated} undated)` : ''}`;
}

const SWATCH_ID = 'td-strata-swatch';

function GradeSwatch({ grade }: { grade: EvidenceGrade }): JSX.Element {
  const color = gradeColorVar(grade);
  const id = `${SWATCH_ID}-${grade}`;
  const fill = GRADE_FILL[grade];
  return (
    <svg width={28} height={10} aria-hidden="true" className="block shrink-0" data-grade-swatch={fill}>
      <defs>
        <pattern id={id} width={fill === 'sparse' ? 8 : 4} height={fill === 'sparse' ? 8 : 4} patternUnits="userSpaceOnUse" patternTransform={fill === 'dots' ? undefined : 'rotate(-45)'}>
          {fill === 'dots' ? (
            <rect x={1} y={1} width={1} height={1} fill={color} />
          ) : fill === 'solid' || fill === 'light' ? (
            <rect width={4} height={4} fill={color} opacity={fill === 'solid' ? 0.5 : 0.28} />
          ) : (
            <>
              <line x1={0} y1={0} x2={0} y2={fill === 'sparse' ? 8 : 4} stroke={color} strokeWidth={1} />
              {fill === 'cross' && <line x1={0} y1={0} x2={4} y2={0} stroke={color} strokeWidth={1} />}
            </>
          )}
        </pattern>
      </defs>
      <rect x={0.5} y={0.5} width={27} height={9} fill={`url(#${id})`} stroke={color} strokeWidth={1} />
    </svg>
  );
}

function LegendEncodings(): JSX.Element {
  return (
    <p className="text-3xs text-text-muted" data-legend-encodings="strata">
      band = session extent, filled by its grade · bundle bars = member sessions with a measured extent (√, one scale for every bundle) · dotted = begun, extent unknown · floor ticks = dated events per bin where glyphs would collide · plates = spawn instants, filled by grade
    </p>
  );
}

export const strataRenderer: SceneRenderer = {
  id: 'strata',
  label: 'Stratified event field',
  paint,
  NodeMark,
  ClusterMark,
  FieldOverlay,
  laneDetail,
  GradeSwatch,
  LegendEncodings,
  nodeClassName: FOCUS_NODE_CLASS,
};
