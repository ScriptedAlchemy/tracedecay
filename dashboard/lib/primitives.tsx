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

/** Centered, muted placeholder (ports `.tsg-empty`). */
export function EmptyState({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return <div className={cn("tdp-empty", className)}>{children}</div>;
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

/** Big-value + small-label stat tile (ports the `.tss-stat` shape). */
export function Stat({
  label,
  value,
  hint,
  className,
}: {
  label: string;
  value: React.ReactNode;
  hint?: string;
  className?: string;
}) {
  return (
    <div className={cn("tdp-stat", className)}>
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
 */
export function BarList<Row extends Record<string, unknown>>({
  rows,
  keyName,
  onPick,
  rowKey,
  titleFor,
  className,
}: {
  rows: Array<Row>;
  keyName: string;
  onPick?: (row: Row) => void;
  rowKey?: (row: Row) => string;
  titleFor?: (row: Row) => string;
  className?: string;
}) {
  return (
    <div className={cn("tdp-bar-list", className)}>
      {rows.map((row) => {
        const label = String(row[keyName] ?? "");
        const key = rowKey?.(row) ?? String(row[keyName]);
        const title = titleFor?.(row);
        const value = "value" in row ? row.value : undefined;
        const meta = "meta" in row ? row.meta : undefined;
        const color = "color" in row ? row.color : undefined;
        const inner = (
          <>
            {color !== undefined && (
              <span className="tdp-bar-dot" style={{ background: String(color) }} />
            )}
            <span className="tdp-bar-label">{label}</span>
            {meta !== undefined && <span className="tdp-bar-meta">{String(meta)}</span>}
            {value !== undefined && <span className="tdp-bar-value">{String(value)}</span>}
          </>
        );
        return onPick ? (
          <button
            key={key}
            type="button"
            className="tdp-bar-row"
            title={title}
            onClick={() => onPick(row)}
          >
            {inner}
          </button>
        ) : (
          <div key={key} className="tdp-bar-row" title={title}>
            {inner}
          </div>
        );
      })}
    </div>
  );
}
