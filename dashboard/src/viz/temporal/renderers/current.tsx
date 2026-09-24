/**
 * The shipped temporal renderer: glowing weighted lane threads over provider
 * rails, spawn curves, halo'd event plates and bracketed bundles.
 */
import type { JSX } from 'react';
import { EventGlyph } from '../glyphs.tsx';
import { gradeColorVar, gradeDashArray, gradeStroke, type TemporalPalette } from '../palette.ts';
import type { ScenePath } from '../types.ts';
import type { ClusterMarkProps, NodeMarkProps, SceneFrame, SceneRenderer } from './contract.ts';
import { focusAlpha, tracePath } from './paint.ts';

function strokePath(
  ctx: CanvasRenderingContext2D,
  path: ScenePath,
  palette: TemporalPalette,
  glowWidth: number,
  coreWidth: number,
): void {
  const stroke = gradeStroke(path.grade, palette);
  const alpha = focusAlpha(path.focus);
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';

  ctx.save();
  if (!palette.light) ctx.globalCompositeOperation = 'lighter';
  ctx.globalAlpha = 0.1 * alpha;
  ctx.strokeStyle = stroke.color;
  ctx.lineWidth = glowWidth;
  ctx.setLineDash([]);
  ctx.beginPath();
  tracePath(ctx, path.controls);
  ctx.stroke();
  ctx.restore();

  ctx.globalAlpha = alpha;
  ctx.strokeStyle = stroke.color;
  ctx.lineWidth = coreWidth;
  ctx.setLineDash([...stroke.dash]);
  ctx.beginPath();
  tracePath(ctx, path.controls);
  ctx.stroke();
  ctx.setLineDash([]);
}

function paint(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX0, fieldX1, top, height } = frame;
  const fieldWidth = Math.max(1, fieldX1 - fieldX0);

  ctx.save();
  ctx.beginPath();
  ctx.rect(fieldX0, 0, fieldWidth, height);
  ctx.clip();

  for (const rail of model.rails) {
    ctx.globalAlpha = palette.light ? 0.05 : 0.07;
    ctx.fillStyle = palette.text;
    ctx.fillRect(fieldX0, rail.y0, fieldWidth, rail.y1 - rail.y0);
    ctx.globalAlpha = 0.4;
    ctx.fillStyle = palette.edge;
    ctx.fillRect(fieldX0, rail.y0, fieldWidth, 1);
  }

  ctx.globalAlpha = 0.35;
  ctx.strokeStyle = palette.grid;
  ctx.lineWidth = 1;
  ctx.setLineDash([]);
  for (const tick of model.ticks) {
    const x = Math.round(tick.x) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x, top);
    ctx.lineTo(x, height);
    ctx.stroke();
  }

  // Records stop at a dated cursor. The layout already withholds later ones;
  // this clip keeps a curve's easing from reaching into the unrevealed band.
  const recordX1 = model.cursor && model.cursor.xBasis === 'time' ? Math.min(fieldX1, model.cursor.x) : fieldX1;
  ctx.save();
  ctx.beginPath();
  ctx.rect(fieldX0, 0, Math.max(0, recordX1 - fieldX0), height);
  ctx.clip();

  for (const cluster of model.clusters) {
    const alpha = focusAlpha(cluster.focus);
    const x1 = Math.max(cluster.x1, cluster.x0 + 2);
    const body = ctx.createLinearGradient(cluster.x0, 0, x1, 0);
    body.addColorStop(0, 'transparent');
    body.addColorStop(0.18, palette.signal);
    body.addColorStop(0.82, palette.signal);
    body.addColorStop(1, 'transparent');
    ctx.globalAlpha = 0.18 * alpha;
    ctx.fillStyle = body;
    ctx.fillRect(cluster.x0, cluster.y - cluster.height / 2, x1 - cluster.x0, cluster.height);
    ctx.globalAlpha = 0.7 * alpha;
    ctx.strokeStyle = palette.signalHot;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(cluster.x0, cluster.y);
    ctx.lineTo(x1, cluster.y);
    ctx.stroke();
  }

  for (const path of model.paths) {
    switch (path.kind) {
      case 'lane': {
        const weight = path.weight ?? 0;
        const stroke = gradeStroke(path.grade, palette);
        strokePath(ctx, path, palette, 6 + weight * 10, stroke.width + weight * 1.4);
        break;
      }
      case 'spawn':
      case 'handoff':
      case 'rejoin':
      case 'result': {
        const stroke = gradeStroke(path.grade, palette);
        strokePath(ctx, path, palette, 5, stroke.width);
        break;
      }
      case 'sequence': {
        ctx.globalAlpha = 0.55 * focusAlpha(path.focus);
        ctx.strokeStyle = palette.text;
        ctx.lineWidth = 1;
        ctx.setLineDash([2, 3]);
        ctx.beginPath();
        tracePath(ctx, path.controls);
        ctx.stroke();
        ctx.setLineDash([]);
        break;
      }
      default: {
        const exhaustive: never = path.kind;
        throw new Error(`unknown path kind: ${String(exhaustive)}`);
      }
    }
  }

  for (const node of model.nodes) {
    const stroke = gradeStroke(node.grade, palette);
    const radius = node.selected ? 22 : 13;
    const halo = ctx.createRadialGradient(node.x, node.y, 1, node.x, node.y, radius);
    halo.addColorStop(0, stroke.color);
    halo.addColorStop(1, 'transparent');
    ctx.save();
    if (!palette.light) ctx.globalCompositeOperation = 'lighter';
    ctx.globalAlpha = (node.selected ? 0.42 : 0.26) * focusAlpha(node.focus);
    ctx.fillStyle = halo;
    ctx.beginPath();
    ctx.arc(node.x, node.y, radius, 0, Math.PI * 2);
    ctx.fill();
    ctx.restore();
  }
  ctx.restore();

  if (model.cursor && model.cursor.x < fieldX1) {
    const x0 = Math.max(fieldX0, model.cursor.x);
    const bandHeight = height - top;
    ctx.save();
    ctx.beginPath();
    ctx.rect(x0, top, fieldX1 - x0, bandHeight);
    ctx.clip();
    ctx.globalAlpha = 0.05;
    ctx.strokeStyle = palette.text;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let x = x0 - bandHeight; x < fieldX1; x += 8) {
      ctx.moveTo(x, height);
      ctx.lineTo(x + bandHeight, top);
    }
    ctx.stroke();
    ctx.restore();
  }

  ctx.restore();
}

function NodeMark({ node, hovered }: NodeMarkProps): JSX.Element {
  const color = gradeColorVar(node.grade);
  const haloRadius = node.selected ? 13 : hovered ? 11 : 0;
  return (
    <>
      <circle
        className="td-focus-ring"
        cx={node.x}
        cy={node.y}
        r={haloRadius}
        fill="none"
        stroke={node.grade === 'ambiguous' ? 'var(--raw-graph-alert)' : 'var(--raw-graph-accent)'}
        strokeWidth={1.2}
        opacity={0.8}
        pointerEvents="none"
      />
      <circle
        cx={node.x}
        cy={node.y}
        r={7}
        fill="var(--raw-graph-substrate)"
        stroke={color}
        strokeWidth={1.2}
        strokeDasharray={gradeDashArray(node.grade) || undefined}
        pointerEvents="none"
      />
      <g transform={`translate(${node.x} ${node.y})`} color={color} pointerEvents="none">
        <EventGlyph kind={node.kind} size={12} />
      </g>
    </>
  );
}

function ClusterMark({ cluster }: ClusterMarkProps): JSX.Element {
  const x1 = Math.max(cluster.x1, cluster.x0 + 2);
  const top = cluster.y - cluster.height / 2;
  const bracket = 6;
  const outline = [
    `M ${cluster.x0} ${top + bracket} V ${top} H ${cluster.x0 + bracket}`,
    `M ${x1 - bracket} ${top} H ${x1} V ${top + bracket}`,
    `M ${x1} ${top + cluster.height - bracket} V ${top + cluster.height} H ${x1 - bracket}`,
    `M ${cluster.x0 + bracket} ${top + cluster.height} H ${cluster.x0} V ${top + cluster.height - bracket}`,
  ].join(' ');
  return (
    <>
      <rect x={cluster.x0} y={top} width={x1 - cluster.x0} height={cluster.height} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={1} opacity={0.35} strokeDasharray="6 4" />
      <path d={outline} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={1.4} />
      <text x={cluster.x0 + bracket + 2} y={top - 3} fontSize={9} fill="var(--raw-graph-text)" pointerEvents="none">
        {cluster.counts.sessions} sessions
      </text>
    </>
  );
}

function FieldOverlay({ frame }: { frame: SceneFrame }): JSX.Element {
  const { model, fieldX1, top, height, tailLabel } = frame;
  return (
    <g pointerEvents="none">
      <line data-tail-marker x1={fieldX1 + 0.5} x2={fieldX1 + 0.5} y1={top - 8} y2={height} stroke="var(--raw-graph-accent)" strokeWidth={1} opacity={0.7} />
      <text x={fieldX1 - 4} y={11} fontSize={10} textAnchor="end" fill="var(--raw-graph-accent)">
        {tailLabel}
      </text>
      {model.cursor && (
        <g data-cursor-mark>
          <line data-cursor x1={model.cursor.x} x2={model.cursor.x} y1={top} y2={height} stroke="var(--raw-graph-text)" strokeWidth={1} opacity={0.7} />
          <path d={`M ${model.cursor.x - 4} ${top - 7} L ${model.cursor.x + 4} ${top - 7} L ${model.cursor.x} ${top - 1} Z`} fill="var(--raw-graph-text)" />
        </g>
      )}
    </g>
  );
}

export const currentRenderer: SceneRenderer = {
  id: 'current',
  label: 'Current weave',
  paint,
  NodeMark,
  ClusterMark,
  FieldOverlay,
  nodeClassName: 'cursor-pointer outline-none [&:focus>circle.td-focus-ring]:stroke-white',
};
