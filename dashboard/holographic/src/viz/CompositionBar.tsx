import { useMemo, useState, type ReactNode } from "react";
import { VizTooltip, TipRow, TipTitle, type TooltipState } from "./Tooltip";

export interface CompositionSegment {
  key: string;
  label: string;
  value: number;
  color: string;
  /** Extra tooltip rows for this segment (e.g. dim, avg trust). */
  detail?: Array<{ label: string; value: ReactNode }>;
}

/**
 * Single stacked proportional bar (100% composition) with per-segment hover
 * breakdowns. Used for bank composition and entity-type share.
 */
export function CompositionBar({
  segments,
  height = 14,
  totalLabel = "total",
}: {
  segments: CompositionSegment[];
  height?: number;
  totalLabel?: string;
}) {
  const [tip, setTip] = useState<TooltipState | null>(null);
  const [hoverKey, setHoverKey] = useState<string | null>(null);
  const total = useMemo(
    () => segments.reduce((sum, s) => sum + Math.max(0, s.value), 0),
    [segments],
  );

  if (total <= 0 || segments.length === 0) {
    return <p className="text-xs text-text-tertiary">No data.</p>;
  }

  let acc = 0;
  return (
    <div className="hv-chart">
      <div className="hv-composition" style={{ height }} role="img" aria-label="Composition bar">
        {segments.map((seg) => {
          const frac = Math.max(0, seg.value) / total;
          acc += frac;
          if (frac <= 0) return null;
          return (
            <div
              key={seg.key}
              className={`hv-composition-seg${hoverKey && hoverKey !== seg.key ? " hv-dim" : ""}`}
              style={{ width: `${(frac * 100).toFixed(3)}%`, background: seg.color }}
              onPointerMove={(event) => {
                const host = event.currentTarget.closest(".hv-chart") as HTMLElement | null;
                const rect = host?.getBoundingClientRect();
                setHoverKey(seg.key);
                setTip({
                  x: rect ? event.clientX - rect.left : 0,
                  y: rect ? event.clientY - rect.top : 0,
                  content: (
                    <>
                      <TipTitle color={seg.color}>{seg.label}</TipTitle>
                      <TipRow label={totalLabel} value={seg.value} />
                      <TipRow label="share" value={`${(frac * 100).toFixed(1)}%`} />
                      {seg.detail?.map((row) => (
                        <TipRow key={row.label} label={row.label} value={row.value} />
                      ))}
                    </>
                  ),
                });
              }}
              onPointerLeave={() => {
                setHoverKey(null);
                setTip(null);
              }}
            />
          );
        })}
      </div>
      <VizTooltip tip={tip} />
    </div>
  );
}
