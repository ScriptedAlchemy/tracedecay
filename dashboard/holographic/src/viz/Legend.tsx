/**
 * Shared chart legend: color swatches with optional counts, optionally
 * toggleable (click to hide/show a series across the owning chart).
 */

export interface LegendItem {
  key: string;
  label: string;
  color: string;
  count?: number;
}

export function VizLegend({
  items,
  hidden,
  onToggle,
}: {
  items: LegendItem[];
  /** Keys currently hidden (only meaningful when `onToggle` is provided). */
  hidden?: ReadonlySet<string>;
  onToggle?: (key: string) => void;
}) {
  if (items.length === 0) return null;
  return (
    <div className="hv-legend" role={onToggle ? "group" : undefined}>
      {items.map((item) => {
        const isHidden = hidden?.has(item.key) ?? false;
        const body = (
          <>
            <span
              className="hv-swatch"
              style={{ background: isHidden ? "transparent" : item.color, borderColor: item.color }}
            />
            <span className="hv-legend-label">{item.label}</span>
            {item.count != null && <span className="hv-legend-count">{item.count}</span>}
          </>
        );
        if (!onToggle) {
          return (
            <span key={item.key} className="hv-legend-item">
              {body}
            </span>
          );
        }
        return (
          <button
            key={item.key}
            type="button"
            className={`hv-legend-item hv-legend-toggle${isHidden ? " hv-legend-off" : ""}`}
            aria-pressed={!isHidden}
            title={isHidden ? `Show ${item.label}` : `Hide ${item.label}`}
            onClick={() => onToggle(item.key)}
          >
            {body}
          </button>
        );
      })}
    </div>
  );
}
