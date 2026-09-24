import { useId, useMemo, useState } from 'react';

import { CAMERA_HEADER_H, CAMERA_ROW_H, layoutCameras } from './cameras.ts';
import { elideToWidth, type SceneFact } from './factScene.ts';
import { useSceneFocus } from './focus.ts';
import {
  FocalReadout,
  HatchDef,
  LABEL_PX,
  LEGEND_PX,
  Label,
  MarkKey,
  RelationKey,
  VariantFrame,
  relationStroke,
  trustText,
  useMeasuredBox,
} from './marks.tsx';
import type { RendererProps } from './types.ts';

/** Variant A, provenance cameras: bounded rows per category frame, one shared
 * zero-anchored trust rail, relations routed through the glyph gutter. */
export function CamerasRenderer({ scene, inspectedFactId, selectedFactId, onInspect, onSelect, graphRead }: RendererProps) {
  const hatchId = useId();
  const [ref, box] = useMeasuredBox({ width: 960, height: 1 });
  const layout = useMemo(() => layoutCameras(scene, box.width), [scene, box.width]);
  const [hovered, setHovered] = useState<string | null>(null);
  const focus = useSceneFocus(scene, hovered, inspectedFactId, selectedFactId);

  return (
    <VariantFrame
      variant="cameras"
      title="Provenance cameras"
      axes="one frame per category · rows by trust · every rail 0 → 1.00 on one scale · cites = entities named, not resolved symbols"
      scene={scene}
      graphRead={graphRead}
      description={`Provenance cameras: ${scene.facts.length} facts in ${layout.frames.length} category frames, ${layout.relations.length} relations drawn. The fact ledger is the exact accessible equivalent.`}
      focal={<FocalReadout fact={focus.fact} role={focus.role} />}
      keys={
        <>
          <MarkKey />
          <RelationKey scene={scene} />
          {layout.relationsPastCap > 0 ? (
            <p className="text-3xs text-state-partial">
              {layout.relationsPastCap} relations reach rows past a frame's cap and are not drawn
            </p>
          ) : null}
        </>
      }
    >
      <div ref={ref} className="w-full">
        <svg
          role="img"
          aria-label="Provenance camera frames; the fact ledger lists the same rows"
          width={layout.width}
          height={layout.height}
          viewBox={`0 0 ${layout.width} ${layout.height}`}
          className="block select-none"
          data-testid="fact-constellation-svg"
          onPointerLeave={() => setHovered(null)}
        >
          <defs>
            <HatchDef id={hatchId} />
          </defs>
          {layout.frames.map((frame) => (
            <g key={frame.title} data-frame={frame.title}>
              <FrameCorners x={frame.x} y={frame.y} w={frame.w} h={frame.h} />
              <Label x={frame.x + 10} y={frame.y + 14} size={LEGEND_PX} mono={false} tone="var(--raw-text-secondary)">
                {frame.title.toUpperCase()} · {frame.count}
              </Label>
              <Label x={frame.x + 10 + titleWidth(frame.title, frame.count)} y={frame.y + 14} size={LEGEND_PX} opacity={0.62}>
                {frame.cites.length > 0
                  ? elideToWidth(
                      `cites ${frame.cites.map((cite) => `${cite.label} ×${cite.count}`).join(' · ')}`,
                      layout.rail.x - 18 - titleWidth(frame.title, frame.count),
                      LEGEND_PX,
                    )
                  : 'cites no entity by name'}
              </Label>
              <Label x={frame.x + layout.rail.x} y={frame.y + 14} size={LEGEND_PX} opacity={0.55}>
                0
              </Label>
              <Label x={frame.x + layout.rail.x + layout.rail.w} y={frame.y + 14} size={LEGEND_PX} opacity={0.55} anchor="end">
                1.00
              </Label>
              {frame.overflow > 0 ? (
                <Label x={frame.x + 24} y={frame.y + CAMERA_HEADER_H + frame.rows.length * CAMERA_ROW_H + 13} size={LEGEND_PX} opacity={0.7}>
                  +{frame.overflow} more in the ledger
                </Label>
              ) : null}
            </g>
          ))}
          <g data-layer="relations">
            {layout.relations.map(({ relation, d, midX, midY, vertical }) => {
              const style = relationStroke(relation.kind);
              const lit = focus.set !== null && focus.set.has(relation.source) && focus.set.has(relation.target);
              const dim = focus.set !== null && !lit;
              return (
                <g key={relation.id} data-relation={relation.kind}>
                  <path
                    d={d}
                    fill="none"
                    stroke={style.stroke}
                    strokeOpacity={dim ? 0.1 : lit || style.loud ? style.opacity : style.opacity * 0.6}
                    strokeWidth={style.width}
                    strokeDasharray={style.dash}
                    className="transition-opacity duration-[var(--dur-state)]"
                  />
                  {style.loud || lit ? (
                    <g transform={vertical ? `rotate(-90 ${midX} ${midY})` : undefined}>
                      <Label x={midX} y={vertical ? midY - 3 : midY + 3.5} size={LEGEND_PX} anchor="middle" tone={style.stroke} opacity={dim ? 0.2 : 1}>
                        {style.label}
                      </Label>
                    </g>
                  ) : null}
                </g>
              );
            })}
          </g>
          {layout.frames.flatMap((frame) =>
            frame.rows.map((row) => (
              <Row
                key={row.fact.nodeId}
                fact={row.fact}
                x={frame.x}
                top={row.top}
                w={frame.w}
                gx={row.gx}
                gy={row.gy}
                rail={layout.rail}
                labelW={layout.labelW}
                hatchId={hatchId}
                dimmed={focus.set !== null && !focus.set.has(row.fact.nodeId)}
                inspected={row.fact.nodeId === focus.inspectedNode}
                selected={row.fact.nodeId === focus.selectedNode}
                onEnter={() => {
                  if (hovered === row.fact.nodeId) return;
                  setHovered(row.fact.nodeId);
                  onInspect(row.fact.factId);
                }}
                onClick={() => onSelect(row.fact.factId)}
              />
            )),
          )}
        </svg>
      </div>
    </VariantFrame>
  );
}

/** Width the engraved frame title claims: the display face runs wide and
 * tracked, about 0.78em a glyph at the legend size. */
function titleWidth(title: string, count: number): number {
  return Math.ceil(`${title} · ${count}`.length * LEGEND_PX * 0.78) + 10;
}

function FrameCorners({ x, y, w, h }: { x: number; y: number; w: number; h: number }) {
  const k = 9;
  const d = [
    `M ${x} ${y + k} V ${y} H ${x + k}`,
    `M ${x + w - k} ${y} H ${x + w} V ${y + k}`,
    `M ${x + w} ${y + h - k} V ${y + h} H ${x + w - k}`,
    `M ${x + k} ${y + h} H ${x} V ${y + h - k}`,
  ].join(' ');
  return (
    <>
      <rect x={x} y={y} width={w} height={h} fill="var(--raw-surface-0)" fillOpacity={0.35} stroke="var(--raw-graph-edge)" strokeOpacity={0.35} strokeWidth={0.8} />
      <path d={d} fill="none" stroke="var(--raw-graph-accent)" strokeOpacity={0.7} strokeWidth={1.2} />
    </>
  );
}

function Row({
  fact,
  x,
  top,
  w,
  gx,
  gy,
  rail,
  labelW,
  hatchId,
  dimmed,
  inspected,
  selected,
  onEnter,
  onClick,
}: {
  fact: SceneFact;
  x: number;
  top: number;
  w: number;
  gx: number;
  gy: number;
  rail: { x: number; w: number };
  labelW: number;
  hatchId: string;
  dimmed: boolean;
  inspected: boolean;
  selected: boolean;
  onEnter: () => void;
  onClick: () => void;
}) {
  const fill = fact.trust == null ? 0 : Math.max(0, Math.min(1, fact.trust)) * rail.w;
  return (
    <g
      data-node={fact.nodeId}
      data-fact-id={fact.factId}
      data-access={fact.access}
      data-inspected={inspected || undefined}
      data-selected={selected || undefined}
      opacity={dimmed ? 0.3 : 1}
      className="cursor-pointer transition-opacity duration-[var(--dur-state)]"
      onPointerMove={onEnter}
      onClick={onClick}
    >
      <rect x={x + 1} y={top} width={w - 2} height={CAMERA_ROW_H} fill={inspected ? 'var(--raw-surface-3)' : 'transparent'} fillOpacity={0.6} />
      {selected ? (
        <rect x={x} y={top + 2} width={2} height={CAMERA_ROW_H - 4} fill="var(--raw-graph-accent)" data-selected-gutter />
      ) : null}
      {fact.restricted ? (
        <rect x={gx - 4} y={gy - 4} width={8} height={8} fill={`url(#${hatchId})`} stroke="var(--raw-state-locked)" data-mark="restricted" />
      ) : fact.trust == null ? (
        <circle cx={gx} cy={gy} r={3.5} fill="none" stroke="var(--raw-graph-text)" strokeDasharray="2 2" data-mark="trust-absent" />
      ) : (
        <circle cx={gx} cy={gy} r={3.5} fill="var(--raw-graph-accent)" fillOpacity={0.42 + fact.trust * 0.58} data-mark="measured" />
      )}
      <Label x={x + 24} y={gy + 3.8} tone={fact.restricted ? 'var(--raw-state-locked)' : 'var(--raw-graph-text)'}>
        {elideToWidth(fact.label, labelW, LABEL_PX)}
      </Label>
      <line x1={x + rail.x} y1={gy} x2={x + rail.x + rail.w} y2={gy} stroke="var(--raw-graph-edge)" strokeOpacity={0.6} strokeWidth={1} />
      <line x1={x + rail.x} y1={gy - 4} x2={x + rail.x} y2={gy + 4} stroke="var(--raw-graph-text)" strokeOpacity={0.6} strokeWidth={1} />
      {fact.restricted ? (
        <rect x={x + rail.x} y={gy - 1.5} width={rail.w} height={3} fill={`url(#${hatchId})`} />
      ) : fill > 0 ? (
        <rect
          x={x + rail.x}
          y={gy - 1.5}
          width={fill}
          height={3}
          fill="var(--raw-graph-accent)"
          fillOpacity={0.85}
          data-rail-width={Math.round(fill * 100) / 100}
        />
      ) : null}
      <Label x={x + w - 8} y={gy + 3.8} anchor="end" tone={fact.trust == null ? 'var(--raw-text-muted)' : 'var(--raw-text-primary)'}>
        {fact.trust == null ? 'absent' : fact.trust.toFixed(2)}
      </Label>
      <rect x={x} y={top} width={w} height={CAMERA_ROW_H} fill="transparent">
        <title>
          {fact.label} · {trustText(fact)} · {fact.degree} relations
        </title>
      </rect>
    </g>
  );
}
