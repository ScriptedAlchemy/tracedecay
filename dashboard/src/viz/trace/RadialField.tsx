/**
 * RADIAL NEIGHBOURHOOD candidate renderer. Geometry comes from `radial.ts`.
 * Labels: ring 1 always (wide), anything else only while inspected.
 */
import { useMemo } from 'react';

import { kindColorVars } from '../graph/kindColor.ts';
import { cn } from '../../ui/cn';
import type { UndrawnNeighbour } from './model.ts';
import { arcPath, layoutRadial, sectorReading, type RadialNode } from './radial.ts';
import type { TraceModel } from './types.ts';
import { clip, clipStart, variantDescription } from './variants.ts';
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

const DEG = 180 / Math.PI;

/** A label set along its spoke, flipped on the left half so it never reads upside down. */
function spokeLabel(entry: RadialNode, text: string, offset: number) {
  const left = Math.cos(entry.angle) < 0;
  const r = entry.radius + offset;
  const x = entry.x + (r - entry.radius) * Math.cos(entry.angle);
  const y = entry.y + (r - entry.radius) * Math.sin(entry.angle);
  const rotate = entry.angle * DEG + (left ? 180 : 0);
  return { x, y, anchor: left ? ('end' as const) : ('start' as const), transform: `rotate(${rotate.toFixed(1)} ${x.toFixed(1)} ${y.toFixed(1)})`, text };
}

export function RadialField({
  model,
  undrawn,
  reduced,
  onPin,
}: {
  model: TraceModel;
  undrawn: readonly UndrawnNeighbour[];
  reduced: boolean;
  onPin?: ((node: PinnedSymbol) => void) | undefined;
}) {
  const { ref, width } = useHostWidth();
  const inspect = useInspect(model);
  const layout = useMemo(
    () => (width > 0 ? layoutRadial(model, undrawn, width) : null),
    [model, undrawn, width],
  );
  const fade = reduced ? '' : 'motion-safe:transition-opacity motion-safe:duration-150';

  return (
    <div className="flex flex-col">
      <div ref={ref} data-trace-field="radial" className="w-full">
        {layout === null ? null : (
          <svg
            width={layout.width}
            height={layout.height}
            viewBox={`0 0 ${layout.width} ${layout.height}`}
            role="group"
            aria-roledescription="radial neighbourhood"
            aria-label={variantDescription(model, 'a radial map, hop distance as rings and modules as sectors')}
            className="block"
          >
            {layout.rings.map((ring) => (
              <g key={ring.hop} aria-hidden>
                <circle cx={layout.cx} cy={layout.cy} r={ring.r} fill="none" strokeDasharray="1 5" className="stroke-edge-strong" />
                <text
                  x={layout.cx - ring.r}
                  y={layout.cy + 3}
                  textAnchor="middle"
                  fontSize={10}
                  letterSpacing="0.16em"
                  stroke="var(--raw-surface-0)"
                  strokeWidth={4}
                  paintOrder="stroke"
                  className="td-legend fill-text-muted"
                >
                  {`${ring.hop} ${ring.hop === 1 ? 'HOP' : 'HOPS'}`}
                </text>
              </g>
            ))}

            {layout.sectors.map((sector) => {
              const mid = (sector.a0 + sector.a1) / 2;
              const reading = sectorReading(sector);
              const labelR = layout.aggregateR + 14;
              const lx = layout.cx + labelR * Math.cos(mid);
              const ly = layout.cy + labelR * Math.sin(mid);
              const left = Math.cos(mid) < -0.15;
              const right = Math.cos(mid) > 0.15;
              return (
                <g key={sector.key}>
                  <path
                    d={arcPath(layout.cx, layout.cy, layout.aggregateR, sector.a0, sector.a1)}
                    fill="none"
                    strokeWidth={1}
                    className="stroke-edge-strong"
                  />
                  {sector.hiddenSymbols > 0 || sector.hiddenEdges !== 0 ? (
                    <path
                      d={arcPath(layout.cx, layout.cy, layout.aggregateR + 4, sector.a0, sector.a1)}
                      fill="none"
                      strokeWidth={Math.min(6, 1 + Math.sqrt(sector.hiddenSymbols + (sector.hiddenEdges ?? 0)) * 0.6)}
                      strokeDasharray={sector.hiddenEdges === null ? '3 3' : undefined}
                      className="stroke-state-unknown"
                      opacity={0.7}
                    />
                  ) : null}
                  {layout.compact ? null : (
                    <text
                      x={lx}
                      y={ly + (left || right ? 3 : Math.sin(mid) > 0 ? 10 : -4)}
                      textAnchor={left ? 'end' : right ? 'start' : 'middle'}
                      fontSize={10}
                      className="font-mono"
                    >
                      <tspan className={sector.path === null ? 'fill-state-unknown' : 'fill-text-muted'}>
                        {clipStart(sector.label, 24)}
                      </tspan>
                      {reading ? <tspan className="fill-state-unknown">{` ${reading}`}</tspan> : null}
                    </text>
                  )}
                </g>
              );
            })}

            {layout.edges.map((edge) => {
              const lit = inspect.id !== null && inspect.litChannel(edge.key);
              return (
                <path
                  key={edge.key}
                  d={edge.d}
                  fill="none"
                  strokeWidth={lit ? edge.width + 0.6 : edge.width}
                  opacity={lit ? 1 : inspect.id === null ? 0.34 : DIM * 0.6}
                  className={cn(lit || edge.upstream ? 'stroke-accent' : 'stroke-text-secondary', fade)}
                />
              );
            })}

            {layout.nodes.map((entry) => {
              const node = entry.node;
              const isFocus = node.id === model.focusId;
              const hop = Math.abs(node.ring);
              const showLabel = isFocus || inspect.id === node.id || (!layout.compact && hop === 1);
              const size = isFocus ? 9 : hop === 1 ? 4.5 : 3.5;
              const label = isFocus ? null : spokeLabel(entry, clip(node.name, 16), size + 5);
              return (
                <g key={node.id} opacity={inspect.lit(node.id) ? 1 : DIM} className={fade}>
                  <SymbolTarget
                    model={model}
                    node={node}
                    inspect={inspect}
                    onPin={onPin}
                    box={{ x: entry.x - size - 4, y: entry.y - size - 4, width: size * 2 + 8, height: size * 2 + 8 }}
                  >
                    {isFocus ? (
                      <circle cx={entry.x} cy={entry.y} r={size} strokeWidth={2.5} className="fill-surface-0 stroke-accent" />
                    ) : (
                      <circle
                        cx={entry.x}
                        cy={entry.y}
                        r={size}
                        strokeWidth={1.2}
                        strokeDasharray={node.degree === null ? '2 2' : undefined}
                        style={kindColorVars(node.kind)}
                        className={cn('stroke-surface-0', node.degree === null ? 'fill-none stroke-state-unknown' : KIND_FILL)}
                      />
                    )}
                    {showLabel && label ? (
                      <text
                        x={label.x}
                        y={label.y + 3.5}
                        textAnchor={label.anchor}
                        transform={label.transform}
                        fontSize={11}
                        stroke="var(--raw-surface-0)"
                        strokeWidth={3}
                        paintOrder="stroke"
                        className="fill-text-primary font-mono"
                      >
                        {label.text}
                      </text>
                    ) : null}
                    {isFocus ? (
                      <text
                        x={entry.x}
                        y={entry.y + size + 16}
                        textAnchor="middle"
                        fontSize={13}
                        stroke="var(--raw-surface-0)"
                        strokeWidth={3}
                        paintOrder="stroke"
                        className="fill-text-primary font-mono"
                      >
                        {clip(node.name, 26)}
                      </text>
                    ) : null}
                  </SymbolTarget>
                </g>
              );
            })}
          </svg>
        )}
      </div>
      <InspectReadout model={model} inspect={inspect} />
      <div className="flex flex-wrap gap-x-4 gap-y-1.5 border-t border-edge-subtle px-3 py-2" data-testid="trace-key">
        <LegendEntry
          label="ring = hop distance; sector = the module (directory of the row's file)"
          sample={<path d="M2 11 A12 12 0 0 1 26 11" fill="none" strokeDasharray="1 3" className="stroke-edge-strong" />}
        />
        <LegendEntry
          label="cyan curve = caller side, ink = callee side; weight = call sites"
          sample={
            <>
              <path d="M2 3 Q14 3 26 3" fill="none" strokeWidth={1.6} className="stroke-accent" />
              <path d="M2 9 Q14 9 26 9" fill="none" strokeWidth={1.6} className="stroke-text-secondary" />
            </>
          }
        />
        <LegendEntry
          label="outer arc = per module: +symbols named but not drawn · edges no channel carries"
          sample={<path d="M2 6 H26" strokeWidth={3} className="stroke-state-unknown" />}
        />
        <LegendEntry
          label="labels: ring 1 and the inspected symbol; the list below names every one"
          sample={<circle cx={14} cy={6} r={3.5} className="fill-text-secondary" />}
        />
      </div>
    </div>
  );
}
