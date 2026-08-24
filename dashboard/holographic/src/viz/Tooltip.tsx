import { useLayoutEffect, useRef, useState, type Key, type ReactNode } from "react";

export interface TooltipState {
  /** Cursor x/y in the chart container's coordinate space (px). */
  x: number;
  y: number;
  content: ReactNode;
}

/**
 * Shared floating tooltip for the holographic charts.
 *
 * Render inside a `position: relative` chart container; the tooltip follows
 * the supplied cursor position and flips to stay inside the container.
 * Pointer events pass through so it never steals hover.
 */
export function VizTooltip({ tip }: { tip: TooltipState | null }) {
  const ref = useRef<HTMLDivElement>(null);
  const [shift, setShift] = useState({ dx: 0, dy: 0 });

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node || !tip) return;
    const parent = node.offsetParent as HTMLElement | null;
    if (!parent) return;
    const pw = parent.clientWidth;
    const w = node.offsetWidth;
    const h = node.offsetHeight;
    let dx = 12;
    let dy = -h - 10;
    if (tip.x + dx + w > pw - 4) dx = -w - 12;
    if (tip.y + dy < 4) dy = 14;
    setShift((prev) => (prev.dx === dx && prev.dy === dy ? prev : { dx, dy }));
  }, [tip]);

  if (!tip) return null;
  return (
    <div
      ref={ref}
      className="hv-tooltip"
      role="status"
      style={{ left: tip.x + shift.dx, top: tip.y + shift.dy }}
    >
      {tip.content}
    </div>
  );
}

/** Consistent label/value row used inside tooltips across all charts. */
export function TipRow({ label, value }: { label: string; value: ReactNode; key?: Key }) {
  return (
    <div className="hv-tooltip-row">
      <span className="hv-tooltip-label">{label}</span>
      <span className="hv-tooltip-value">{value}</span>
    </div>
  );
}

/** Color swatch + title heading used as the first tooltip line. */
export function TipTitle({ color, children }: { color?: string; children: ReactNode }) {
  return (
    <div className="hv-tooltip-title">
      {color && <span className="hv-swatch" style={{ background: color }} />}
      <span className="hv-tooltip-title-text">{children}</span>
    </div>
  );
}
