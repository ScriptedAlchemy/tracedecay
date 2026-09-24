/**
 * ANATOMY PLATE candidate renderer. Geometry comes from `plate.ts`; this file
 * draws it and wires the shared inspect state. Nothing here decides a number.
 */
import { useMemo } from 'react';

import { kindColorVars } from '../graph/kindColor.ts';
import { cn } from '../../ui/cn';
import type { NeighborsPayload, UndrawnNeighbour } from './model.ts';
import {
  layoutPlate,
  PLATE_FIELD_H,
  PLATE_HEAD_H,
  PLATE_ROW,
  type PlateFocusMeta,
  type PlateLayout,
} from './plate.ts';
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
  type Inspect,
  type PinnedSymbol,
} from './fieldKit.tsx';

const SEGMENT_GAP = 1.5;

/** Tick step for the call-site ruler: the smallest round step at least 28px apart. */
function niceStep(pxPerCall: number): number {
  for (const step of [1, 2, 5, 10, 20, 50, 100]) if (step * pxPerCall >= 28) return step;
  return 200;
}

function hairline(from: { x: number; y: number }, to: { x: number; y: number }): string {
  const mid = (from.x + to.x) / 2;
  return `M${from.x},${from.y} C${mid},${from.y} ${mid},${to.y} ${to.x},${to.y}`;
}

function Ruler({ layout, x0, x1, grow }: { layout: PlateLayout; x0: number; x1: number; grow: 1 | -1 }) {
  const { pxPerCall, maxCalls, y } = layout.scale;
  const anchor = grow === 1 ? x0 : x1;
  const step = niceStep(pxPerCall);
  const ticks: number[] = [];
  for (let n = 0; n <= maxCalls; n += step) ticks.push(n);
  return (
    <g aria-hidden>
      <line x1={anchor} x2={anchor + grow * maxCalls * pxPerCall} y1={y} y2={y} className="stroke-edge-strong" />
      {ticks.map((n) => (
        <g key={n}>
          <line x1={anchor + grow * n * pxPerCall} x2={anchor + grow * n * pxPerCall} y1={y - 3} y2={y + 3} className="stroke-edge-strong" />
          <text
            x={anchor + grow * n * pxPerCall}
            y={y + 14}
            textAnchor="middle"
            fontSize={10}
            className="fill-text-muted font-mono"
          >
            {n}
          </text>
        </g>
      ))}
    </g>
  );
}

function Plate({
  layout,
  model,
  inspect,
}: {
  layout: PlateLayout;
  model: TraceModel;
  inspect: Inspect;
}) {
  const focus = model.nodes.find((node) => node.id === model.focusId)!;
  const { x, y, width, height } = layout.plate;
  const chars = Math.floor((width - 28) / 6.6);
  return (
    <SymbolTarget model={model} node={focus} inspect={inspect} box={{ x, y, width, height }}>
      <rect x={x} y={y} width={width} height={height} rx={2} className="fill-surface-1 stroke-edge-strong" />
      <rect x={x + 3.5} y={y + 3.5} width={width - 7} height={height - 7} rx={1} fill="none" className="stroke-edge-subtle" />
      {/* The selection gutter: this plate IS the selected symbol. */}
      <rect x={x} y={y} width={3} height={height} className="fill-accent" />
      <text x={x + 14} y={y + 24} fontSize={15} className="fill-text-primary font-mono">
        {clip(focus.name, Math.floor((width - 28) / 9))}
      </text>
      <rect x={x + 14} y={y + 32} width={8} height={8} rx={1} style={kindColorVars(focus.kind)} className={KIND_FILL} />
      <text x={x + 27} y={y + 40} fontSize={10} className="td-legend fill-text-muted" letterSpacing="0.16em">
        {focus.kind.toUpperCase()}
      </text>
      <line x1={x + 10} x2={x + width - 10} y1={y + 48} y2={y + 48} className="stroke-edge-subtle" />
      {layout.fields.map((field, i) => {
        const fy = y + PLATE_HEAD_H + 14 + i * PLATE_FIELD_H;
        const value = field.label === 'file' ? clipStart(field.value, chars) : clip(field.value, chars);
        return (
          <g key={field.label}>
            <text x={x + 14} y={fy} fontSize={10} letterSpacing="0.12em" className="td-legend fill-text-muted">
              {field.label.toUpperCase()}
            </text>
            <text
              x={x + 14}
              y={fy + 13}
              fontSize={11}
              className={cn('font-mono', field.absent ? 'fill-state-unknown' : 'fill-text-primary')}
            >
              {value}
            </text>
          </g>
        );
      })}
    </SymbolTarget>
  );
}

export function PlateField({
  model,
  root,
  meta,
  undrawn,
  reduced,
  onPin,
}: {
  model: TraceModel;
  root: NeighborsPayload;
  meta: PlateFocusMeta;
  undrawn: readonly UndrawnNeighbour[];
  reduced: boolean;
  onPin?: ((node: PinnedSymbol) => void) | undefined;
}) {
  const { ref, width } = useHostWidth();
  const inspect = useInspect(model);
  const { signature, endLine } = meta;
  const layout = useMemo(
    () => (width > 0 ? layoutPlate(model, root, { signature, endLine }, undrawn, width) : null),
    [model, root, signature, endLine, undrawn, width],
  );
  const fade = reduced ? '' : 'motion-safe:transition-opacity motion-safe:duration-150';

  return (
    <div className="flex flex-col">
      <div ref={ref} data-trace-field="plate" className="w-full">
        {layout === null ? null : (
          <svg
            width={layout.width}
            height={layout.height}
            viewBox={`0 0 ${layout.width} ${layout.height}`}
            role="group"
            aria-roledescription="anatomy plate"
            aria-label={variantDescription(model, 'an anatomy plate, callers left and callees right on one call-site scale')}
            className="block"
          >
            {layout.columns.map((column) => (
              <g key={`${column.side}${column.hop}`}>
                <text
                  x={column.titleX}
                  y={column.titleY}
                  textAnchor={column.titleAnchor}
                  fontSize={10}
                  letterSpacing="0.16em"
                  className="td-legend fill-text-muted"
                >
                  {column.title.toUpperCase()}
                </text>
                {column.notes.map((note) => (
                  <text
                    key={note.text}
                    x={layout.stacked || column.side === 'down' ? column.x0 : column.x1}
                    y={note.y}
                    textAnchor={layout.stacked || column.side === 'down' ? 'start' : 'end'}
                    fontSize={10}
                    className="fill-state-unknown font-mono"
                  >
                    {note.text}
                  </text>
                ))}
              </g>
            ))}

            {layout.links.map((link) => {
              const lit = inspect.litChannel(link.key) && inspect.id !== null;
              return (
                <path
                  key={`${link.key}:${link.from.y}`}
                  d={hairline(link.from, link.to)}
                  fill="none"
                  strokeWidth={lit ? 1.6 : 1}
                  opacity={inspect.litChannel(link.key) ? 1 : DIM}
                  className={cn(lit ? 'stroke-accent' : 'stroke-edge-strong', fade)}
                />
              );
            })}
            {layout.ports.map((port) => (
              <line
                key={port.key}
                x1={port.x}
                x2={port.x}
                y1={port.y - 4}
                y2={port.y + 4}
                strokeWidth={2}
                opacity={inspect.litChannel(port.key) ? 1 : DIM}
                className={cn('stroke-text-secondary', fade)}
              />
            ))}

            <Plate layout={layout} model={model} inspect={inspect} />

            {layout.rows.map((row) => {
              const textX = row.anchorX;
              const anchor = row.grow === -1 ? 'end' : 'start';
              let cursor = row.anchorX;
              return (
                <g key={row.node.id} opacity={inspect.lit(row.node.id) ? 1 : DIM} className={fade}>
                  <SymbolTarget
                    model={model}
                    node={row.node}
                    inspect={inspect}
                    onPin={onPin}
                    box={{
                      x: row.column.x0 - 2,
                      y: row.y,
                      width: row.column.x1 - row.column.x0 + 4,
                      height: PLATE_ROW - 4,
                    }}
                  >
                    <text x={textX} y={row.y + 12} textAnchor={anchor} fontSize={11} className="fill-text-primary font-mono">
                      {row.name}
                    </text>
                    {row.segments.map((calls, i) => {
                      const length = calls * layout.scale.pxPerCall;
                      const x = row.grow === 1 ? cursor : cursor - length;
                      cursor += row.grow * (length + SEGMENT_GAP);
                      return (
                        <rect
                          key={i}
                          x={x}
                          y={row.y + 18}
                          width={length}
                          height={6}
                          style={kindColorVars(row.node.kind)}
                          className={KIND_FILL}
                          opacity={row.node.degree === null ? 0.45 : 0.92}
                        />
                      );
                    })}
                    <text x={textX} y={row.y + 37} textAnchor={anchor} fontSize={10} className="fill-text-muted font-mono">
                      {row.segments.length === 0 ? (
                        <tspan className="fill-state-unknown">no drawn channel inward · </tspan>
                      ) : (
                        <tspan className="fill-text-secondary">{`${row.segments.join('+')} · `}</tspan>
                      )}
                      {row.meta}
                    </text>
                  </SymbolTarget>
                </g>
              );
            })}

            {layout.columns
              .filter((column) => column.hop === 1 || layout.stacked)
              .slice(0, layout.stacked ? 1 : 2)
              .map((column) => (
                <Ruler
                  key={`ruler-${column.side}`}
                  layout={layout}
                  x0={column.x0}
                  x1={column.x1}
                  grow={!layout.stacked && column.side === 'up' ? -1 : 1}
                />
              ))}
            <text
              x={layout.stacked ? 12 : layout.plate.x + layout.plate.width / 2}
              y={layout.scale.y + 4}
              textAnchor={layout.stacked ? 'start' : 'middle'}
              fontSize={10}
              letterSpacing="0.16em"
              transform={layout.stacked ? 'translate(0 26)' : undefined}
              className="td-legend fill-text-muted"
            >
              ONE SCALE · CALL SITES PER CHANNEL
            </text>
            {layout.omittedChannels > 0 ? (
              <text
                x={layout.stacked ? 12 : layout.plate.x + layout.plate.width / 2}
                y={layout.scale.y + (layout.stacked ? 44 : 20)}
                textAnchor={layout.stacked ? 'start' : 'middle'}
                fontSize={10}
                className="fill-state-unknown font-mono"
              >
                {`${layout.omittedChannels} ${layout.omittedChannels === 1 ? 'channel' : 'channels'} between neighbours not on the plate`}
              </text>
            ) : null}
          </svg>
        )}
      </div>
      <InspectReadout model={model} inspect={inspect} />
      <div className="flex flex-wrap gap-x-4 gap-y-1.5 border-t border-edge-subtle px-3 py-2" data-testid="trace-key">
        <LegendEntry
          label="bar = call sites on one channel, one scale both sides; hue = kind"
          sample={<rect x={2} y={4} width={24} height={5} className="fill-text-secondary" />}
        />
        <LegendEntry
          label="segments = one channel each into the hop inside"
          sample={
            <>
              <rect x={2} y={4} width={13} height={5} className="fill-text-secondary" />
              <rect x={16.5} y={4} width={9} height={5} className="fill-text-secondary" />
            </>
          }
        />
        <LegendEntry
          label="hairline to a port = the channel itself"
          sample={
            <>
              <path d="M2 9 C14 9 14 3 25 3" fill="none" className="stroke-edge-strong" />
              <line x1={26} x2={26} y1={0} y2={6} strokeWidth={2} className="stroke-text-secondary" />
            </>
          }
        />
        <LegendEntry
          label="cyan gutter = the selected symbol"
          sample={<rect x={2} y={1} width={3} height={10} className="fill-accent" />}
        />
        <LegendEntry
          label="absent / not drawn = the wire was silent or the budget stopped"
          sample={<line x1={2} x2={26} y1={6} y2={6} strokeDasharray="3 3" className="stroke-state-unknown" />}
        />
      </div>
    </div>
  );
}
