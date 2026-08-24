import { useEffect, useRef, useState } from 'react';
import { cn } from '../../ui/cn';
import type { DeliveryField as Field } from './field.ts';

/**
 * The delivery field, drawn.
 *
 * SVG for the same reasons the Loom weave is SVG: forty-odd bodies is not a
 * canvas workload, and staying in the DOM buys real dash patterns for the
 * unknown-branch band and hue that flips with the theme through the token
 * variables rather than through a second colour table.
 *
 * Colour is deliberately NOT a channel here. The measurements are position
 * (when indexed, how many branches) and size (how many checkouts); giving every
 * repository its own hue would add forty-four colours that mean nothing and
 * would compete with the state hues that do. Every body is the accent, lit by
 * its own recency.
 */

const GUTTER = 40;
const RIGHT_PAD = 10;
const TOP_PAD = 14;
/** The measured plot: repositories with a known branch count. */
const PLOT_HEIGHT = 190;
/** Separated band for entries that are not git checkouts at all. */
const UNKNOWN_BAND = 40;
/** Clearance between the branch axis floor and the fence below it, so a body
 * measuring the minimum branch count is not drawn sitting ON the rule that
 * separates measured repositories from unmeasured ones. */
const FENCE_GAP = 9;
const AXIS_HEIGHT = 18;

export function DeliveryFieldPlot({
  field,
  selectedId,
  onSelect,
  ariaLabel,
}: {
  field: Field;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  ariaLabel: string;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(760);

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

  const hasUnknown = field.unknownBranchCount > 0;
  const height =
    TOP_PAD + PLOT_HEIGHT + (hasUnknown ? UNKNOWN_BAND : 0) + AXIS_HEIGHT;
  const fieldWidth = Math.max(width - GUTTER - RIGHT_PAD, 80);
  const columnWidth = fieldWidth / Math.max(field.columns.length, 1);
  const fenceY = TOP_PAD + PLOT_HEIGHT + FENCE_GAP;
  const unknownY = fenceY + (UNKNOWN_BAND - FENCE_GAP) / 2 + 4;

  const xOf = (column: number, offset: number) =>
    GUTTER + (column + 0.5 + offset) * columnWidth;
  const yOf = (y: number | null) =>
    y == null ? unknownY : TOP_PAD + (1 - y) * PLOT_HEIGHT;

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
        {/* Branch-axis rules at the ceiling and the floor, labelled, so the log
          * scale can be read off the picture instead of assumed. */}
        {[
          { y: TOP_PAD, label: String(field.branchCeiling) },
          { y: TOP_PAD + PLOT_HEIGHT, label: String(field.branchFloor) },
        ].map((rule) => (
          <g key={rule.label + rule.y}>
            <line
              x1={GUTTER}
              y1={rule.y}
              x2={Math.max(width - RIGHT_PAD, GUTTER)}
              y2={rule.y}
              className="stroke-edge-subtle"
              strokeWidth={1}
              strokeOpacity={0.6}
            />
            <text
              x={GUTTER - 6}
              y={rule.y + 3}
              textAnchor="end"
              className="fill-text-muted text-[9px] tabular-nums"
            >
              {rule.label}
            </text>
          </g>
        ))}

        {/* Column dividers. */}
        {field.columns.map((column, index) =>
          index === 0 ? null : (
            <line
              key={column.id}
              x1={GUTTER + index * columnWidth}
              y1={TOP_PAD - 6}
              x2={GUTTER + index * columnWidth}
              y2={TOP_PAD + PLOT_HEIGHT + (hasUnknown ? UNKNOWN_BAND : 0)}
              className="stroke-edge-subtle"
              strokeWidth={1}
            />
          ),
        )}

        {/* The unknown-branch band, fenced off from the measured plot by a
          * dashed rule. A body down here has no branch measurement at all; it
          * is not a repository with none. */}
        {hasUnknown ? (
          <>
            <line
              x1={GUTTER}
              y1={fenceY}
              x2={Math.max(width - RIGHT_PAD, GUTTER)}
              y2={fenceY}
              className="stroke-edge-strong"
              strokeWidth={1}
              strokeDasharray="3 3"
            />
            <text
              x={GUTTER + 4}
              y={fenceY + 11}
              className="fill-text-muted text-[8px] uppercase tracking-[0.16em]"
            >
              no git directory · branches unknown
            </text>
          </>
        ) : null}

        {/* The bodies. */}
        {field.bodies.map((body) => {
          const cx = xOf(body.column, body.offset);
          const cy = yOf(body.y);
          const radius = 2.5 + body.size * 4.5;
          const selected = selectedId === body.id;
          return (
            <g
              key={body.id}
              className="cursor-pointer"
              onClick={() => onSelect(selected ? null : body.id)}
              aria-hidden
            >
              <circle cx={cx} cy={cy} r={Math.max(radius + 5, 9)} fill="transparent" />
              <circle
                cx={cx}
                cy={cy}
                r={radius}
                className={cn(
                  body.branches == null
                    ? 'fill-none stroke-text-muted'
                    : 'fill-accent stroke-none',
                )}
                strokeWidth={1}
                strokeDasharray={body.branches == null ? '2 2' : undefined}
                // Recency as luminance. Floored so a dormant repository sinks
                // toward the substrate without vanishing off the field — it is
                // still registered, and the reader has to be able to see that.
                fillOpacity={
                  body.branches == null ? 0 : 0.28 + body.vitality * 0.72
                }
              />
              {body.active ? (
                <circle
                  cx={cx}
                  cy={cy}
                  r={radius + 3}
                  className="fill-none stroke-accent"
                  strokeWidth={1}
                  strokeOpacity={0.7}
                />
              ) : null}
              {selected ? (
                <circle
                  cx={cx}
                  cy={cy}
                  r={radius + 5.5}
                  className="fill-none stroke-text-primary"
                  strokeWidth={1}
                />
              ) : null}
            </g>
          );
        })}

        {/* The recency axis, printed under its own columns. */}
        {field.columns.map((column, index) => (
          <text
            key={`axis-${column.id}`}
            x={GUTTER + (index + 0.5) * columnWidth}
            y={height - 5}
            textAnchor="middle"
            className="fill-text-muted text-[9px] uppercase tracking-[0.14em]"
          >
            {column.label} · {column.count}
          </text>
        ))}
      </svg>
    </div>
  );
}
