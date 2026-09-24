/**
 * The TRACE anatomy plate. Geometry comes from `plate.ts`; this file draws it
 * and wires the inspect state. Nothing here decides a number.
 *
 * Connectors are faint hairlines at one fixed opacity. Where a corridor
 * carries several, they overlap exactly, so a trunk's brightness is the
 * compositing of its links, a count, not a styling choice. The inspected
 * route is redrawn on top in cyan and every other connector steps back.
 */
import { useMemo } from 'react';

import { kindColorVars } from '../graph/kindColor.ts';
import { cn } from '../../ui/cn';
import type { NeighborsPayload, UndrawnNeighbour } from './model.ts';
import {
  kindShape,
  layoutPlate,
  PLATE_FIELD_H,
  PLATE_HEAD_H,
  PLATE_ROW,
  type PlateFocusMeta,
  type PlateLayout,
} from './plate.ts';
import type { TraceModel } from './types.ts';
import { clip, clipStart, plateDescription } from './inspect.ts';
import {
  DIM,
  InspectReadout,
  KIND_FILL,
  KindGlyph,
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

/** One connector's opacity at rest; overlapping ones composite into a trunk. */
const CONNECTOR_ALPHA = 0.2;

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
      <text x={x + 14} y={y + 24} fontSize={14} className="fill-text-primary font-mono">
        {clip(focus.name, Math.floor((width - 28) / 9))}
      </text>
      <KindGlyph shape={kindShape(focus.kind)} x={x + 18} y={y + 36} size={8} style={kindColorVars(focus.kind)} className={KIND_FILL} />
      {/* Selection said in words and position, never by hue alone. */}
      <text x={x + width - 12} y={y + 40} textAnchor="end" fontSize={10} letterSpacing="0.16em" className="td-legend fill-accent">
        SELECTED
      </text>
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
            aria-label={plateDescription(model)}
            className="block tabular-nums"
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

            <g aria-hidden data-connectors>
              {layout.connectors.map((connector) => (
                <path
                  key={connector.key}
                  d={connector.d}
                  fill="none"
                  strokeWidth={1}
                  opacity={inspect.id === null ? CONNECTOR_ALPHA : inspect.litChannel(connector.key) ? 0 : CONNECTOR_ALPHA / 3}
                  className={cn('stroke-text-secondary', fade)}
                />
              ))}
              {inspect.id === null
                ? null
                : layout.connectors
                    .filter((connector) => inspect.litChannel(connector.key))
                    .map((connector) => (
                      <path
                        key={`lit:${connector.key}`}
                        data-lit
                        d={connector.d}
                        fill="none"
                        strokeWidth={1.6}
                        className="stroke-accent"
                      />
                    ))}
            </g>

            <Plate layout={layout} model={model} inspect={inspect} />
            {layout.throughPorts.map((port) => (
              <line
                key={port.x}
                aria-hidden
                x1={port.x}
                x2={port.x}
                y1={port.y - 5}
                y2={port.y + 5}
                strokeWidth={2}
                className="stroke-text-secondary"
              />
            ))}

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
                    <KindGlyph
                      shape={kindShape(row.node.kind)}
                      x={textX + row.grow * 4}
                      y={row.y + 8}
                      style={kindColorVars(row.node.kind)}
                      className={KIND_FILL}
                    />
                    <text x={textX + row.grow * 14} y={row.y + 12} textAnchor={anchor} fontSize={11} className="fill-text-primary font-mono">
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
            {layout.crossLinks > 0 ? (
              <text
                x={layout.stacked ? 12 : layout.plate.x + layout.plate.width / 2}
                y={layout.scale.y + (layout.stacked ? 44 : 20)}
                textAnchor={layout.stacked ? 'start' : 'middle'}
                fontSize={10}
                className="fill-state-unknown font-mono"
              >
                {`${layout.crossLinks} of ${model.channels.length} links run between neighbours: connector only, no bar`}
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
          label="connector = one call link, caller port to callee port; a trunk brightens with the links it carries"
          sample={
            <>
              <path d="M2 3 H12 V9 H26" fill="none" opacity={CONNECTOR_ALPHA} className="stroke-text-secondary" />
              <path d="M2 3 H12 V9 H26" fill="none" opacity={CONNECTOR_ALPHA * 3} className="stroke-text-secondary" />
            </>
          }
        />
        <LegendEntry
          label="shape + hue = kind (● fn ◆ method ■ struct ▲ trait ▬ module)"
          sample={
            <>
              <circle cx={4} cy={6} r={3} className="fill-text-secondary" />
              <path d="M12 2.5 L15.5 6 L12 9.5 L8.5 6 Z" className="fill-text-secondary" />
              <rect x={18} y={3} width={6} height={6} className="fill-text-secondary" />
            </>
          }
        />
        <LegendEntry
          label="cyan gutter + SELECTED = the traced symbol"
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
