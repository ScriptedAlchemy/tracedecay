/**
 * Rail instrument: the field as a measuring instrument. A 32px graticule,
 * one banded rail per session with hairline separators, kind glyphs on one
 * shared time scale, and causal links routed as orthogonal hairlines whose
 * line style and printed tag carry the evidence grade. Where marks would
 * collide, the lane shows its per-bin event histogram and each event keeps a
 * hairline tick and its button.
 */
import type { JSX } from 'react';
import type { DensityBin, LaneDensity } from '../density.ts';
import { EventGlyph } from '../glyphs.tsx';
import { gradeColorVar, gradeDashArray, gradeStroke, type TemporalPalette } from '../palette.ts';
import type { EvidenceGrade, SceneLane, ScenePath } from '../types.ts';
import type { ClusterMarkProps, NodeMarkProps, SceneFrame, SceneRenderer } from './contract.ts';
import { CursorAndTail, FOCUS_NODE_CLASS, FocusRing, paintUnrevealed } from './marks.tsx';
import { focusAlpha, hatch, pathEnds } from './paint.ts';

const GRATICULE_PX = 32;
/** Below this gap between neighbouring marks a lane switches to its histogram. */
const COLLIDE_PX = 9;
const CORNER_PX = 4;
/** Above this many drawn links, non-EXACT tags print only on the focused chain. */
const TAGGED_LINKS_MAX = 40;

const GRADE_TAG: Readonly<Record<EvidenceGrade, string>> = {
  exact: 'EXACT',
  explicit: 'EXPLICIT',
  inferred: 'INFERRED',
  ambiguous: 'AMBIGUOUS',
  stale: 'STALE',
  unavailable: 'UNAVAILABLE',
};

function collides(frame: SceneFrame, laneId: string): LaneDensity | null {
  const lane = frame.density?.lanes.get(laneId);
  return lane && lane.minGap < COLLIDE_PX ? lane : null;
}

function isLink(path: ScenePath): boolean {
  return path.kind === 'spawn' || path.kind === 'handoff' || path.kind === 'rejoin' || path.kind === 'result';
}

/** The orthogonal route of a relation: down the relation instant, then along
 * the child rail. The layout's cubic is centred on that instant. */
function traceRoute(ctx: CanvasRenderingContext2D, path: ScenePath): void {
  const { x0, y0, x1, y1 } = pathEnds(path.controls);
  const x = (x0 + x1) / 2;
  const dir = y1 >= y0 ? 1 : -1;
  const r = Math.min(CORNER_PX, Math.abs(y1 - y0) / 2);
  ctx.moveTo(x, y0);
  ctx.lineTo(x, y1 - dir * r);
  ctx.arcTo(x, y1, x + r, y1, r);
  ctx.lineTo(x1, y1);
}

function paintHistogram(ctx: CanvasRenderingContext2D, lane: SceneLane, density: LaneDensity, palette: TemporalPalette, alpha: number): void {
  const floor = lane.y + lane.height / 2 - 2;
  const room = Math.max(4, lane.height / 2 - 4);
  const peak = Math.max(1, density.peak.active + density.peak.open);
  const width = (bin: DensityBin): number => Math.max(1, bin.x1 - bin.x0 - 1);
  // One path per layer: a dense page is hundreds of bins per lane.
  const layer = (color: string, layerAlpha: number, rect: (bin: DensityBin) => readonly [number, number, number, number] | null): void => {
    ctx.beginPath();
    for (const bin of density.bins) {
      const r = rect(bin);
      if (r) ctx.rect(r[0], r[1], r[2], r[3]);
    }
    ctx.globalAlpha = layerAlpha * alpha;
    ctx.fillStyle = color;
    ctx.fill();
  };
  // A single session's coverage is its rail; only a bundle has a count to bar.
  if (lane.kind === 'bundle') {
    layer(palette.signal, 0.55, (bin) => {
      const h = (bin.active / peak) * room;
      return h > 0 ? [bin.x0, floor - h, width(bin), h] : null;
    });
    layer(palette.dim, 0.5, (bin) => {
      const activeH = (bin.active / peak) * room;
      const h = (bin.open / peak) * room;
      return h > 0 ? [bin.x0, floor - activeH - h, width(bin), h] : null;
    });
  }
  layer(palette.text, 0.6, (bin) => {
    if (bin.events === 0) return null;
    const h = Math.min(room, 1 + Math.log2(1 + bin.events) * 1.5);
    return [bin.x0 + width(bin) / 2 - 0.5, lane.y - lane.height / 2 + 3, 1, h];
  });
}

function paint(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX0, fieldX1, top, height } = frame;
  const width = Math.max(1, fieldX1 - fieldX0);
  ctx.save();
  ctx.beginPath();
  ctx.rect(fieldX0, top, width, height - top);
  ctx.clip();
  ctx.lineWidth = 1;
  ctx.setLineDash([]);

  // Graticule: 32px minor rules as texture, the labelled ticks as the majors.
  ctx.strokeStyle = palette.grid;
  ctx.globalAlpha = palette.light ? 0.35 : 0.3;
  ctx.beginPath();
  for (let x = fieldX0 + GRATICULE_PX; x < fieldX1; x += GRATICULE_PX) {
    ctx.moveTo(Math.round(x) + 0.5, top);
    ctx.lineTo(Math.round(x) + 0.5, height);
  }
  ctx.stroke();
  ctx.strokeStyle = palette.edge;
  ctx.globalAlpha = 0.35;
  ctx.beginPath();
  for (const tick of model.ticks) {
    ctx.moveTo(Math.round(tick.x) + 0.5, top);
    ctx.lineTo(Math.round(tick.x) + 0.5, height);
  }
  ctx.stroke();

  // Lane bands with hairline separators; the selected band carries the tint.
  model.lanes.forEach((lane) => {
    const y0 = lane.y - lane.height / 2;
    if (lane.focus === 'selected' || lane.focus === 'path') {
      ctx.globalAlpha = lane.focus === 'selected' ? 0.08 : 0.035;
      ctx.fillStyle = palette.signal;
      ctx.fillRect(fieldX0, y0, width, lane.height);
    } else if (lane.row % 2 === 1) {
      ctx.globalAlpha = palette.light ? 0.03 : 0.022;
      ctx.fillStyle = palette.text;
      ctx.fillRect(fieldX0, y0, width, lane.height);
    }
    ctx.globalAlpha = 0.7;
    ctx.fillStyle = palette.grid;
    ctx.fillRect(fieldX0, Math.round(y0 + lane.height) - 1, width, 1);
  });
  for (const rail of model.rails) {
    ctx.globalAlpha = 0.9;
    ctx.fillStyle = palette.edge;
    ctx.fillRect(fieldX0, Math.round(rail.y0), width, 1);
  }

  const laneById = new Map(model.lanes.map((lane) => [lane.id, lane] as const));
  // Rails, or a histogram where marks would collide or the lane is a bundle.
  for (const path of model.paths) {
    if (path.kind !== 'lane') continue;
    const lane = laneById.get(path.fromId);
    if (!lane) continue;
    const alpha = focusAlpha(path.focus);
    const density = frame.density?.lanes.get(lane.id) ?? null;
    if (density && (lane.kind === 'bundle' || density.minGap < COLLIDE_PX)) {
      paintHistogram(ctx, lane, density, palette, alpha);
    }
    const stroke = gradeStroke(path.grade, palette);
    ctx.globalAlpha = alpha;
    ctx.strokeStyle = path.focus === 'selected' ? palette.signalHot : stroke.color;
    ctx.lineWidth = (path.focus === 'selected' ? 1.5 : 1) + (path.weight ?? 0) * 1.5;
    ctx.setLineDash([...stroke.dash]);
    ctx.beginPath();
    ctx.moveTo(lane.x0, Math.round(lane.y) + 0.5);
    ctx.lineTo(Math.max(lane.x1, lane.x0 + 1), Math.round(lane.y) + 0.5);
    ctx.stroke();
  }

  // Causal links: routed hairlines, context first so the focused chain lands on top.
  const links = model.paths.filter(isLink).sort((a, b) => focusAlpha(a.focus) - focusAlpha(b.focus));
  ctx.lineCap = 'butt';
  for (const path of links) {
    const stroke = gradeStroke(path.grade, palette);
    ctx.globalAlpha = focusAlpha(path.focus);
    ctx.strokeStyle = stroke.color;
    ctx.lineWidth = path.focus === 'selected' || path.focus === 'path' ? 1.5 : 1;
    ctx.setLineDash([...stroke.dash]);
    ctx.beginPath();
    traceRoute(ctx, path);
    ctx.stroke();
  }
  ctx.setLineDash([]);

  // Undated turns keep their recorded order as a dotted ladder.
  ctx.strokeStyle = palette.text;
  for (const path of model.paths) {
    if (path.kind !== 'sequence') continue;
    ctx.globalAlpha = 0.45 * focusAlpha(path.focus);
    ctx.setLineDash([1, 3]);
    ctx.beginPath();
    ctx.moveTo(path.controls[0]!, path.controls[1]!);
    ctx.lineTo(path.controls[2]!, path.controls[3]!);
    ctx.stroke();
  }
  ctx.setLineDash([]);

  // Where a lane has no measured extent the rail ends in a hatched stub.
  ctx.strokeStyle = palette.dim;
  for (const lane of model.lanes) {
    if (lane.endSource !== null || lane.offscreen || !lane.revealed || lane.kind === 'bundle') continue;
    ctx.save();
    ctx.beginPath();
    ctx.rect(lane.x0, lane.y - 3, 18, 6);
    ctx.clip();
    ctx.globalAlpha = 0.8 * focusAlpha(lane.focus);
    hatch(ctx, lane.x0, lane.y - 3, lane.x0 + 18, lane.y + 3, 3, 1);
    ctx.restore();
  }

  ctx.restore();
  paintUnrevealed(ctx, frame, palette);
}

function NodeMark({ node, hovered, frame }: NodeMarkProps): JSX.Element {
  const color = gradeColorVar(node.grade);
  if (collides(frame, node.laneId) && !node.selected) {
    return (
      <>
        <FocusRing x={node.x} y={node.y} r={6} />
        <line x1={node.x} x2={node.x} y1={node.y - 4} y2={node.y + 4} stroke={color} strokeWidth={1} pointerEvents="none" />
      </>
    );
  }
  return (
    <>
      {(node.selected || hovered) && (
        <rect
          x={node.x - 10}
          y={node.y - 10}
          width={20}
          height={20}
          fill="none"
          stroke="var(--raw-graph-accent)"
          strokeWidth={node.selected ? 2 : 1}
          opacity={node.selected ? 1 : 0.6}
          pointerEvents="none"
        />
      )}
      <FocusRing x={node.x} y={node.y} r={13} />
      <rect
        x={node.x - 7}
        y={node.y - 7}
        width={14}
        height={14}
        fill="var(--raw-graph-substrate)"
        stroke={color}
        strokeWidth={1}
        strokeDasharray={gradeDashArray(node.grade) || undefined}
        pointerEvents="none"
      />
      <g transform={`translate(${node.x} ${node.y})`} color={color} pointerEvents="none">
        <EventGlyph kind={node.kind} size={11} />
      </g>
    </>
  );
}

function ClusterMark({ cluster }: ClusterMarkProps): JSX.Element {
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

function FieldOverlay({ frame }: { frame: SceneFrame }): JSX.Element {
  const { model, fieldX1 } = frame;
  const links = model.paths.filter(isLink);
  const tagAll = links.length <= TAGGED_LINKS_MAX;
  // Rail legends are engraved vertically in the right gutter, clear of every mark.
  const gutterX = fieldX1 + (model.viewport.right + 8) / 2;
  return (
    <g pointerEvents="none">
      {model.rails.map((rail) => {
        const text = `${rail.label.toUpperCase()} · ${rail.lanes}`;
        if (rail.y1 - rail.y0 < text.length * 6 + 8) return null;
        const cy = (rail.y0 + rail.y1) / 2;
        return (
          <g key={rail.id}>
            <line x1={fieldX1 + 4} x2={fieldX1 + 4} y1={rail.y0 + 2} y2={rail.y1 - 2} stroke="var(--raw-graph-edge)" strokeWidth={1} />
            <text
              x={gutterX}
              y={cy}
              transform={`rotate(-90 ${gutterX} ${cy})`}
              fontSize={8.5}
              letterSpacing="0.16em"
              textAnchor="middle"
              dominantBaseline="central"
              fill="var(--raw-graph-text)"
              opacity={0.6}
              style={{ fontFamily: 'var(--font-display)', fontStretch: '112%' }}
            >
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
        const focused = path.focus === 'selected' || path.focus === 'path';
        // Solid EXACT is the default reading; every other grade is tagged.
        if (!focused && (path.grade === 'exact' || !tagAll)) return null;
        const x = (x0 + x1) / 2;
        return (
          <text
            key={path.id}
            data-link-grade={path.grade}
            x={x - 4}
            y={(y0 + y1) / 2 + 3}
            fontSize={8}
            letterSpacing="0.1em"
            textAnchor="end"
            fill={gradeColorVar(path.grade)}
            opacity={focusAlpha(path.focus)}
            style={{ fontFamily: 'var(--font-mono)' }}
          >
            {GRADE_TAG[path.grade]}
          </text>
        );
      })}
      <CursorAndTail frame={frame} />
    </g>
  );
}

function laneDetail(lane: SceneLane, frame: SceneFrame): string | null {
  const density = frame.density?.lanes.get(lane.id);
  if (!density) return null;
  const { totals, peak } = density;
  const commits = totals.commits > 0 ? ` · ${totals.commits} ${totals.commits === 1 ? 'commit' : 'commits'}` : '';
  return lane.kind === 'bundle'
    ? `1+${totals.sessions - 1} sess · ${totals.messages.toLocaleString()} msg · peak ${peak.active}`
    : `${lane.provider} · ${totals.messages.toLocaleString()} msg${commits}`;
}

function LegendEncodings(): JSX.Element {
  return (
    <p className="text-3xs text-text-muted" data-legend-encodings="rail">
      rail weight = messages (log) · bars = member sessions with a measured extent per bin · grey bars = begun, extent unknown · top ticks = dated events per bin · a lane whose marks would collide keeps hairline ticks · link tags print every non-EXACT grade
    </p>
  );
}

export const railRenderer: SceneRenderer = {
  id: 'rail',
  label: 'Rail instrument',
  paint,
  NodeMark,
  ClusterMark,
  FieldOverlay,
  laneDetail,
  LegendEncodings,
  nodeClassName: FOCUS_NODE_CLASS,
};
