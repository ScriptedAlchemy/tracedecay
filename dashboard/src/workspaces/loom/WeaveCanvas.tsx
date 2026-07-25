import { useEffect, useMemo, useRef, useState } from 'react';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { cn } from '../../ui/cn';
import { axisTicks, dayBands } from './tracks.ts';
import type { PlacedThread, Weave } from './weave.ts';

/**
 * The weave, drawn.
 *
 * SVG rather than Canvas2D, deliberately. The track engine's canvas path earns
 * its complexity at thousands of spans; this surface draws one mark per session
 * in the served window — a hundred or so — and in exchange for staying in the
 * DOM it gets three things the honesty contract actually needs: real dashed
 * strokes for unmeasured extent, hue that flips with the theme through the same
 * CSS custom properties every other mark in the console uses, and marks an
 * accessibility tool can see. The canvas would have had to reimplement all
 * three, and the third one badly.
 *
 * Nothing in here decides anything. Positions, widths and solidity all arrive
 * from `composeWeave`; this file turns them into pixels and nothing more.
 */

/** Left gutter for the printed time axis. */
const GUTTER = 64;
const RIGHT_PAD = 12;
/** Header strip carrying the host column names. */
const HEAD = 26;
/** Vertical room for the field itself. Constant so a screenshot is stable and
 * the axis pitch does not change under the reader as data arrives. */
const FIELD_HEIGHT = 520;
const TOP_PAD = 10;
const BOTTOM_PAD = 18;

/** How far an unmeasured tail runs before it stops. Fixed, and short: the
 * length must not read as a duration, because it is not one. */
const OPEN_TAIL = 15;
/** The solid head every thread gets, so a thread with no measured end is still
 * a visible mark at the time it genuinely started. */
const HEAD_CAP = 5;

/** Vertical pixels the time axis actually spans. Exported because the caller
 * has to convert a mark's on-screen size into seconds before it can ask
 * `composeWeave` to pack: packing is a question about what OVERLAPS, and what
 * overlaps depends on the scale it is drawn at. */
export const PLOT_HEIGHT = FIELD_HEIGHT - TOP_PAD - BOTTOM_PAD;
/** Vertical room one mark occupies: its head plus its open tail plus a hair of
 * clearance. Two threads closer together than this collide on screen. */
export const MARK_PITCH_PX = HEAD_CAP + OPEN_TAIL + 4;

export function WeaveCanvas({
  weave,
  selectedId,
  onSelect,
  ariaLabel,
}: {
  weave: Weave;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  ariaLabel: string;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(880);

  useEffect(() => {
    const element = hostRef.current;
    if (!element || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width;
      if (next && next > 0) setWidth(next);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const height = HEAD + FIELD_HEIGHT;
  const fieldWidth = Math.max(width - GUTTER - RIGHT_PAD, 80);
  const extent = weave.extent;

  const geometry = useMemo(() => {
    if (!extent) return null;
    const span = Math.max(extent.end - extent.start, 1);
    const plotTop = HEAD + TOP_PAD;
    const plotHeight = Math.max(FIELD_HEIGHT - TOP_PAD - BOTTOM_PAD, 1);
    const y = (time: number) =>
      plotTop + ((time - extent.start) / span) * plotHeight;

    // Every host column gets the same share of the field; inside it, each
    // packed sub-column gets an equal slice of that share. A column with one
    // lane therefore draws a wide, calm thread and a busy column draws several
    // narrow ones, which is the packing telling the truth about congestion.
    const columnWidth = fieldWidth / Math.max(weave.hosts.length, 1);
    const centerOf = (thread: PlacedThread) => {
      const host = weave.hosts[thread.column];
      const lanes = Math.max(host?.lanes ?? 1, 1);
      const laneWidth = columnWidth / lanes;
      return (
        GUTTER + thread.column * columnWidth + (thread.lane + 0.5) * laneWidth
      );
    };
    const maxThickness = (thread: PlacedThread) => {
      const host = weave.hosts[thread.column];
      const lanes = Math.max(host?.lanes ?? 1, 1);
      return Math.max(columnWidth / lanes - 8, 2);
    };
    return { y, columnWidth, centerOf, maxThickness, plotTop, plotHeight, span };
  }, [extent, fieldWidth, weave.hosts]);

  // The axis helpers are written for a horizontal track engine, so they are
  // handed the field's HEIGHT as their length and their `x` output is read as
  // a `y`. The arithmetic is orientation-free; only the name is horizontal.
  const ticks = useMemo(
    () =>
      geometry && extent
        ? axisTicks({ start: extent.start, end: extent.end }, geometry.plotHeight)
        : [],
    [geometry, extent],
  );
  const bands = useMemo(
    () =>
      geometry && extent
        ? dayBands({ start: extent.start, end: extent.end }, geometry.plotHeight)
        : [],
    [geometry, extent],
  );

  return (
    <div ref={hostRef} className="td-well relative w-full border border-edge-subtle">
      <svg
        role="img"
        aria-label={ariaLabel}
        width="100%"
        height={height}
        viewBox={`0 0 ${Math.max(width, 1)} ${height}`}
        className="block"
      >
        {/* Calendar bands: the weave's warp. Alternating tint only, no label
          * inside the field — the axis gutter carries the words. */}
        {geometry
          ? bands
              .filter((band) => band.odd)
              .map((band) => (
                <rect
                  key={`band-${band.time}`}
                  x={GUTTER}
                  y={geometry.plotTop + band.x0}
                  width={fieldWidth}
                  height={Math.max(band.x1 - band.x0, 0)}
                  className="fill-surface-2/40"
                />
              ))
          : null}

        {/* Host column dividers and names. */}
        {geometry
          ? weave.hosts.map((host, index) => {
              const x = GUTTER + index * geometry.columnWidth;
              return (
                <g key={host.id}>
                  {index > 0 ? (
                    <line
                      x1={x}
                      y1={HEAD}
                      x2={x}
                      y2={height}
                      className="stroke-edge-subtle"
                      strokeWidth={1}
                    />
                  ) : null}
                  {/* Label and count in ONE text run: the count rides a tspan
                    * whose dx is measured from the end of the label's last
                    * glyph, so it cannot collide with it. Positioning the two
                    * as separate elements meant guessing the label's rendered
                    * width from its character count, and letterspaced small
                    * caps do not have the width that guess assumed — every
                    * column header printed as "CLAUDE12". */}
                  {/* At 320px a column is ~78px wide and three full host names
                    * ran into each other ("CLAUDE 12CODEX 11CURSOR"). The name
                    * is truncated to what its own column can hold rather than
                    * being allowed to trespass on its neighbour's — a header
                    * that overlaps the next column mislabels it. */}
                  {/* Below ~96px a column cannot hold a host name and its
                    * count without one of them being cut, and a clipped count
                    * ("12" rendered as "1") is worse than no count: it is a
                    * wrong number. The header is simply omitted there. Nothing
                    * is lost — the per-host readout row directly beneath the
                    * canvas names every host with its thread and message
                    * totals, and it wraps rather than truncating. */}
                  {geometry.columnWidth >= HEADER_MIN_WIDTH ? (
                    <>
                      <clipPath id={`weave-head-${index}`}>
                        <rect
                          x={x}
                          y={0}
                          width={Math.max(geometry.columnWidth - 2, 1)}
                          height={HEAD}
                        />
                      </clipPath>
                      {/* Truncation trims the label to an ESTIMATE of the room
                        * its column has; the clip guarantees it. Font metrics
                        * are not knowable here, so the estimate alone kept
                        * letting a count graze the column next door and
                        * mislabel it. */}
                      <text
                        x={x + 6}
                        y={HEAD - 9}
                        clipPath={`url(#weave-head-${index})`}
                        className="fill-text-secondary text-[9px] uppercase tracking-[0.18em]"
                      >
                        {truncateLabel(host.label, geometry.columnWidth)}
                        <tspan dx={7} className="fill-text-muted tracking-normal">
                          {host.count}
                        </tspan>
                      </text>
                    </>
                  ) : null}
                </g>
              );
            })
          : null}
        <line
          x1={GUTTER}
          y1={HEAD}
          x2={Math.max(width - RIGHT_PAD, GUTTER)}
          y2={HEAD}
          className="stroke-edge-strong"
          strokeWidth={1}
        />

        {/* The printed time axis: real labels at real times, running down. */}
        {geometry
          ? ticks.map((tick) => (
              <g key={`tick-${tick.time}`}>
                <line
                  x1={GUTTER - 4}
                  y1={geometry.plotTop + tick.x}
                  x2={Math.max(width - RIGHT_PAD, GUTTER)}
                  y2={geometry.plotTop + tick.x}
                  className="stroke-edge-subtle"
                  strokeWidth={1}
                  strokeOpacity={0.5}
                />
                <text
                  x={GUTTER - 8}
                  y={geometry.plotTop + tick.x + 3}
                  textAnchor="end"
                  className="fill-text-muted text-[9px] tabular-nums"
                >
                  {tick.label}
                </text>
              </g>
            ))
          : null}

        {/* The threads. */}
        {geometry
          ? weave.threads.map((thread) => (
              <Thread
                key={thread.id}
                thread={thread}
                x={geometry.centerOf(thread)}
                y0={geometry.y(thread.start)}
                y1={thread.end != null ? geometry.y(thread.end) : null}
                maxThickness={geometry.maxThickness(thread)}
                selected={selectedId === thread.id}
                dimmed={selectedId != null && selectedId !== thread.id}
                onSelect={onSelect}
              />
            ))
          : null}
      </svg>
    </div>
  );
}

/** Narrowest column that can carry a host name and its count without cutting
 * either. Below this the header is dropped rather than clipped. */
const HEADER_MIN_WIDTH = 96;

/** Letterspaced 9px small caps run about 7.4px per character; the count and its
 * gap need roughly 42px more. Trimmed to at least three characters so a column
 * always carries some identity rather than an ellipsis alone. */
function truncateLabel(label: string, columnWidth: number): string {
  const room = Math.floor((columnWidth - 42) / 7.4);
  if (room >= label.length) return label;
  return `${label.slice(0, Math.max(room - 1, 3))}…`;
}

/**
 * One session.
 *
 * Two shapes, and which one is drawn is a statement about the DATA, not about
 * the session: a thread whose end the store served is a solid bar between two
 * measured times; a thread whose end it did not serve is a solid head at the
 * one time that WAS measured, followed by a dashed stub of fixed length that
 * says "and then, unrecorded". The stub never reaches a tick, so it cannot be
 * mistaken for a duration.
 *
 * A session the store reports at zero messages is drawn hollow — outline only,
 * at the same width the smallest real thread gets. Zero is a reading, and a
 * reading has to be visible; collapsing it to nothing would hide it among the
 * sessions that simply are not there.
 */
function Thread({
  thread,
  x,
  y0,
  y1,
  maxThickness,
  selected,
  dimmed,
  onSelect,
}: {
  thread: PlacedThread;
  x: number;
  y0: number;
  y1: number | null;
  maxThickness: number;
  selected: boolean;
  dimmed: boolean;
  onSelect: (id: string | null) => void;
}) {
  // The width channel gets real range. At a nine-pixel span every thread was
  // within a few pixels of every other one, so the measurement it carries —
  // message count — was drawn but not legible; a 998-turn session has to LOOK
  // heavier than a 12-turn one from across the field. Still clamped to the
  // sub-column, so a wide thread can never overlap its neighbour.
  const thickness = Math.max(Math.min(2 + thread.weight * 26, maxThickness), 2);
  const half = thickness / 2;
  const strokeClass =
    'stroke-[var(--kind-dark)] [[data-theme=light]_&]:stroke-[var(--kind-light)]';
  const fillClass =
    'fill-[var(--kind-dark)] [[data-theme=light]_&]:fill-[var(--kind-light)]';
  const measuredEnd = y1 != null && y1 > y0;
  const bodyEnd = measuredEnd ? y1 : y0 + HEAD_CAP;
  const boundaryClass =
    thread.endSource === 'session_end'
      ? 'stroke-[var(--ev-measured)]'
      : 'stroke-[var(--ev-associated)]';

  return (
    <g
      style={kindColorVars(thread.host)}
      className={cn(
        'cursor-pointer',
        dimmed && 'opacity-25',
        selected && 'opacity-100',
      )}
      onClick={() => onSelect(selected ? null : thread.id)}
      // The table below is the keyboard and screen-reader path (plan 11a
      // archetype 3), so these marks stay out of the tab order rather than
      // duplicating every row as a second focus stop.
      aria-hidden
    >
      {/* Generous invisible hit area: a two-pixel thread is not a click
        * target, and widening the visible mark to make it one would inflate a
        * measured quantity. */}
      <rect
        x={x - Math.max(half, 6)}
        y={y0 - 3}
        width={Math.max(thickness, 12)}
        height={Math.max(bodyEnd - y0 + (measuredEnd ? 6 : OPEN_TAIL + 6), 12)}
        fill="transparent"
      />

      {thread.hollow ? (
        <rect
          x={x - half}
          y={y0}
          width={thickness}
          height={Math.max(bodyEnd - y0, 3)}
          className={cn(strokeClass, 'fill-none')}
          strokeWidth={1}
        />
      ) : (
        <rect
          x={x - half}
          y={y0}
          width={thickness}
          height={Math.max(bodyEnd - y0, 3)}
          className={fillClass}
          fillOpacity={selected ? 1 : 0.82}
        />
      )}

      {/* Boundary provenance is a separate visual channel from host hue:
        * measured session end, associated last-message observation, or
        * unknown open extent. */}
      {measuredEnd ? (
        <line
          x1={x - half - 2}
          y1={bodyEnd}
          x2={x + half + 2}
          y2={bodyEnd}
          className={boundaryClass}
          strokeWidth={2}
        />
      ) : (
        <line
          x1={x}
          y1={bodyEnd + 1}
          x2={x}
          y2={bodyEnd + OPEN_TAIL}
          className="stroke-[var(--ev-unknown)]"
          strokeWidth={Math.min(thickness, 2)}
          strokeDasharray="2 3"
          strokeOpacity={0.7}
        />
      )}

      {/* Subagent threads carry a real flag from the store, so they get a
        * mark: a hairline crossbar at the head. Shape, not hue — the hue is
        * already spent on the host. */}
      {thread.isSubagent ? (
        <line
          x1={x - half - 3}
          y1={y0}
          x2={x + half + 3}
          y2={y0}
          className={strokeClass}
          strokeWidth={1}
        />
      ) : null}

      {selected ? (
        <rect
          x={x - half - 3}
          y={y0 - 3}
          width={thickness + 6}
          height={Math.max(bodyEnd - y0, 3) + 6}
          className="fill-none stroke-accent"
          strokeWidth={1}
        />
      ) : null}
    </g>
  );
}
