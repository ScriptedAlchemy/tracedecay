import { useId, useMemo, useState } from 'react';

import { elideToWidth } from './factScene.ts';
import { useSceneFocus } from './focus.ts';
import { layoutLattice } from './lattice.ts';
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
import type { RendererProps } from './types.ts';

/** Below this hub distance a bundle's label would sit on the hub labels. */
const SHORT_BUNDLE = 110;
const FOCUS_LABEL_W = 220;

/** Variant C, constellation lattice: entity hubs, fact satellites, relations
 * bundled per hub pair and kind with their exact counts. */
export function LatticeRenderer({ scene, inspectedFactId, selectedFactId, onInspect, onSelect, graphRead }: RendererProps) {
  const hatchId = useId();
  const [ref, box] = useMeasuredBox({ width: 960, height: 360 });
  const layout = useMemo(() => layoutLattice(scene, box), [scene, box]);
  const [hovered, setHovered] = useState<string | null>(null);
  const focus = useSceneFocus(scene, hovered, inspectedFactId, selectedFactId);
  const focusHubs = new Set(focus.set ? (focus.fact?.entityIds ?? []) : []);
  const hubDimmed = (id: string) => focus.set !== null && !focusHubs.has(id);

  return (
    <VariantFrame
      variant="lattice"
      title="Constellation lattice"
      axes="hubs · entities cited by name, force-settled · satellites · their facts, luminance = trust"
      scene={scene}
      graphRead={graphRead}
      description={`Constellation lattice: ${layout.hubs.length} entity hubs with ${layout.satellites.length} fact satellites, ${layout.bundles.length} relation bundles. The fact ledger is the exact accessible equivalent.`}
      focal={<FocalReadout fact={focus.fact} role={focus.role} />}
      keys={
        <>
          <MarkKey unconfirmed />
          <RelationKey scene={scene} />
          <p className="text-3xs text-text-muted">
            hub position is force-settled, not a measurement; hubs group by cited name, not resolved symbol
            {layout.relationsIntoAggregates > 0 ? ` · ${layout.relationsIntoAggregates} relations reach folded satellites` : ''}
          </p>
        </>
      }
    >
      <div ref={ref} className="h-[clamp(200px,28vh,360px)] w-full">
        <svg
          role="img"
          aria-label="Constellation lattice of entity hubs and fact satellites"
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
          <g data-layer="orbits">
            {layout.hubs.map((hub) => (
              <circle
                key={hub.id}
                cx={hub.x}
                cy={hub.y}
                r={hub.orbit}
                fill="none"
                stroke="var(--raw-graph-edge)"
                strokeOpacity={hubDimmed(hub.id) ? 0.1 : 0.3}
                strokeWidth={0.7}
              />
            ))}
          </g>
          <g data-layer="spokes">
            {layout.spokes.map((spoke) => (
              <line
                key={`${spoke.factNodeId}-${spoke.hubId}`}
                x1={spoke.x1}
                y1={spoke.y1}
                x2={spoke.x2}
                y2={spoke.y2}
                stroke="var(--raw-graph-edge)"
                strokeOpacity={focus.set !== null && !focus.set.has(spoke.factNodeId) ? 0.06 : spoke.primary ? 0.35 : 0.5}
                strokeWidth={0.7}
                strokeDasharray={spoke.primary ? undefined : '1.5 3'}
              />
            ))}
          </g>
          <g data-layer="bundles">
            {layout.bundles.map((bundle) => {
              const style = relationStroke(bundle.kind);
              const lit =
                focus.set !== null &&
                bundle.relations.some((relation) => focus.set!.has(relation.source) && focus.set!.has(relation.target));
              const dim = focus.set !== null && !lit;
              return (
                <g key={bundle.key} data-relation={bundle.kind} data-bundle-count={bundle.count}>
                  <path
                    d={bundle.d}
                    fill="none"
                    stroke={style.stroke}
                    strokeOpacity={dim ? 0.08 : style.opacity}
                    strokeWidth={style.width + Math.log2(bundle.count) * 1.2}
                    strokeDasharray={style.dash}
                    strokeLinecap="round"
                    className="transition-opacity duration-[var(--dur-state)]"
                  />
                  {lit || (bundle.length >= SHORT_BUNDLE && (bundle.count > 1 || style.loud)) ? (
                    <Label x={bundle.midX} y={bundle.midY + 3.5} anchor="middle" size={LEGEND_PX} tone={style.stroke} opacity={dim ? 0.2 : 1}>
                      {style.loud || lit ? `${style.label} ` : ''}
                      {bundle.count > 1 ? `×${bundle.count}` : ''}
                    </Label>
                  ) : null}
                </g>
              );
            })}
            {layout.chords.map((chord) => {
              const style = relationStroke(chord.relation.kind);
              return (
                <path
                  key={chord.relation.id}
                  d={chord.d}
                  data-relation={chord.relation.kind}
                  fill="none"
                  stroke={style.stroke}
                  strokeOpacity={focus.set !== null && !focus.set.has(chord.relation.source) ? 0.08 : style.opacity}
                  strokeWidth={style.width}
                  strokeDasharray={style.dash}
                />
              );
            })}
          </g>
          <g data-layer="hubs">
            {layout.hubs.map((hub) => (
              <g key={hub.id} data-hub={hub.id} opacity={hubDimmed(hub.id) ? 0.3 : 1} className="transition-opacity duration-[var(--dur-state)]">
                <polygon
                  points={`${hub.x},${hub.y - hub.r} ${hub.x + hub.r},${hub.y} ${hub.x},${hub.y + hub.r} ${hub.x - hub.r},${hub.y}`}
                  fill="var(--raw-surface-0)"
                  stroke="var(--raw-graph-text)"
                  strokeOpacity={0.8}
                  strokeWidth={1}
                />
                <Label x={hub.x} y={hub.y + hub.orbit + 14} anchor="middle" size={LEGEND_PX} mono={false} tone="var(--raw-text-secondary)">
                  {elideToWidth(hub.label, 150, LEGEND_PX).toUpperCase()} · {hub.count}
                </Label>
              </g>
            ))}
          </g>
          <g data-layer="aggregates">
            {layout.aggregates.map((aggregate) => (
              <g key={aggregate.hubId} data-aggregate={aggregate.count}>
                <circle cx={aggregate.x} cy={aggregate.y} r={7} fill="var(--raw-surface-0)" stroke="var(--raw-graph-text)" strokeDasharray="1 2" />
                <Label x={aggregate.x} y={aggregate.y + 3.5} anchor="middle" size={LEGEND_PX}>
                  +{aggregate.count}
                </Label>
              </g>
            ))}
          </g>
          <g data-layer="bodies">
            {layout.satellites.map((satellite) => {
              const selected = satellite.fact.nodeId === focus.selectedNode;
              const inspected = satellite.fact.nodeId === focus.inspectedNode;
              const dimmed = focus.set !== null && !focus.set.has(satellite.fact.nodeId);
              return (
                <g
                  key={satellite.fact.nodeId}
                  data-node={satellite.fact.nodeId}
                  data-fact-id={satellite.fact.factId}
                  data-access={satellite.fact.access}
                  data-inspected={inspected || undefined}
                  data-selected={selected || undefined}
                  opacity={dimmed ? 0.22 : 1}
                  className="cursor-pointer transition-opacity duration-[var(--dur-state)]"
                  onPointerMove={() => {
                    if (hovered === satellite.fact.nodeId) return;
                    setHovered(satellite.fact.nodeId);
                    onInspect(satellite.fact.factId);
                  }}
                  onClick={() => onSelect(satellite.fact.factId)}
                >
                  <FocusMarks x={satellite.x} y={satellite.y} r={satellite.r} selected={selected} inspected={inspected} />
                  <FactMark
                    fact={satellite.fact}
                    x={satellite.x}
                    y={satellite.y}
                    r={satellite.r}
                    hatchId={hatchId}
                    hollowRing={satellite.unconfirmed}
                  />
                  <circle cx={satellite.x} cy={satellite.y} r={Math.max(satellite.r + 4, 8)} fill="transparent">
                    <title>
                      {satellite.fact.label} · {trustText(satellite.fact)}
                    </title>
                  </circle>
                </g>
              );
            })}
          </g>
          <g data-layer="labels">
            {layout.satellites
              .filter((satellite) => satellite.fact.nodeId === focus.inspectedNode || satellite.fact.nodeId === focus.selectedNode)
              .map((satellite) => {
                const text = elideToWidth(satellite.fact.label, FOCUS_LABEL_W, LABEL_PX);
                const room = text.length * LABEL_PX * 0.6 + satellite.r + 8;
                const anchor =
                  satellite.labelAnchor === 'start' && satellite.x + room > layout.width
                    ? 'end'
                    : satellite.labelAnchor === 'end' && satellite.x - room < 0
                      ? 'start'
                      : satellite.labelAnchor;
                return (
                  <Label
                    key={`label:${satellite.fact.nodeId}`}
                    x={anchor === 'start' ? satellite.x + satellite.r + 6 : satellite.x - satellite.r - 6}
                    y={satellite.y + 3.5}
                    anchor={anchor}
                    tone="var(--raw-text-primary)"
                  >
                    {text}
                  </Label>
                );
              })}
          </g>
        </svg>
      </div>
    </VariantFrame>
  );
}
