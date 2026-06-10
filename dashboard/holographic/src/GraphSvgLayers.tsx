import { memo } from "react";
import type { KeyboardEvent } from "react";
import type { SimLink, SimNode } from "./associationGraphTypes";
import {
  coOccurrenceWeight,
  colorOf,
  edgeStrokeWidth,
  edgeStyle,
  linkEndpoint,
  radiusOf,
} from "./associationGraphUtils";
import { focusRingRadius } from "./graphHitTest";
import { truncate } from "./ui";

function isVisible(id: string, visibleIds: Set<string> | null): boolean {
  return !visibleIds || visibleIds.has(id);
}

export const GraphEdges = memo(function GraphEdges({
  links,
  visibleIds,
  highlightId,
  highlightIds,
  maxWeight,
}: {
  links: SimLink[];
  visibleIds: Set<string> | null;
  highlightId: string | null;
  highlightIds: Set<string> | null;
  maxWeight: number;
}) {
  return (
    <>
      {links.map((link, index) => {
        const source = linkEndpoint(link.source);
        const target = linkEndpoint(link.target);
        if (!source || !target) return null;
        if (!isVisible(source.id, visibleIds) || !isVisible(target.id, visibleIds)) {
          return null;
        }
        const touchesSelection =
          source.id === highlightId || target.id === highlightId;
        const baseOpacity = link.kind === "bundles" ? 0.55 : 0.35;
        const strokeOpacity = highlightIds
          ? touchesSelection
            ? 0.85
            : 0.04
          : baseOpacity;
        const weight = coOccurrenceWeight(link, source, target);
        const style = edgeStyle(link.kind);
        return (
          <line
            key={`${source.id}-${target.id}-${index}`}
            x1={source.x ?? 0}
            y1={source.y ?? 0}
            x2={target.x ?? 0}
            y2={target.y ?? 0}
            // The `stroke-primary` class (CSS) wins over the per-kind stroke
            // attribute, so selected edges still flash the accent color.
            className={touchesSelection ? "stroke-primary" : undefined}
            stroke={style.color}
            strokeDasharray={style.dash}
            strokeOpacity={strokeOpacity}
            strokeLinecap="round"
            strokeWidth={edgeStrokeWidth(weight, maxWeight, touchesSelection)}
          />
        );
      })}
    </>
  );
});

/**
 * Lightweight layer painted while the layout settles: plain circles only (no
 * labels, no per-node interactivity), so re-rendering ~hundreds of nodes every
 * animation frame stays cheap and the main thread never blocks.
 */
export function GraphSettleLayer({ nodes }: { nodes: SimNode[] }) {
  return (
    <>
      {nodes.map((node) => {
        const r = radiusOf(node.kind, node.degree);
        const color = colorOf(node.kind);
        return (
          <circle
            key={node.id}
            cx={node.x ?? 0}
            cy={node.y ?? 0}
            r={r}
            fill={color}
            fillOpacity={0.55}
            stroke={color}
            strokeWidth={1}
          />
        );
      })}
    </>
  );
}

export const GraphNodes = memo(function GraphNodes({
  nodes,
  selectedId,
  activeId,
  ringId,
  highlightIds,
  registerNodeEl,
  onSelect,
  onHoverEnter,
  onHoverLeave,
  onNodeKeyDown,
  onNodeFocus,
}: {
  nodes: SimNode[];
  selectedId: string | null;
  activeId: string | null;
  ringId: string | null;
  highlightIds: Set<string> | null;
  registerNodeEl: (id: string, el: SVGGElement | null) => void;
  onSelect: (id: string) => void;
  onHoverEnter: (id: string) => void;
  onHoverLeave: (id: string) => void;
  onNodeKeyDown: (event: KeyboardEvent<SVGGElement>, id: string) => void;
  onNodeFocus: (id: string) => void;
}) {
  return (
    <>
      {nodes.map((node) => {
        const x = node.x ?? 0;
        const y = node.y ?? 0;
        const r = radiusOf(node.kind, node.degree);
        const color = colorOf(node.kind);
        const isSelected = node.id === selectedId;
        const showRing = node.id === ringId;
        const inFocus = !highlightIds || highlightIds.has(node.id);
        const ringR = focusRingRadius(r, isSelected);
        return (
          <g
            key={node.id}
            ref={(el) => registerNodeEl(node.id, el)}
            role="button"
            tabIndex={node.id === activeId ? 0 : -1}
            aria-label={`${node.kind}: ${node.label}`}
            aria-pressed={isSelected}
            transform={`translate(${x} ${y})`}
            className="cursor-pointer outline-none"
            opacity={inFocus ? 1 : 0.12}
            onPointerDown={(event) => event.stopPropagation()}
            onClick={() => onSelect(node.id)}
            onPointerEnter={() => onHoverEnter(node.id)}
            onPointerLeave={() => onHoverLeave(node.id)}
            onFocus={() => onNodeFocus(node.id)}
            onKeyDown={(event) => onNodeKeyDown(event, node.id)}
          >
            {showRing && (
              <circle
                cx={0}
                cy={0}
                r={ringR}
                fill="none"
                className="stroke-primary"
                strokeWidth={2}
                strokeOpacity={0.9}
                strokeDasharray="3 2"
              />
            )}
            <circle
              cx={0}
              cy={0}
              r={isSelected ? r + 2 : r}
              fill={color}
              fillOpacity={isSelected ? 0.95 : inFocus ? 0.6 : 0.45}
              stroke={isSelected ? "#ffffff" : color}
              strokeWidth={isSelected ? 2.5 : 1}
            />
          </g>
        );
      })}
    </>
  );
});

/**
 * Labels live in their own memoized layer (drawn above the node circles) so a
 * zoom only re-renders the handful of visible labels — never the hundreds of
 * interactive node groups — and a pan re-renders neither.
 */
export const GraphLabels = memo(function GraphLabels({
  nodes,
  labeledIds,
  worldLabelFont,
}: {
  nodes: SimNode[];
  labeledIds: Set<string>;
  worldLabelFont: number;
}) {
  if (labeledIds.size === 0) return null;
  return (
    <>
      {nodes.map((node) => {
        if (!labeledIds.has(node.id)) return null;
        const r = radiusOf(node.kind, node.degree);
        return (
          <text
            key={node.id}
            x={node.x ?? 0}
            y={(node.y ?? 0) + r + worldLabelFont + 2}
            textAnchor="middle"
            fontSize={worldLabelFont}
            className="pointer-events-none fill-text-secondary font-mono-ui"
            style={{
              paintOrder: "stroke",
              stroke: "var(--color-card)",
              strokeWidth: worldLabelFont * 0.32,
              strokeLinejoin: "round",
            }}
          >
            {truncate(node.label, 22)}
          </text>
        );
      })}
    </>
  );
});
