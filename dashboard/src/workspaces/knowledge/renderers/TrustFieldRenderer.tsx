import { useId, useMemo, useState } from 'react';

import { elideToWidth } from './factScene.ts';
import { useSceneFocus } from './focus.ts';
import {
  FactMark,
  FocalReadout,
  FocusMarks,
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
import { layoutTrustField } from './trustField.ts';
import type { RendererProps } from './types.ts';

const FIELD_LABEL_W = 30 * LABEL_PX * 0.6;

/** Variant B, trust field: trust across, last update up, retrievals as area,
 * entity envelopes, contradictions on top and enumerated beneath. */
export function TrustFieldRenderer({ scene, inspectedFactId, selectedFactId, onInspect, onSelect, graphRead }: RendererProps) {
  const hatchId = useId();
  const clipId = useId();
  const [ref, box] = useMeasuredBox({ width: 960, height: 360 });
  const [zoom, setZoom] = useState<string | null>(null);
  const layout = useMemo(() => layoutTrustField(scene, box, zoom), [scene, box, zoom]);
  const [hovered, setHovered] = useState<string | null>(null);
  const focus = useSceneFocus(scene, hovered, inspectedFactId, selectedFactId);
  const focusEntities = new Set(focus.role === 'hub' ? [] : (focus.fact?.entityIds ?? []));
  const { plot } = layout;
  const disputes = scene.relations.filter((relation) => relation.kind === 'contradicts' || relation.kind === 'supersedes');
  const isFocus = (nodeId: string) => nodeId === focus.inspectedNode || nodeId === focus.selectedNode;

  return (
    <VariantFrame
      variant="field"
      title="Trust field"
      axes="x · trust, one linear scale · y · row updated_at, newest up · area · retrievals"
      scene={scene}
      graphRead={graphRead}
      description={`Trust field: ${scene.facts.length} facts by trust and last update, ${disputes.length} contradictions or supersessions listed beneath. The fact ledger is the exact accessible equivalent.`}
      keys={
        <>
          <MarkKey />
          <RelationKey scene={scene} />
          <p className="text-3xs text-text-muted">
            the envelope outlines the facts citing the inspected fact's entities, or the zoomed entity; entities are
            cited by name, not resolved symbols · largest retrieval count{' '}
            <span className="td-value text-text-secondary">{layout.retrievalCeiling}</span>
          </p>
        </>
      }
      focal={<FocalReadout fact={focus.fact} role={focus.role} />}
      control={
        <>
          <label className="flex items-center gap-2 text-3xs text-text-muted">
            <span className="td-legend">zoom</span>
            <select
              value={zoom ?? ''}
              onChange={(event) => setZoom(event.target.value || null)}
              className="min-h-[var(--touch-target-min)] border border-edge-subtle bg-surface-1 px-2 font-mono text-2xs text-text-primary"
              aria-label="Zoom the field into one entity's facts"
            >
              <option value="">all facts · {scene.facts.length}</option>
              {scene.entities.map((entity) => (
                <option key={entity.nodeId} value={entity.nodeId}>
                  {entity.label} · {entity.factIds.length}
                </option>
              ))}
            </select>
          </label>
        </>
      }
    >
      <div ref={ref} className="h-[clamp(220px,32vh,380px)] w-full">
        <svg
          role="img"
          aria-label={
            layout.zoom
              ? `Trust field zoomed to ${layout.zoom.label}, ${layout.zoom.count} facts`
              : 'Trust field of every drawn fact'
          }
          width={layout.width}
          height={layout.height}
          viewBox={`0 0 ${layout.width} ${layout.height}`}
          className="block select-none"
          data-testid="fact-constellation-svg"
          data-zoom={layout.zoom?.entityId}
          onPointerLeave={() => setHovered(null)}
        >
          <defs>
            <HatchDef id={hatchId} />
            <clipPath id={clipId}>
              <rect x={plot.x0 - 12} y={plot.y0 - 12} width={layout.width - plot.x0 + 12} height={plot.y1 - plot.y0 + 48} />
            </clipPath>
          </defs>
          <g data-layer="axes">
            {layout.xTicks.map((tick) => (
              <g key={`x${tick.label}`}>
                <line x1={tick.at} y1={plot.y0} x2={tick.at} y2={plot.y1} stroke="var(--raw-graph-edge)" strokeOpacity={0.28} strokeDasharray="2 5" />
                <Label x={tick.at} y={layout.height - 8} anchor="middle" size={LEGEND_PX} opacity={0.7}>
                  {tick.label}
                </Label>
              </g>
            ))}
            {layout.yTicks.map((tick) => (
              <g key={`y${tick.label}${tick.at}`}>
                <line x1={plot.x0} y1={tick.at} x2={plot.x1} y2={tick.at} stroke="var(--raw-graph-edge)" strokeOpacity={0.22} strokeDasharray="2 5" />
                <Label x={plot.x0 - 6} y={tick.at + 3.5} anchor="end" size={LEGEND_PX} opacity={0.7}>
                  {tick.label}
                </Label>
              </g>
            ))}
            <line x1={plot.x0} y1={plot.y1} x2={plot.x1} y2={plot.y1} stroke="var(--raw-graph-text)" strokeOpacity={0.5} />
            <line x1={plot.x0} y1={plot.y0} x2={plot.x0} y2={plot.y1} stroke="var(--raw-graph-text)" strokeOpacity={0.5} />
            <Label x={plot.x1 + 22} y={plot.y0 + 8} anchor="middle" size={LEGEND_PX} opacity={0.7}>
              absent
            </Label>
            <Label x={plot.x1 + 22} y={plot.y0 + 20} anchor="middle" size={LEGEND_PX} opacity={0.9}>
              {layout.trustAbsent}
            </Label>
            <Label x={plot.x0 + 4} y={plot.y1 + 20} size={LEGEND_PX} opacity={0.7}>
              updated_at absent {layout.timeAbsent}
            </Label>
            {layout.timeDomain == null ? (
              <Label x={(plot.x0 + plot.x1) / 2} y={(plot.y0 + plot.y1) / 2 - 14} anchor="middle" size={LEGEND_PX}>
                no loaded row carried updated_at; the vertical axis is not measured
              </Label>
            ) : null}
          </g>
          <g clipPath={`url(#${clipId})`}>
            <g data-layer="envelopes">
              {layout.envelopes.map((envelope) => {
                const lit = layout.zoom !== null || focusEntities.has(envelope.entityId);
                if (!lit) return null;
                return (
                  <g key={envelope.entityId} data-entity={envelope.entityId}>
                    <path
                      d={envelope.d}
                      fill={lit ? 'var(--raw-graph-accent)' : 'none'}
                      fillOpacity={0.04}
                      stroke="var(--raw-graph-text)"
                      strokeOpacity={lit ? 0.7 : 0.16}
                      strokeWidth={0.8}
                      className="transition-opacity duration-[var(--dur-state)]"
                    />
                    {lit ? (
                      <Label
                        x={Math.min(envelope.labelX, plot.x1 - 4)}
                        y={Math.max(envelope.labelY, plot.y0 + 10)}
                        anchor={envelope.labelX > plot.x1 - 90 ? 'end' : 'middle'}
                        size={LEGEND_PX}
                        mono={false}
                        tone="var(--raw-text-secondary)"
                      >
                        {envelope.label.toUpperCase()} · {envelope.count}
                      </Label>
                    ) : null}
                  </g>
                );
              })}
            </g>
            <g data-layer="relations">
              {[...layout.relations]
                .sort((a, b) => Number(a.loud) - Number(b.loud))
                .map((edge, index, all) => {
                  const style = relationStroke(edge.relation.kind);
                  // Loud labels alternate above and below their line so two
                  // disputes crossing near each other stay separately legible.
                  const loudIndex = all.slice(0, index).filter((other) => other.loud).length;
                  const lit = focus.set !== null && focus.set.has(edge.relation.source) && focus.set.has(edge.relation.target);
                  const dim = !edge.loud && focus.set !== null && !lit;
                  return (
                    <g key={edge.relation.id} data-relation={edge.relation.kind}>
                      <line
                        x1={edge.x1}
                        y1={edge.y1}
                        x2={edge.x2}
                        y2={edge.y2}
                        stroke={style.stroke}
                        strokeOpacity={dim ? 0.08 : edge.loud ? 0.95 : style.opacity * 0.7}
                        strokeWidth={edge.loud ? 2 : style.width}
                        strokeDasharray={style.dash}
                        className="transition-opacity duration-[var(--dur-state)]"
                      />
                      {edge.loud || lit ? (
                        <Label
                          x={(edge.x1 + edge.x2) / 2}
                          y={(edge.y1 + edge.y2) / 2 + (loudIndex % 2 === 0 ? -6 : 14)}
                          anchor="middle"
                          size={LEGEND_PX}
                          tone={style.stroke}
                        >
                          {style.label}
                        </Label>
                      ) : null}
                    </g>
                  );
                })}
            </g>
            <g data-layer="bodies">
              {layout.points
                .filter((point) => point.visible)
                .map((point) => {
                  const selected = point.fact.nodeId === focus.selectedNode;
                  const inspected = point.fact.nodeId === focus.inspectedNode;
                  const dimmed = !point.inZoom || (focus.set !== null && !focus.set.has(point.fact.nodeId));
                  return (
                    <g
                      key={point.fact.nodeId}
                      data-node={point.fact.nodeId}
                      data-fact-id={point.fact.factId}
                      data-access={point.fact.access}
                      data-inspected={inspected || undefined}
                      data-selected={selected || undefined}
                      opacity={dimmed ? 0.22 : 1}
                      className="cursor-pointer transition-opacity duration-[var(--dur-state)]"
                      onPointerMove={() => {
                        if (hovered === point.fact.nodeId) return;
                        setHovered(point.fact.nodeId);
                        onInspect(point.fact.factId);
                      }}
                      onClick={() => onSelect(point.fact.factId)}
                    >
                      <FocusMarks x={point.x} y={point.y} r={point.r} selected={selected} inspected={inspected} />
                      <FactMark fact={point.fact} x={point.x} y={point.y} r={point.r} hatchId={hatchId} />
                      <circle cx={point.x} cy={point.y} r={Math.max(point.r + 5, 10)} fill="transparent">
                        <title>
                          {point.fact.label} · {trustText(point.fact)} ·{' '}
                          {point.fact.retrievals == null ? 'retrievals absent' : `${point.fact.retrievals} retrievals`}
                        </title>
                      </circle>
                    </g>
                  );
                })}
            </g>
            <g data-layer="labels">
              {layout.points
                .filter((point) => point.visible && point.labelled && !isFocus(point.fact.nodeId))
                .map((point) => {
                  const right = point.x + 190 < plot.x1;
                  return (
                    <Label
                      key={`label:${point.fact.nodeId}`}
                      x={right ? point.x + point.r + 4 : point.x - point.r - 4}
                      y={point.y + 3.5}
                      anchor={right ? 'start' : 'end'}
                      opacity={focus.set !== null && !focus.set.has(point.fact.nodeId) ? 0.25 : 0.92}
                    >
                      {elideToWidth(point.fact.label, FIELD_LABEL_W, LABEL_PX)}
                    </Label>
                  );
                })}
              {layout.points
                .filter((point) => point.visible && isFocus(point.fact.nodeId))
                .map((point) => {
                  const text = elideToWidth(point.fact.label, FIELD_LABEL_W + 60, LABEL_PX);
                  const w = text.length * LABEL_PX * 0.6 + 8;
                  const right = point.x + point.r + 6 + w < layout.width;
                  const x = right ? point.x + point.r + 6 : point.x - point.r - 6 - w;
                  return (
                    <g key={`focus-label:${point.fact.nodeId}`} data-focus-label>
                      <rect x={x} y={point.y - 9} width={w} height={16} fill="var(--raw-surface-1)" stroke="var(--raw-graph-accent)" strokeOpacity={0.5} strokeWidth={0.8} />
                      <Label x={x + 4} y={point.y + 3.5} tone="var(--raw-text-primary)" opacity={1}>
                        {text}
                      </Label>
                    </g>
                  );
                })}
            </g>
          </g>
        </svg>
      </div>
      <Disputes scene={scene} disputes={disputes} onSelect={onSelect} onInspect={onInspect} />
    </VariantFrame>
  );
}

function Disputes({
  scene,
  disputes,
  onSelect,
  onInspect,
}: {
  scene: RendererProps['scene'];
  disputes: RendererProps['scene']['relations'];
  onSelect: (factId: string) => void;
  onInspect: (factId: string) => void;
}) {
  const complete = scene.model.coverage.completeness === 'complete';
  if (disputes.length === 0) {
    return (
      <p className="px-3 pb-1 text-3xs text-text-muted" data-testid="field-disputes">
        {complete
          ? 'no contradiction or supersession in this complete read'
          : `no contradiction or supersession among the drawn facts; this ${scene.model.coverage.completeness} read may not hold them all`}
      </p>
    );
  }
  return (
    <section aria-label="Contradictions and supersessions" className="px-3 pb-1" data-testid="field-disputes">
      <h3 className="td-legend py-1">contradictions · supersessions {disputes.length}</h3>
      <ul className="flex flex-col">
        {disputes.map((relation) => {
          const source = scene.byNode.get(relation.source);
          const target = scene.byNode.get(relation.target);
          const style = relationStroke(relation.kind);
          if (!source || !target) return null;
          return (
            <li key={relation.id}>
              <button
                type="button"
                className="flex min-h-[var(--touch-target-min)] w-full items-center gap-3 border-t border-edge-subtle/60 px-1 text-left text-2xs hover:bg-surface-2"
                onPointerMove={() => onInspect(source.factId)}
                onFocus={() => onInspect(source.factId)}
                onClick={() => onSelect(source.factId)}
                data-dispute={relation.kind}
              >
                <svg aria-hidden width="22" height="6" viewBox="0 0 22 6" className="shrink-0">
                  <line x1="1" y1="3" x2="21" y2="3" stroke={style.stroke} strokeWidth={2} strokeDasharray={style.dash} />
                </svg>
                <span className="td-legend w-28 shrink-0" style={{ color: style.stroke }}>
                  {style.label}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-text-primary">{source.label}</span>
                <span className="td-value shrink-0 text-text-secondary">{trustText(source)}</span>
                <span aria-hidden className="shrink-0 text-text-muted">→</span>
                <span className="min-w-0 flex-1 truncate font-mono text-text-primary">{target.label}</span>
                <span className="td-value shrink-0 text-text-secondary">{trustText(target)}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
