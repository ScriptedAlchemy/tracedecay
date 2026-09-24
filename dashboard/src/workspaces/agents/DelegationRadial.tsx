import { useId, useMemo, useRef } from 'react';
import { cn } from '../../ui/cn';
import {
  OpenedStrip,
  Swatch,
  TopologyPopulation,
  useApertureWidth,
  type TopologyInteraction,
} from './DelegationTopology.tsx';
import { RingGlyph, RingHoverCard, ringCountLine, ringRadius } from './delegationRings.tsx';
import { cullLabels, layoutDelegationRadial, radialPitch, type LabelBox } from './delegationRadial.ts';
import { neighbourhood, type FittedTopology } from './delegationTopology.ts';
import { markHandlers } from './topologyVariant.tsx';

/**
 * The radial delegation field: generation as ring, siblings by angle, the
 * reading's fan-out as bracket arcs. Labels print for the first two rings and
 * for whatever is inspected or selected; every other mark is named by its
 * control and by the exact tree, so a dense outer ring never overprints.
 */

const HIT = 44;
const MIN_SIZE = 420;
const MAX_SIZE = 640;
/** Mono 11px advances about 6.6px; rounded up so a measured box never
 * truncates the name it was measured for. */
const CHAR = 7;
const LABEL_HEIGHT = 26;

export function DelegationRadial({
  fit,
  interaction,
}: {
  fit: FittedTopology;
  interaction: TopologyInteraction;
}) {
  const { model } = fit;
  const { inspectedId, selectedId } = interaction;
  const apertureRef = useRef<HTMLDivElement | null>(null);
  const measured = useApertureWidth(apertureRef);
  const width = Math.max(MIN_SIZE, measured ?? 720);
  const size = Math.min(MAX_SIZE, width);
  const hatchId = useId();
  const pitch = radialPitch(model, size);
  const radial = useMemo(() => layoutDelegationRadial(model, pitch), [model, pitch]);
  const byId = useMemo(() => new Map(model.marks.map((mark) => [mark.id, mark])), [model]);
  const keep = useMemo(
    () => (inspectedId !== null && byId.has(inspectedId) ? neighbourhood(model, inspectedId) : null),
    [model, byId, inspectedId],
  );
  const cx = width / 2;
  const cy = size / 2;
  const dim = (id: string) => keep !== null && !keep.has(id);
  const radiusOf = (id: string) => ringRadius(byId.get(id)!, model.maxDescendants);
  const tops = model.marks.filter((mark) => mark.parentId === null).length;
  const labelText = (mark: (typeof model.marks)[number]) =>
    mark.kind === 'bundle' ? `${mark.sessions} × ${mark.label}` : mark.label;
  const labelBox = (mark: (typeof model.marks)[number]): LabelBox & { side: 'below' | 'right' | 'left' } => {
    const at = radial.placements.get(mark.id)!;
    const r = radiusOf(mark.id);
    const width = Math.min(160, Math.max(labelText(mark).length, ringCountLine(mark).length) * CHAR + 6);
    if (at.radius === 0) {
      return { id: mark.id, side: 'below', x: cx + at.x - width / 2, y: cy + at.y + r + 6, width, height: LABEL_HEIGHT };
    }
    const right = Math.cos(at.angle) >= -0.01;
    return {
      id: mark.id,
      side: right ? 'right' : 'left',
      x: right ? cx + at.x + r + 8 : cx + at.x - r - 8 - width,
      y: cy + at.y - 13,
      width,
      height: LABEL_HEIGHT,
    };
  };
  // The origin caption and inspected/selected labels always print;
  // generations 0 and 1 print widest subtree first while they do not collide,
  // and ring captions fill whatever room is left.
  const captionCos = Math.cos(radial.captionAngle);
  const captionSin = Math.sin(radial.captionAngle);
  const ringCaption = (radius: number): LabelBox => ({
    id: `ring:${radius}`,
    x: cx + captionCos * radius - 22,
    y: cy + captionSin * radius - 8,
    width: 44,
    height: 14,
  });
  const originBox: LabelBox = { id: 'origin', x: cx - 70, y: cy - 10, width: 140, height: 50 };
  const labelled = cullLabels([
    ...(radial.centre === 'origin' ? [originBox] : []),
    ...model.marks
      .filter((mark) => mark.id === inspectedId || mark.id === selectedId || mark.generation <= 1)
      .sort((a, b) => {
        const pinned = (id: string) => (id === inspectedId || id === selectedId ? 1 : 0);
        const weight = (mark: (typeof model.marks)[number]) =>
          mark.kind === 'bundle' ? mark.sessions + mark.descendants : mark.node.descendants;
        return pinned(b.id) - pinned(a.id) || a.generation - b.generation || weight(b) - weight(a) || a.row - b.row;
      })
      .map(labelBox),
    ...radial.rings.map(ringCaption),
  ]);

  return (
    <div className="flex min-w-0 flex-col gap-2" data-delegation-radial={model.drawnSessions} data-radial-centre={radial.centre}>
      <div
        ref={apertureRef}
        role="group"
        aria-label="Radial delegation field"
        tabIndex={0}
        className="td-optic td-grain td-scanlines relative max-h-[48rem] overflow-auto"
        onMouseLeave={() => interaction.onInspect(null)}
        onBlur={(event) => {
          const next = event.relatedTarget;
          if (!(next instanceof Node) || !event.currentTarget.contains(next)) interaction.onInspect(null);
        }}
      >
        <div className="relative" style={{ width, height: size }}>
          <svg aria-hidden width={width} height={size} className="absolute inset-0">
            <defs>
              <pattern id={hatchId} width="5" height="5" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
                <line x1="0" y1="0" x2="0" y2="5" stroke="var(--raw-state-conflicting)" strokeWidth="1.4" />
              </pattern>
            </defs>
            <g transform={`translate(${cx},${cy})`}>
              {radial.rings.map((radius, index) => (
                <g key={radius}>
                  <circle r={radius} fill="none" stroke="var(--raw-graph-edge)" strokeOpacity={0.5} strokeDasharray="1 4" />
                  {labelled.has(`ring:${radius}`) ? (
                    <text
                      x={captionCos * radius}
                      y={captionSin * radius}
                      dy={3}
                      textAnchor="middle"
                      fill="var(--raw-graph-accent)"
                      fillOpacity={0.8}
                      fontSize={10}
                      fontFamily="var(--font-mono)"
                      letterSpacing="0.12em"
                      paintOrder="stroke"
                      stroke="var(--raw-graph-substrate)"
                      strokeWidth={3}
                    >
                      GEN {radial.centre === 'top' ? index + 1 : index}
                    </text>
                  ) : null}
                </g>
              ))}
              {radial.centre === 'origin' ? (
                <g data-radial-origin>
                  <path d="M-8 0 H8 M0 -8 V8" stroke="var(--raw-graph-text)" strokeOpacity={0.6} />
                  <text y={22} textAnchor="middle" fill="var(--raw-graph-text)" fillOpacity={0.75} fontSize={10} fontFamily="var(--font-mono)" letterSpacing="0.12em">
                    READING · {tops} TOPS
                  </text>
                  <text y={36} textAnchor="middle" fill="var(--raw-graph-text)" fillOpacity={0.5} fontSize={10} fontFamily="var(--font-mono)">
                    origin, not a session
                  </text>
                </g>
              ) : null}
              {radial.fans.map((fan) => {
                const lit = keep !== null && keep.has(fan.parentId) && fan.childIds.some((id) => keep.has(id));
                return (
                  <path
                    key={fan.parentId}
                    d={fan.path}
                    fill="none"
                    stroke={lit ? 'var(--raw-graph-accent)' : 'var(--raw-graph-edge)'}
                    strokeWidth={lit ? 1.4 : 1}
                    strokeOpacity={keep !== null && !lit ? 0.18 : lit ? 1 : 0.7}
                    strokeDasharray={fan.childIds.every((id) => byId.get(id)?.kind === 'bundle') ? '3 3' : undefined}
                    data-radial-fan={fan.childIds.length}
                  />
                );
              })}
              {model.stubs.map((stub) => {
                const at = radial.placements.get(stub.to);
                if (at === undefined) return null;
                const r = radiusOf(stub.to);
                const inner = Math.max(0, at.radius - r - 32);
                const outer = at.radius - r - 3;
                const cos = Math.cos(at.angle);
                const sin = Math.sin(at.angle);
                return stub.kind === 'missing_parent' ? (
                  <g key={stub.id} opacity={dim(stub.to) ? 0.25 : 1} data-topology-stub="missing_parent">
                    <line x1={cos * inner} y1={sin * inner} x2={cos * outer} y2={sin * outer} stroke="var(--raw-graph-alert)" strokeWidth={1.3} strokeDasharray="4 3" />
                    <circle cx={cos * (inner - 4)} cy={sin * (inner - 4)} r={3.5} fill="none" stroke="var(--raw-graph-alert)" strokeDasharray="2 2" />
                  </g>
                ) : (
                  <path
                    key={stub.id}
                    d={`M${at.x - r},${at.y - 2} C${at.x - r - 18},${at.y - 24} ${at.x + r + 18},${at.y - 24} ${at.x + r},${at.y - 2}`}
                    fill="none"
                    stroke="var(--raw-state-conflicting)"
                    strokeWidth={1.3}
                    strokeDasharray="3 2"
                    opacity={dim(stub.to) ? 0.25 : 1}
                    data-topology-stub="cycle"
                  />
                );
              })}
              {model.marks.map((mark) => {
                const at = radial.placements.get(mark.id)!;
                return (
                  <RingGlyph
                    key={mark.id}
                    mark={mark}
                    at={at}
                    radius={radiusOf(mark.id)}
                    hatchId={hatchId}
                    inspected={mark.id === inspectedId}
                    selected={mark.id === selectedId}
                    dim={dim(mark.id)}
                  />
                );
              })}
            </g>
          </svg>
          <ul aria-label="Sessions and bundles by ring" className="absolute inset-0 m-0 list-none p-0">
            {model.marks.map((mark) => {
              const at = radial.placements.get(mark.id)!;
              const box = labelled.has(mark.id) ? labelBox(mark) : null;
              return (
                <li key={mark.id} className="contents">
                  <button
                    type="button"
                    className={cn('absolute transition-opacity duration-[var(--dur-state)]', dim(mark.id) && 'opacity-40')}
                    style={{ left: cx + at.x - HIT / 2, top: cy + at.y - HIT / 2, width: HIT, height: HIT }}
                    {...markHandlers(mark, interaction)}
                  />
                  {box ? (
                    <span
                      aria-hidden
                      className={cn(
                        'pointer-events-none absolute flex flex-col transition-opacity duration-[var(--dur-state)]',
                        dim(mark.id) && 'opacity-40',
                        box.side === 'below' ? 'items-center' : box.side === 'right' ? 'items-start' : 'items-end',
                      )}
                      style={{ left: box.x, top: box.y, width: box.width, color: 'var(--raw-graph-text)' }}
                      data-radial-label={mark.id}
                    >
                      <span className="max-w-full truncate font-mono text-2xs tabular-nums">{labelText(mark)}</span>
                      <span className="max-w-full truncate font-mono text-3xs tabular-nums opacity-70">{ringCountLine(mark)}</span>
                    </span>
                  ) : null}
                </li>
              );
            })}
          </ul>
          {inspectedId !== null && byId.has(inspectedId) ? (
            <RingHoverCard
              mark={byId.get(inspectedId)!}
              at={{ x: cx + radial.placements.get(inspectedId)!.x, y: cy + radial.placements.get(inspectedId)!.y }}
              radius={radiusOf(inspectedId)}
            />
          ) : null}
        </div>
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1.5 px-1 text-3xs text-text-muted">
        <TopologyPopulation model={model} fit={fit} />
        <Swatch label="ring · generation">
          <path d="M2 14 A12 12 0 0 1 22 14" fill="none" stroke="var(--raw-graph-edge)" strokeDasharray="1 3" />
        </Swatch>
        <Swatch label="bracket arc · fan-out">
          <path d="M12 15 V9 M4 9 A10 10 0 0 1 20 9 M4 9 V3 M20 9 V3" fill="none" stroke="var(--raw-graph-edge)" strokeWidth={1.1} />
        </Swatch>
        <Swatch label="size · sessions beneath">
          <circle cx={12} cy={8} r={6} fill="var(--raw-graph-accent)" fillOpacity={0.12} stroke="var(--raw-graph-accent)" strokeWidth={1.2} />
        </Swatch>
        <Swatch label="cut edge · parent not in reading">
          <path d="M3 8 H15" stroke="var(--raw-graph-alert)" strokeWidth={1.3} strokeDasharray="3 2" />
        </Swatch>
        <span>labels · generations 0–1, hover and selection</span>
      </div>
      <OpenedStrip model={model} onToggleExpanded={interaction.onToggleExpanded} />
    </div>
  );
}
