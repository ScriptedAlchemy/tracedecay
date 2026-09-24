/**
 * Overlay marks the rail, weave and strata renderers share: the playback
 * cursor, the `NOW` marker at the newest loaded record, and the 2px cyan
 * focus ring. Loaded-page language only: `NOW` names the newest record in
 * this page and never a live connection.
 */
import type { JSX } from 'react';
import { formatMoment } from '../../../workspaces/loom/tracks.ts';
import { xToTime } from '../layout.ts';
import type { TemporalPalette } from '../palette.ts';
import type { SceneFrame } from './contract.ts';
import { hatch } from './paint.ts';

/** Focus is a stable 2px cyan ring, shown only for keyboard focus. */
export const FOCUS_NODE_CLASS =
  'cursor-pointer outline-none [&:focus-visible>.td-focus-ring]:opacity-100';

export function FocusRing({ x, y, r }: { x: number; y: number; r: number }): JSX.Element {
  return (
    <circle
      className="td-focus-ring"
      cx={x}
      cy={y}
      r={r}
      fill="none"
      stroke="var(--raw-graph-accent)"
      strokeWidth={2}
      opacity={0}
      pointerEvents="none"
    />
  );
}

export function CursorAndTail({ frame }: { frame: SceneFrame }): JSX.Element {
  const { model, fieldX0, fieldX1, top, height, tailLabel, density } = frame;
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
        : 'CURSOR · recorded order';
  return (
    <g pointerEvents="none" style={{ fontFamily: 'var(--font-mono)' }}>
      {tailX !== null && (
        <g data-tail-marker data-tail-x={Math.round(tailX)}>
          <title>{`${tailLabel} = newest record in this loaded page${tailTime === null ? '' : ` · ${formatMoment(tailTime)}`} · not a live stream`}</title>
          <line x1={tailX + 0.5} x2={tailX + 0.5} y1={top - 6} y2={height} stroke="var(--raw-graph-accent)" strokeWidth={1} opacity={0.55} strokeDasharray="2 3" />
          <path d={`M ${tailX - 4} ${top - 9} H ${tailX + 4} L ${tailX} ${top - 3} Z`} fill="var(--raw-graph-accent)" />
          <text x={Math.min(tailX, fieldX1 - 2)} y={11} fontSize={9} letterSpacing="0.16em" textAnchor={tailX > fieldX1 - 30 ? 'end' : 'middle'} fill="var(--raw-graph-accent)">
            {tailLabel}
          </text>
        </g>
      )}
      {tailX === null && tailLater && (
        <g data-tail-marker data-tail-x="later">
          <title>{`${tailLabel} lies after this window`}</title>
          <text x={fieldX1 - 2} y={11} fontSize={9} letterSpacing="0.16em" textAnchor="end" fill="var(--raw-graph-accent)">
            {`${tailLabel} →`}
          </text>
        </g>
      )}
      {cursor && cursor.x >= fieldX0 && cursor.x <= fieldX1 && (
        <g data-cursor-mark data-cursor-basis={cursor.xBasis}>
          {cursor.xBasis === 'time' ? (
            <>
              <line data-cursor x1={cursor.x} x2={cursor.x} y1={top - 10} y2={height} stroke="var(--raw-graph-accent)" strokeWidth={1} />
              <rect x={cursor.x - 1.5} y={top - 12} width={3} height={6} fill="var(--raw-graph-accent)" />
            </>
          ) : (
            // A recorded-order position is not a time: the cursor spans only its own lane.
            <line data-cursor x1={cursor.x} x2={cursor.x} y1={cursorLane ? cursorLane.y - cursorLane.height / 2 : top} y2={cursorLane ? cursorLane.y + cursorLane.height / 2 : height} stroke="var(--raw-graph-accent)" strokeWidth={1} />
          )}
          {cursorText && (
            <text
              x={cursor.x > fieldX1 - 160 ? cursor.x - 5 : cursor.x + 5}
              y={(cursor.xBasis === 'time' || !cursorLane ? top : cursorLane.y - cursorLane.height / 2) + 11}
              fontSize={9}
              letterSpacing="0.08em"
              textAnchor={cursor.x > fieldX1 - 160 ? 'end' : 'start'}
              fill="var(--raw-graph-accent)"
            >
              {cursorText}
            </text>
          )}
        </g>
      )}
    </g>
  );
}

/** Veils and hatches the band after a dated cursor: those records are
 * withheld, so the band reads as not yet revealed rather than empty. */
export function paintUnrevealed(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX1, top, height } = frame;
  const cursor = model.cursor;
  if (!cursor || cursor.xBasis !== 'time' || cursor.x >= fieldX1) return;
  const x0 = Math.max(frame.fieldX0, cursor.x);
  ctx.save();
  ctx.beginPath();
  ctx.rect(x0, top, fieldX1 - x0, height - top);
  ctx.clip();
  ctx.globalAlpha = 0.5;
  ctx.fillStyle = palette.substrate;
  ctx.fillRect(x0, top, fieldX1 - x0, height - top);
  ctx.globalAlpha = 0.06;
  ctx.strokeStyle = palette.text;
  ctx.lineWidth = 1;
  hatch(ctx, x0, top, fieldX1, height, 9, 1);
  ctx.restore();
}
