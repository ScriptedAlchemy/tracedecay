/**
 * The Facts camera's field: provenance cameras over the memory graph the
 * overview served, inside the height the ledger leaves it.
 *
 * `cameras.ts` decides the geometry; this draws it and carries the reader's
 * three verbs. Hover inspects (and dims what the fact is not wired to), click
 * selects, and the selected fact's relations lift with one restrained halo
 * while the rest recede. Neither changes a measured value or fires activity.
 * The SVG is one `role="img"` whose label is the scene's own description; the
 * fact ledger below is the exact accessible equivalent and the keyboard path,
 * and the frame-open controls and the dispute rows are native buttons.
 */
import { useId, useMemo, useState } from 'react';

import type { MemoryReadStatusV1 } from '../../contracts/generated.ts';
import { cn } from '../../ui/cn';
import { Corners } from '../../ui/instrument.tsx';
import {
  CoverageFooter,
  FocalReadout,
  HatchDef,
  Label,
  MarkKey,
  RelationKey,
  TIER_PX,
  relationStroke,
  trustOpacity,
  trustText,
  useMeasuredBox,
} from './cameraMarks.tsx';
import { CAMERA_ROW_H, LABEL_X, layoutCameras, type CameraFrame, type CameraLayout, type FrameKey } from './cameras.ts';
import { ADVANCE, disputesOf, elideToWidth, hubFact, type FactScene, type SceneFact } from './factScene.ts';

export function FactCameras({
  scene,
  inspectedFactId,
  selectedFactId,
  onInspect,
  onSelect,
  graphRead,
}: {
  scene: FactScene;
  /** The fact the reader is inspecting (hover or focus, sticky until Escape). */
  inspectedFactId: string | null;
  /** The fact the reader has selected. Persistent; drawn as a gutter and a halo on its relations. */
  selectedFactId: string | null;
  onInspect: (factId: string) => void;
  onSelect: (factId: string) => void;
  graphRead: MemoryReadStatusV1 | undefined;
}) {
  const hatchId = useId();
  const [ref, box] = useMeasuredBox({ width: 960, height: 400 });
  const [hovered, setHovered] = useState<string | null>(null);
  // The opened frame, remembered against the selection it was chosen under:
  // a new selection returns the field to following it.
  const [opened, setOpened] = useState<{ open: { key: FrameKey } | null; selection: string | null } | null>(null);

  const nodeOf = (factId: string | null) => (factId == null ? null : (scene.nodeIdByFact.get(factId) ?? null));
  const selectedNode = nodeOf(selectedFactId);
  const inspectedNode = hovered ?? nodeOf(inspectedFactId);
  const focusNode = inspectedNode !== selectedNode ? inspectedNode : null;
  const selectedFact = selectedNode ? scene.byNode.get(selectedNode) : undefined;

  const resting = useMemo(() => layoutCameras(scene, box, null), [scene, box]);
  const followed = resting.mode === 'aggregate' && selectedFact ? { key: selectedFact.category } : null;
  const open = opened && opened.selection === selectedFactId ? opened.open : followed;
  const layout = useMemo(() => (open ? layoutCameras(scene, box, open) : resting), [scene, box, open, resting]);

  const focusSet = useMemo(() => {
    if (focusNode == null) return null;
    const out = new Set<string>([focusNode]);
    for (const neighbour of scene.neighbours.get(focusNode) ?? []) out.add(neighbour);
    if (selectedNode) out.add(selectedNode);
    return out;
  }, [focusNode, scene.neighbours, selectedNode]);

  const focal = focusNode ? scene.byNode.get(focusNode) : undefined;
  const readout = focal
    ? { fact: focal, role: 'inspecting' as const }
    : selectedFact
      ? { fact: selectedFact, role: 'selected' as const }
      : { fact: hubFact(scene), role: 'hub' as const };
  const disputes = disputesOf(scene);
  const setOpen = (next: { key: FrameKey } | null) => setOpened({ open: next, selection: selectedFactId });

  return (
    <figure
      className="td-optic td-grain relative flex min-h-0 flex-col lg:h-full"
      data-testid="fact-constellation"
      data-camera-mode={layout.mode}
    >
      <Corners tone="signal" />
      <figcaption className="relative z-10 flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3 pt-1.5 lg:flex-nowrap">
        <span className="flex shrink-0 flex-col gap-1.5">
          <span className="td-title text-text-secondary">Provenance cameras</span>
          <span className="td-legend">{axesNote(layout)}</span>
        </span>
        <span className="flex min-w-0 flex-1 basis-80 items-center gap-2 lg:basis-0">
          <FocalReadout fact={readout.fact} role={readout.role} />
        </span>
        {layout.mode === 'open' ? (
          <button
            type="button"
            onClick={() => setOpen(null)}
            className="td-hit shrink-0 border border-edge-subtle px-2 text-body text-text-secondary hover:bg-surface-2"
          >
            All categories
          </button>
        ) : null}
      </figcaption>
      <div
        ref={ref}
        role="region"
        aria-label="Provenance camera field"
        tabIndex={layout.height > box.height ? 0 : -1}
        className="relative z-10 min-h-0 flex-1 overflow-auto max-lg:h-[22rem] max-lg:flex-none"
      >
        <svg
          role="img"
          aria-label={sceneDescription(scene, layout)}
          width={layout.width}
          height={layout.height}
          viewBox={`0 0 ${layout.width} ${layout.height}`}
          className="block select-none"
          data-testid="fact-constellation-svg"
        >
          <defs>
            <HatchDef id={hatchId} />
          </defs>
          {layout.frames.map((frame) => (
            <Frame key={frame.title} frame={frame} layout={layout} hatchId={hatchId} />
          ))}
          <g data-layer="relations">
            {layout.relations.map(({ relation, d, midX, midY, vertical }) => {
              const style = relationStroke(relation.kind);
              const touches = (node: string | null) => node != null && (relation.source === node || relation.target === node);
              const lit = focusSet !== null ? focusSet.has(relation.source) && focusSet.has(relation.target) : false;
              const lifted = focusSet === null && touches(selectedNode);
              const recede = focusSet !== null ? !lit : selectedNode !== null && !lifted;
              return (
                <g key={relation.id} data-relation={relation.kind} data-lifted={lifted || undefined} data-lit={lit || undefined}>
                  {lifted ? (
                    <path d={d} fill="none" stroke="var(--raw-graph-accent)" strokeOpacity={0.16} strokeWidth={6} strokeLinejoin="round" data-halo />
                  ) : null}
                  <path
                    d={d}
                    fill="none"
                    stroke={style.stroke}
                    strokeOpacity={recede ? 0.14 : lifted || lit || style.loud ? 0.95 : style.opacity * 0.6}
                    strokeWidth={lifted ? style.width + 0.6 : style.width}
                    strokeDasharray={style.dash}
                    strokeLinejoin="round"
                    className="transition-opacity duration-[var(--dur-state)]"
                  />
                  {style.loud || lit || lifted ? (
                    <g transform={vertical ? `rotate(-90 ${midX} ${midY})` : undefined}>
                      <Label x={midX} y={vertical ? midY - 3 : midY + 3.5} tier="legend" anchor="middle" tone={style.stroke} opacity={recede ? 0.25 : 1}>
                        {style.label.toUpperCase()}
                      </Label>
                    </g>
                  ) : null}
                </g>
              );
            })}
            {layout.stubs.map((stub) => {
              const style = relationStroke(stub.relation.kind);
              const lifted = focusSet === null && stub.node === selectedNode;
              const lit = focusSet !== null && focusSet.has(stub.node);
              const recede = focusSet !== null ? !lit : selectedNode !== null && !lifted;
              return (
                <g key={`stub:${stub.relation.id}`} data-relation={stub.relation.kind} data-stub data-lifted={lifted || undefined} data-lit={lit || undefined}>
                  {lifted ? <path d={stub.d} stroke="var(--raw-graph-accent)" strokeOpacity={0.16} strokeWidth={6} data-halo /> : null}
                  <path
                    d={stub.d}
                    stroke={style.stroke}
                    strokeOpacity={recede ? 0.14 : lifted || lit || style.loud ? 0.95 : style.opacity * 0.6}
                    strokeWidth={lifted ? style.width + 0.6 : style.width}
                    strokeDasharray={style.dash}
                  >
                    <title>
                      {style.label} → a fact in {stub.other ? (stub.other.key ?? 'category absent') : 'no drawn frame'}
                    </title>
                  </path>
                </g>
              );
            })}
          </g>
          {layout.frames.flatMap((frame) =>
            frame.rows.map((row) => (
              <Row
                key={row.fact.nodeId}
                fact={row.fact}
                x={row.x}
                top={row.top}
                w={row.w}
                gx={row.gx}
                gy={row.gy}
                layout={layout}
                hatchId={hatchId}
                dimmed={focusSet !== null && !focusSet.has(row.fact.nodeId)}
                inspected={row.fact.nodeId === inspectedNode}
                selected={row.fact.nodeId === selectedNode}
                onEnter={() => {
                  if (hovered === row.fact.nodeId) return;
                  setHovered(row.fact.nodeId);
                  onInspect(row.fact.factId);
                }}
                onLeave={() => setHovered(null)}
                onClick={() => onSelect(row.fact.factId)}
              />
            )),
          )}
        </svg>
        {layout.mode === 'aggregate'
          ? layout.frames.map((frame) => (
              <button
                key={frame.title}
                type="button"
                onClick={() => setOpen({ key: frame.key })}
                aria-label={`Open ${frame.title}: ${frame.count} ${frame.count === 1 ? 'fact' : 'facts'}${frame.disputes > 0 ? `, ${frame.disputes} ${frame.disputes === 1 ? 'dispute' : 'disputes'}` : ''}`}
                className="absolute cursor-pointer bg-transparent hover:bg-surface-2/30"
                style={{ left: frame.x, top: frame.y, width: frame.w, height: frame.h }}
                data-open-frame={frame.title}
              />
            ))
          : null}
      </div>
      <Disputes scene={scene} disputes={disputes} selectedNode={selectedNode} onSelect={onSelect} onInspect={onInspect} />
      {/* One line of keys: the aggregate field draws no relation lines, so
        * it keys its marks only. */}
      <div className="relative z-10 flex flex-wrap items-center gap-x-5 gap-y-1 px-3 pt-1">
        <MarkKey disputedCap={layout.mode === 'aggregate'} />
        {layout.relations.length > 0 ? <RelationKey relations={layout.relations.map((route) => route.relation)} /> : null}
        {layout.mode !== 'aggregate' && layout.relationsOffField > 0 ? (
          <p className="text-3xs text-state-partial">
            {layout.relationsOffField} {layout.relationsOffField === 1 ? 'relation' : 'relations'} off this view
          </p>
        ) : null}
      </div>
      <CoverageFooter coverage={scene.coverage} graphRead={graphRead} unplaced={scene.unplaced} />
    </figure>
  );
}

function axesNote(layout: CameraLayout): string {
  switch (layout.mode) {
    case 'rows':
      return 'a frame per category';
    case 'aggregate':
      return 'open a frame for rows';
    case 'open':
      return 'one frame open';
    default: {
      const unhandled: never = layout.mode;
      return unhandled;
    }
  }
}

function sceneDescription(scene: FactScene, layout: CameraLayout): string {
  const { coverage } = scene;
  return (
    `Provenance cameras: ${coverage.drawnFacts} fact ${coverage.drawnFacts === 1 ? 'root' : 'roots'} in ` +
    `${layout.frames.length} category ${layout.frames.length === 1 ? 'frame' : 'frames'} (${layout.mode}), ` +
    `${scene.relations.length} fact-to-fact relations, ${disputesOf(scene).length} contradictions or supersessions; ` +
    `drawn from ${coverage.factCandidatesExamined} examined of ${coverage.factUniverse} facts in the store, graph coverage ${coverage.completeness}. ` +
    `Entities are cited by name, not resolved symbols. The fact ledger below this field is the exact accessible equivalent.`
  );
}

function Frame({ frame, layout, hatchId }: { frame: CameraFrame; layout: CameraLayout; hatchId: string }) {
  const k = 9;
  const { x, y, w, h } = frame;
  const corners = [
    `M ${x} ${y + k} V ${y} H ${x + k}`,
    `M ${x + w - k} ${y} H ${x + w} V ${y + k}`,
    `M ${x + w} ${y + h - k} V ${y + h} H ${x + w - k}`,
    `M ${x + k} ${y + h} H ${x} V ${y + h - k}`,
  ].join(' ');
  const title = elideToWidth(`${frame.title.toUpperCase()} · ${frame.count}`, w - 20, TIER_PX.legend, ADVANCE.legend);
  const titleW = title.length * TIER_PX.legend * ADVANCE.legend + 12;
  const firstRow = frame.rows[0];
  return (
    <g data-frame={frame.title}>
      <rect x={x} y={y} width={w} height={h} fill="var(--raw-surface-0)" fillOpacity={0.35} stroke="var(--raw-graph-edge)" strokeOpacity={0.35} strokeWidth={0.8} />
      <path d={corners} fill="none" stroke="var(--raw-graph-accent)" strokeOpacity={0.7} strokeWidth={1.2} />
      <Label x={x + 10} y={y + 15} tier="legend" tone="var(--raw-text-secondary)">
        {title}
        {/* One text run, so the browser spaces the cites after the real
          * title width rather than after an estimate of it. */}
        {frame.rail ? null : (
          <tspan dx={12} fill="var(--raw-graph-text)" fillOpacity={0.75}>
            {elideToWidth(
              frame.cites.length > 0
                ? `cites ${frame.cites.map((cite) => `${cite.label} ×${cite.count}`).join(' · ')}`
                : 'cites no entity by name',
              (firstRow ? firstRow.x - x : 0) + layout.rail.x - titleW - 34,
              TIER_PX.legend,
              ADVANCE.legend,
            ).toUpperCase()}
          </tspan>
        )}
      </Label>
      {frame.rail ? (
        <Aggregate frame={frame} rail={frame.rail} hatchId={hatchId} />
      ) : (
        <>
          {firstRow ? (
            <>
              <Label x={firstRow.x + layout.rail.x} y={y + 15} tier="legend" opacity={0.6}>
                0
              </Label>
              <Label x={firstRow.x + layout.rail.x + layout.rail.w} y={y + 15} tier="legend" opacity={0.6} anchor="end">
                1.00
              </Label>
            </>
          ) : null}
          {frame.overflowAt ? (
            <Label x={frame.overflowAt.x} y={frame.overflowAt.y} tier="legend" opacity={0.75}>
              {`+${frame.overflow} MORE IN THE LEDGER`}
            </Label>
          ) : null}
        </>
      )}
    </g>
  );
}

/** A frame too full to draw as rows: its exact count, and every fact as one
 * tick on the shared rail, brightening with trust. A fact with no trust is a
 * tick in the gutter left of zero (crosshatched when withheld), with its
 * count; a fact on a contradiction or supersession wears a conflict cap. */
function Aggregate({ frame, rail, hatchId }: { frame: CameraFrame; rail: NonNullable<CameraFrame['rail']>; hatchId: string }) {
  const gutterX = frame.x + 9;
  const absent = frame.ticks.filter((tick) => tick.x == null);
  return (
    <g data-aggregate={frame.count}>
      <line x1={rail.x} y1={rail.y} x2={rail.x + rail.w} y2={rail.y} stroke="var(--raw-graph-edge)" strokeOpacity={0.7} />
      <line x1={rail.x} y1={rail.y - 7} x2={rail.x} y2={rail.y + 7} stroke="var(--raw-graph-text)" strokeOpacity={0.6} />
      {frame.ticks.map((tick) => {
        const x = tick.x ?? gutterX;
        return (
          <g key={tick.fact.nodeId} data-tick={tick.fact.factId} data-disputed={tick.disputed || undefined}>
            {tick.fact.restricted || tick.x == null ? (
              <rect
                x={x - 1.5}
                y={rail.y - 6}
                width={3}
                height={12}
                fill={tick.fact.restricted ? `url(#${hatchId})` : 'none'}
                stroke={tick.fact.restricted ? 'var(--raw-state-locked)' : 'var(--raw-graph-text)'}
                strokeWidth={0.8}
                strokeDasharray={tick.fact.restricted ? undefined : '2 1.5'}
              />
            ) : (
              <rect x={x - 1} y={rail.y - 6} width={2} height={12} fill="var(--raw-graph-accent)" fillOpacity={trustOpacity(tick.fact.trust ?? 0)} />
            )}
            {tick.disputed ? <rect x={x - 2} y={rail.y - 11} width={4} height={3} fill="var(--raw-state-conflicting)" /> : null}
          </g>
        );
      })}
      {absent.length > 0 ? (
        <Label x={gutterX} y={rail.y + 18} tier="legend" anchor="middle" opacity={0.8}>
          {String(absent.length)}
        </Label>
      ) : null}
      <Label x={rail.x + rail.w / 2} y={rail.y + 19} tier="value" anchor="middle" tone="var(--raw-text-secondary)">
        {frame.trustRange ? `${frame.trustRange[0].toFixed(2)}–${frame.trustRange[1].toFixed(2)}` : 'trust absent'}
      </Label>
    </g>
  );
}

function Row({
  fact,
  x,
  top,
  w,
  gx,
  gy,
  layout,
  hatchId,
  dimmed,
  inspected,
  selected,
  onEnter,
  onLeave,
  onClick,
}: {
  fact: SceneFact;
  x: number;
  top: number;
  w: number;
  gx: number;
  gy: number;
  layout: CameraLayout;
  hatchId: string;
  dimmed: boolean;
  inspected: boolean;
  selected: boolean;
  onEnter: () => void;
  onLeave: () => void;
  onClick: () => void;
}) {
  const { rail } = layout;
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
      onPointerLeave={onLeave}
      onClick={onClick}
    >
      <rect x={x + 1} y={top + 1} width={w - 2} height={CAMERA_ROW_H - 2} fill={inspected ? 'var(--raw-surface-3)' : 'transparent'} fillOpacity={0.6} />
      {selected ? <rect x={x} y={top + 2} width={2} height={CAMERA_ROW_H - 4} fill="var(--raw-graph-accent)" data-selected-gutter /> : null}
      {fact.restricted ? (
        <rect x={gx - 4} y={gy - 4} width={8} height={8} fill={`url(#${hatchId})`} stroke="var(--raw-state-locked)" data-mark="restricted" />
      ) : fact.trust == null ? (
        <circle cx={gx} cy={gy} r={4} fill="none" stroke="var(--raw-graph-text)" strokeDasharray="2 2" data-mark="trust-absent" />
      ) : (
        <circle cx={gx} cy={gy} r={4} fill="var(--raw-graph-accent)" fillOpacity={trustOpacity(fact.trust)} data-mark="measured" />
      )}
      <Label x={x + LABEL_X} y={gy + 5} tier="body" tone={fact.restricted ? 'var(--raw-state-locked)' : 'var(--raw-graph-text)'}>
        {elideToWidth(fact.label, layout.labelW, TIER_PX.body, ADVANCE.body)}
      </Label>
      <line x1={x + rail.x} y1={gy} x2={x + rail.x + rail.w} y2={gy} stroke="var(--raw-graph-edge)" strokeOpacity={0.6} strokeWidth={1} />
      <line x1={x + rail.x} y1={gy - 5} x2={x + rail.x} y2={gy + 5} stroke="var(--raw-graph-text)" strokeOpacity={0.6} strokeWidth={1} />
      {fact.restricted ? (
        <rect x={x + rail.x} y={gy - 2} width={rail.w} height={4} fill={`url(#${hatchId})`} />
      ) : fill > 0 && fact.trust != null ? (
        <rect
          x={x + rail.x}
          y={gy - 2}
          width={fill}
          height={4}
          fill="var(--raw-graph-accent)"
          fillOpacity={trustOpacity(fact.trust)}
          data-rail-width={Math.round(fill * 100) / 100}
        />
      ) : null}
      <Label x={x + w - 8} y={gy + 4} anchor="end" tier="value" tone={fact.trust == null ? 'var(--raw-text-muted)' : 'var(--raw-text-primary)'}>
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

/** Every contradiction and supersession among the drawn facts, as exact rows;
 * a row selects the relation's source. */
function Disputes({
  scene,
  disputes,
  selectedNode,
  onSelect,
  onInspect,
}: {
  scene: FactScene;
  disputes: readonly FactScene['relations'][number][];
  selectedNode: string | null;
  onSelect: (factId: string) => void;
  onInspect: (factId: string) => void;
}) {
  if (disputes.length === 0) {
    return (
      <p className="relative z-10 px-3 pt-1 text-3xs text-text-muted" data-testid="fact-disputes">
        {scene.coverage.completeness === 'complete'
          ? 'no contradiction or supersession in this complete read'
          : `no contradiction or supersession among the drawn facts; this ${scene.coverage.completeness} read may not hold them all`}
      </p>
    );
  }
  return (
    <section aria-label="Contradictions and supersessions" className="relative z-10 px-3 pt-1" data-testid="fact-disputes">
      <ul className="grid gap-x-3 lg:grid-cols-2">
        {disputes.map((relation) => {
          const source = scene.byNode.get(relation.source);
          const target = scene.byNode.get(relation.target);
          if (!source || !target) return null;
          const style = relationStroke(relation.kind);
          const current = selectedNode === relation.source || selectedNode === relation.target;
          return (
            <li key={relation.id} className="min-w-0">
              <button
                type="button"
                className={cn(
                  'flex min-h-[var(--touch-target-min)] w-full min-w-0 flex-col justify-center gap-0.5 border-l-2 px-2 text-left hover:bg-surface-2',
                  current ? 'border-accent bg-surface-2/60' : 'border-transparent',
                )}
                onPointerMove={() => onInspect(source.factId)}
                onFocus={() => onInspect(source.factId)}
                onClick={() => onSelect(source.factId)}
                data-dispute={relation.kind}
              >
                <span className="flex items-center gap-2">
                  <svg aria-hidden width="22" height="6" viewBox="0 0 22 6" className="shrink-0">
                    <line x1="1" y1="3" x2="21" y2="3" stroke={style.stroke} strokeWidth={2} strokeDasharray={style.dash} />
                  </svg>
                  <span className="td-legend" style={{ color: style.stroke }}>
                    {style.label}
                  </span>
                  <span className="td-value ml-auto text-xs text-text-secondary" data-cell="numeric">
                    {trustText(source)} → {trustText(target)}
                  </span>
                </span>
                <span className="flex min-w-0 items-baseline gap-1.5 text-body">
                  <span className={cn('min-w-0 flex-1 truncate', source.restricted ? 'text-state-locked' : 'text-text-primary')}>
                    {source.label}
                  </span>
                  <span aria-hidden className="shrink-0 text-text-muted">
                    →
                  </span>
                  <span className={cn('min-w-0 flex-1 truncate', target.restricted ? 'text-state-locked' : 'text-text-primary')}>
                    {target.label}
                  </span>
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
