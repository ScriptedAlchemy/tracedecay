import React from "react";
import { cn } from "./cn";
import { Button } from "./sdk";

/** Shared dashboard UI primitives (`tdp-*`), themed through host `--color-*` variables. */
/** Muted placeholder for empty/loading states. */
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

/** Destructive-tinted alert with an optional Retry button. */
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

/** Big-value + small-label stat tile.
 *
 * `title` surfaces a plain-language explanation as a native browser tooltip
 * (and a `help` cursor) on the whole tile — used by holographic stat rows that
 * need to explain what a number means without adding visible chrome. */
export function Stat({
  label,
  value,
  hint,
  title,
  variant = "default",
  className,
}: {
  label: string;
  value: React.ReactNode;
  hint?: string;
  /** Native tooltip text for the whole tile; sets cursor: help when present. */
  title?: string;
  variant?: "default" | "compact";
  className?: string;
}) {
  return (
    <div
      className={cn("tdp-stat", variant === "compact" && "tdp-stat-compact", className)}
      title={title}
    >
      <div className="tdp-stat-value">{value}</div>
      <div className="tdp-stat-label">{label}</div>
      {hint && <div className="tdp-stat-hint">{hint}</div>}
    </div>
  );
}

/** Label/value bar list of optionally-pickable rows. */
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
  keyName: keyof Row & string;
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
