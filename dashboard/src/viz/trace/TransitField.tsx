/**
 * TRANSIT MAP candidate renderer. Geometry comes from `transit.ts`; depth
 * comes from the strata read, and only from it.
 */
import { useMemo } from 'react';

import type { StrataMeasurementV1 } from '../../contracts/generated.ts';
import { absenceReason, type StructureResult } from '../../data/query/structure.ts';
import { kindColorVars } from '../graph/kindColor.ts';
import { cn } from '../../ui/cn';
import type { UndrawnNeighbour } from './model.ts';
import { gapReading, layoutTransit, type TransitDepths, type TransitLine } from './transit.ts';
import type { TraceModel } from './types.ts';
import { clip, variantDescription } from './variants.ts';
import {
  DIM,
  InspectReadout,
  KIND_FILL,
  LegendEntry,
  SymbolTarget,
  useHostWidth,
  useInspect,
  type PinnedSymbol,
} from './fieldKit.tsx';

export function transitDepths(
  strata: StructureResult<StrataMeasurementV1> | undefined,
): TransitDepths & { note: string } {
  if (strata === undefined) return { byPath: null, maxDepth: null, floor: false, note: 'strata read pending' };
  if (strata.outcome !== 'measured') {
    return { byPath: null, maxDepth: null, floor: false, note: `strata ${strata.outcome}: ${absenceReason(strata)}` };
  }
  const m = strata.measurement;
  const floor =
    m.scan.files_examined >= m.scan.max_files ||
    m.scan.dependency_edges_examined >= m.scan.max_dependency_edges;
  return {
    byPath: new Map(m.files.map((file) => [file.path, file.depth])),
    maxDepth: m.max_depth,
    floor,
    note: `${m.granularity}-level depth, ${m.algorithm}, ${m.files.length} files${floor ? ', scan capped so every depth is a floor' : ''}`,
  };
}

function lineClass(line: TransitLine, lit: boolean): string {
  if (lit) return 'stroke-accent';
  switch (line.kind) {
    case 'climb':
      return 'stroke-state-error';
    case 'unmeasured':
      return 'stroke-state-unknown';
    case 'descend':
    case 'level':
      return 'stroke-text-secondary';
    default: {
      const exhaustive: never = line.kind;
      return exhaustive;
    }
  }
}

export function TransitField({
  model,
  undrawn,
  strata,
  reduced,
  onPin,
}: {
  model: TraceModel;
  undrawn: readonly UndrawnNeighbour[];
  strata: StructureResult<StrataMeasurementV1> | undefined;
  reduced: boolean;
  onPin?: ((node: PinnedSymbol) => void) | undefined;
}) {
  const { ref, width } = useHostWidth();
  const inspect = useInspect(model);
  const depths = useMemo(() => transitDepths(strata), [strata]);
  const layout = useMemo(
    () => (width > 0 ? layoutTransit(model, depths, width) : null),
    [model, depths, width],
  );
  const fade = reduced ? '' : 'motion-safe:transition-opacity motion-safe:duration-150';
  const hidden = (ring: number) =>
    undrawn.filter((entry) => entry.hop === Math.abs(ring) && (ring < 0 ? entry.side === 'up' : entry.side === 'down')).length;
  const climbs = layout?.lines.filter((line) => line.kind === 'climb').length ?? 0;

  return (
    <div className="flex flex-col">
      <div ref={ref} data-trace-field="transit" className="w-full">
        {layout === null ? null : (
          <svg
            width={layout.width}
            height={layout.height}
            viewBox={`0 0 ${layout.width} ${layout.height}`}
            role="group"
            aria-roledescription="transit map"
            aria-label={variantDescription(model, 'a transit map, hop distance across and measured file dependency depth down')}
            className="block"
          >
            <defs>
              <pattern id="trace-transit-hatch" width={6} height={6} patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
                <line x1={0} y1={0} x2={0} y2={6} strokeWidth={1} className="stroke-edge-subtle" />
              </pattern>
            </defs>

            {layout.columns.map((column) => (
              <g key={column.ring}>
                <text
                  x={column.x}
                  y={14}
                  fontSize={10}
                  letterSpacing="0.16em"
                  textAnchor={layout.compact ? 'middle' : 'start'}
                  className={cn('td-legend', column.ring === 0 ? 'fill-accent' : 'fill-text-muted')}
                >
                  {(layout.compact ? (column.ring === 0 ? 'focus' : String(column.ring)) : column.title).toUpperCase()}
                </text>
                {column.ring !== 0 && hidden(column.ring) > 0 && !layout.compact ? (
                  <text x={column.x} y={27} fontSize={10} className="fill-state-unknown font-mono">
                    {`+${hidden(column.ring)} named, not drawn`}
                  </text>
                ) : null}
              </g>
            ))}

            {layout.bands.map((band) => (
              <g key={band.key}>
                {band.kind === 'station' ? (
                  <rect x={layout.gutter - 8} y={band.y} width={layout.width - layout.gutter} height={band.height} className="fill-surface-1" opacity={0.55} />
                ) : (
                  <rect
                    x={layout.gutter - 8}
                    y={band.y}
                    width={layout.width - layout.gutter}
                    height={band.height}
                    fill="url(#trace-transit-hatch)"
                    strokeDasharray={band.kind === 'unmeasured' ? '3 3' : undefined}
                    className={band.kind === 'unmeasured' ? 'stroke-state-unknown' : undefined}
                  />
                )}
                <line x1={layout.gutter - 8} x2={layout.width - 4} y1={band.y} y2={band.y} className="stroke-edge-subtle" />
                <text x={4} y={band.y + (band.kind === 'empty' ? 14 : 16)} fontSize={10} className={cn('font-mono', band.kind === 'station' ? 'fill-text-secondary' : 'fill-state-unknown')}>
                  {band.kind === 'unmeasured' ? (layout.compact ? 'd ?' : 'depth absent') : layout.compact ? `d ${band.depth}` : `depth ${band.depth}`}
                </text>
                {band.kind === 'empty' && !layout.compact ? (
                  <text x={layout.gutter + 4} y={band.y + 14} fontSize={10} letterSpacing="0.16em" className="td-legend fill-text-muted">
                    NO STATION
                  </text>
                ) : null}
                {band.kind === 'unmeasured' && !layout.compact ? (
                  <text x={layout.gutter + 4} y={band.y + 13} fontSize={10} letterSpacing="0.12em" className="td-legend fill-state-unknown">
                    DEPTH UNMEASURED · FILE NOT IN THE STRATA READ
                  </text>
                ) : null}
              </g>
            ))}

            {layout.lines.map((line) => {
              const lit = inspect.id !== null && inspect.litChannel(line.key);
              return (
                <path
                  key={line.key}
                  d={line.d}
                  fill="none"
                  strokeWidth={line.width}
                  strokeLinejoin="round"
                  strokeLinecap="round"
                  strokeDasharray={line.kind === 'unmeasured' ? '4 3' : undefined}
                  opacity={lit ? 1 : inspect.litChannel(line.key) ? 0.72 : DIM}
                  className={cn(lineClass(line, lit), fade)}
                />
              );
            })}

            {layout.stations.map((station) => {
              const node = station.node;
              const isFocus = node.id === model.focusId;
              const showLabel = !layout.compact || isFocus || inspect.id === node.id;
              const label = clip(node.name, isFocus ? layout.labelChars + 4 : layout.labelChars);
              return (
                <g key={node.id} opacity={inspect.lit(node.id) ? 1 : DIM} className={fade}>
                  <SymbolTarget
                    model={model}
                    node={node}
                    inspect={inspect}
                    onPin={onPin}
                    box={{
                      x: station.x - 8,
                      y: station.y - 17,
                      width: layout.compact ? 16 : Math.min(label.length * 6.7 + 20, 200),
                      height: 24,
                    }}
                  >
                    {isFocus ? (
                      <circle cx={station.x} cy={station.y} r={6.5} strokeWidth={2.5} className="fill-surface-0 stroke-accent" />
                    ) : (
                      <circle
                        cx={station.x}
                        cy={station.y}
                        r={4.5}
                        strokeWidth={1.5}
                        strokeDasharray={node.degree === null ? '2 2' : undefined}
                        style={kindColorVars(node.kind)}
                        className={cn('stroke-surface-0', node.degree === null ? 'fill-none stroke-state-unknown' : KIND_FILL)}
                      />
                    )}
                    {showLabel ? (
                      <text
                        x={station.x + 8}
                        y={station.y - 6}
                        fontSize={11}
                        stroke="var(--raw-surface-0)"
                        strokeWidth={3}
                        paintOrder="stroke"
                        className={cn('font-mono', isFocus ? 'fill-accent' : 'fill-text-primary')}
                      >
                        {label}
                      </text>
                    ) : null}
                  </SymbolTarget>
                </g>
              );
            })}

            {/* Foot ruler: per-corridor depth delta of the lines that cross it. */}
            <line x1={layout.gutter - 8} x2={layout.width - 4} y1={layout.rulerY - 10} y2={layout.rulerY - 10} className="stroke-edge-strong" />
            {layout.gaps.map((gap, i) => (
              <g key={i}>
                <line x1={gap.x} x2={gap.x} y1={layout.rulerY - 14} y2={layout.rulerY - 6} className="stroke-edge-strong" />
                <text x={gap.x} y={layout.rulerY + 6} fontSize={10} textAnchor="middle" className={cn('font-mono', gap.climbs > 0 ? 'fill-state-error' : 'fill-text-secondary')}>
                  {layout.compact ? (gap.climbs > 0 ? `↑${gap.climbs}` : '') : gapReading(gap)}
                </text>
              </g>
            ))}
            <text x={4} y={layout.rulerY + 6} fontSize={10} className="fill-text-muted font-mono">
              {layout.compact ? 'Δ' : 'Δ per hop'}
            </text>
            <text x={4} y={layout.rulerY + 26} fontSize={10} className="fill-text-muted font-mono">
              {clip(depths.note, Math.floor(layout.width / 6.3))}
            </text>
          </svg>
        )}
      </div>
      <InspectReadout model={model} inspect={inspect} />
      <div className="flex flex-wrap gap-x-4 gap-y-1.5 border-t border-edge-subtle px-3 py-2" data-testid="trace-key">
        <LegendEntry
          label="band = the symbol's FILE dependency depth, measured by strata, 0 at top"
          sample={<rect x={1} y={2} width={26} height={8} className="fill-surface-1 stroke-edge-subtle" />}
        />
        <LegendEntry
          label="line weight = call sites on the channel"
          sample={<path d="M2 9 H10 L18 3 H26" fill="none" strokeWidth={2.4} className="stroke-text-secondary" />}
        />
        <LegendEntry
          label={`climb into a shallower file (${climbs} here): an observed boundary crossing, not proof of a bug`}
          sample={<path d="M2 10 H14 V2 H26" fill="none" strokeWidth={2} className="stroke-state-error" />}
        />
        <LegendEntry
          label="dashed = a depth the read did not measure; hatched = no station at that depth"
          sample={<path d="M2 6 H26" fill="none" strokeWidth={2} strokeDasharray="4 3" className="stroke-state-unknown" />}
        />
      </div>
    </div>
  );
}

