import React from "react";
import { cn } from "./cn";
import { Button } from "./sdk";

/**
 * Shared dashboard UI primitives (`tdp-*` namespace).
 *
 * Built on the host-SDK design-system components and the host `--color-*`
 * CSS variables, so they theme correctly in both the standalone tracedecay
 * shell (which aliases every `--color-*` to its `--ts-*` token — see
 * shell/src/styles.css) and the Hermes dashboard (whose shadcn palette
 * defines the same `--color-*` names).
 *
 * Visual values are ported verbatim from the Code Graph Explorer
 * (graph/src/styles.css `tsg-*` classes) with their `--ts-*` colors resolved
 * through the equivalent `--color-*` host var, so a plugin that adopts these
 * looks identical to the original hand-rolled markup.
 */

/**
 * Muted placeholder for empty/loading states.
 *
 * `variant`:
 *  - `"centered"` (default, ports `.tsg-empty`): centered muted block.
 *  - `"dashed"`: left-aligned dashed-border box (ports savings `.tss-empty`),
 *    for no-data panels that carry an `<h3>`/`<p>` explanation.
 */
export function EmptyState({
  children,
  variant = "centered",
  className,
}: {
  children: React.ReactNode;
  variant?: "centered" | "dashed";
  className?: string;
}) {
  return (
    <div className={cn("tdp-empty", variant === "dashed" && "tdp-empty-dashed", className)}>
      {children}
    </div>
  );
}

/**
 * Destructive-tinted alert with an optional Retry button (ports `.tsg-error`).
 * Rendered with `role="alert"`.
 */
export function ErrorPanel({
  error,
  onRetry,
  className,
}: {
  error: string;
  onRetry?: () => void;
  className?: string;
}) {
  return (
    <div className={cn("tdp-error", className)} role="alert">
      <span className="tdp-error-text">{error}</span>
      {onRetry && (
        <Button
          size="sm"
          variant="destructive"
          onClick={onRetry}
          className="tdp-error-retry"
        >
          Retry
        </Button>
      )}
    </div>
  );
}

/** Muted, animated placeholder bars for loading states. */
export function SkeletonLines({
  count = 3,
  widths,
  className,
}: {
  count?: number;
  widths?: Array<string>;
  className?: string;
}) {
  return (
    <div className={cn("tdp-skeleton", className)} aria-hidden="true">
      {Array.from({ length: count }, (_, i) => (
        <span
          key={i}
          className="tdp-skeleton-line"
          style={widths && widths[i] ? { width: widths[i] } : undefined}
        />
      ))}
    </div>
  );
}

/**
 * Big-value + small-label stat tile (ports the `.tss-stat` shape).
 *
 * `variant`:
 *  - `"default"`: bordered card tile with a large mono value (graph/savings).
 *  - `"compact"`: smaller, tighter tile with an uppercase label and tabular
 *    value (ports the LCM headline `.hermes-lcm-stat` row, which sits several
 *    abreast in a flex strip).
 */
export function Stat({
  label,
  value,
  hint,
  variant = "default",
  className,
}: {
  label: string;
  value: React.ReactNode;
  hint?: string;
  variant?: "default" | "compact";
  className?: string;
}) {
  return (
    <div className={cn("tdp-stat", variant === "compact" && "tdp-stat-compact", className)}>
      <div className="tdp-stat-value">{value}</div>
      <div className="tdp-stat-label">{label}</div>
      {hint && <div className="tdp-stat-hint">{hint}</div>}
    </div>
  );
}

/**
 * Label/value bar list of optionally-pickable rows (ports graph's
 * `.tsg-hub-list` / `.tsg-hub` shape).
 *
 * `keyName` selects the row field used as the visible label and default key.
 * Each row may also carry optional `value`, `meta`, and `color` fields.
 *
 * Options:
 *  - `proportional`: render each row as a head (label + value) over a fill
 *    track whose width is proportional to the row's numeric magnitude, ports
 *    the LCM `.hermes-lcm-bar-*` source/role/depth bars. The magnitude is read
 *    from `valueName` (default `"value"`); the displayed value is still the
 *    row's `value` field (a pre-formatted string), so the caller controls both
 *    the fill ratio and the exact rendered text.
 *  - `valueName`: field holding the numeric fill magnitude (default `"value"`).
 *  - `emptyText`: when set and `rows` is empty, renders an `EmptyState` with
 *    this text instead of an empty container.
 */
export function BarList<Row extends Record<string, unknown>>({
  rows,
  keyName,
  onPick,
  rowKey,
  titleFor,
  className,
  proportional,
  valueName = "value",
  emptyText,
}: {
  rows: Array<Row>;
  keyName: string;
  onPick?: (row: Row) => void;
  rowKey?: (row: Row) => string;
  titleFor?: (row: Row) => string;
  className?: string;
  proportional?: boolean;
  valueName?: string;
  emptyText?: string;
}) {
  if (!rows.length) {
    return emptyText ? <EmptyState>{emptyText}</EmptyState> : <div className={cn("tdp-bar-list", className)} />;
  }
  const total = proportional
    ? rows.reduce((acc, row) => acc + (Number(row[valueName]) || 0), 0) || 1
    : 0;
  return (
    <div className={cn("tdp-bar-list", className)}>
      {rows.map((row) => {
        const label = String(row[keyName] ?? "");
        const key = rowKey?.(row) ?? String(row[keyName]);
        const title = titleFor?.(row);
        const value = "value" in row ? row.value : undefined;
        const meta = "meta" in row ? row.meta : undefined;
        const color = "color" in row ? row.color : undefined;
        const inner = proportional ? (
          <>
            <div className="tdp-bar-head">
              <span className="tdp-bar-label">{label}</span>
              {value !== undefined && <span className="tdp-bar-value">{String(value)}</span>}
            </div>
            <div className="tdp-bar-track" aria-hidden="true">
              <span
                className="tdp-bar-fill"
                style={{
                  width:
                    Math.max(2, Math.round(((Number(row[valueName]) || 0) / total) * 100)) +
                    "%",
                }}
              />
            </div>
          </>
        ) : (
          <>
            {color !== undefined && (
              <span className="tdp-bar-dot" style={{ background: String(color) }} />
            )}
            <span className="tdp-bar-label">{label}</span>
            {meta !== undefined && <span className="tdp-bar-meta">{String(meta)}</span>}
            {value !== undefined && <span className="tdp-bar-value">{String(value)}</span>}
          </>
        );
        const rowClassName = cn(
          "tdp-bar-row",
          proportional && "tdp-bar-row-prop",
          onPick && "tdp-bar-row-pickable",
        );
        return onPick ? (
          <button
            key={key}
            type="button"
            className={rowClassName}
            title={title}
            onClick={() => onPick(row)}
          >
            {inner}
          </button>
        ) : (
          <div key={key} className={rowClassName} title={title}>
            {inner}
          </div>
        );
      })}
    </div>
  );
}
