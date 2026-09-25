/**
 * The Trace plate's DOM interaction: the measured width it lays out against,
 * the inspect state a hover or keyboard focus sets, and the focusable symbol
 * target.
 *
 * Hover and keyboard focus both INSPECT (light the symbol's drawn route to the
 * focus); only click or Enter re-centres the trace, which is the same
 * production path the list's re-centre control takes. The 2px cyan focus mark
 * is drawn only for keyboard focus, so a pointer never fakes it.
 */
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
} from 'react';

import type { KindShape } from './plate.ts';
import type { TraceModel, TraceNode } from './types.ts';
import { inspectLine, inspectPath, type InspectedPath } from './inspect.ts';

/** Kind hue as an SVG fill or stroke, through the same vars the list uses. */
export const KIND_FILL = 'fill-[var(--kind-dark)] [[data-theme=light]_&]:fill-[var(--kind-light)]';

/** The pin target a renderer hands back: what the trace knows of a symbol. */
export interface PinnedSymbol {
  id: string;
  kind: string;
  name: string;
  file_path: string | null;
  start_line: number | null;
  degree: number | null;
}

function pinnedFrom(node: TraceNode): PinnedSymbol {
  return {
    id: node.id,
    kind: node.kind,
    name: node.name,
    file_path: node.filePath,
    start_line: node.startLine,
    degree: node.degree,
  };
}

export interface Inspect {
  /** The symbol being inspected, by hover first, then keyboard focus. */
  readonly id: string | null;
  /** The symbol holding keyboard focus with a visible focus ring. */
  readonly focusVisible: string | null;
  readonly path: InspectedPath;
  /** Whether a mark is on the inspected route (true when nothing is inspected). */
  lit(id: string): boolean;
  litChannel(key: string): boolean;
  hover(id: string | null): void;
  focus(id: string | null, visible: boolean): void;
}

export function useInspect(model: TraceModel): Inspect {
  const [hovered, setHovered] = useState<string | null>(null);
  const [focused, setFocused] = useState<{ id: string; visible: boolean } | null>(null);
  const id = hovered ?? focused?.id ?? null;
  const path = inspectPath(model, id);
  return {
    id,
    focusVisible: focused?.visible ? focused.id : null,
    path,
    lit: (node) => id === null || path.nodes.has(node),
    litChannel: (key) => id === null || path.channels.has(key),
    hover: setHovered,
    focus: (next, visible) => setFocused(next === null ? null : { id: next, visible }),
  };
}

/** A focusable symbol mark. `box` is the focus ring, in the SVG's own units. */
export function SymbolTarget({
  model,
  node,
  inspect,
  onPin,
  box,
  children,
}: {
  model: TraceModel;
  node: TraceNode;
  inspect: Inspect;
  onPin?: ((node: PinnedSymbol) => void) | undefined;
  box: { x: number; y: number; width: number; height: number };
  children: ReactNode;
}) {
  const pin = useCallback(() => {
    if (node.id !== model.focusId) onPin?.(pinnedFrom(node));
  }, [model.focusId, node, onPin]);
  const onKeyDown = (event: KeyboardEvent<SVGGElement>) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      pin();
    }
  };
  return (
    <g
      data-node={node.id}
      data-ring={node.ring}
      tabIndex={0}
      role="button"
      aria-label={inspectLine(model, node.id)}
      className="cursor-pointer outline-none"
      onPointerEnter={() => inspect.hover(node.id)}
      onPointerLeave={() => inspect.hover(null)}
      onFocus={(event) => inspect.focus(node.id, event.currentTarget.matches(':focus-visible'))}
      onBlur={() => inspect.focus(null, false)}
      onClick={pin}
      onKeyDown={onKeyDown}
    >
      {/* Transparent hit area, so the whole labelled mark answers the pointer. */}
      <rect x={box.x} y={box.y} width={box.width} height={box.height} fill="transparent" />
      {children}
      {inspect.focusVisible === node.id ? (
        <rect
          data-focus-ring
          x={box.x - 2}
          y={box.y - 2}
          width={box.width + 4}
          height={box.height + 4}
          rx={2}
          fill="none"
          strokeWidth={2}
          className="stroke-accent"
        />
      ) : null}
    </g>
  );
}

/** The one line under a candidate field that says what is being inspected. */
export function InspectReadout({ model, inspect }: { model: TraceModel; inspect: Inspect }) {
  return (
    <p
      aria-live="polite"
      data-trace-inspect
      className="min-h-10 border-t border-edge-subtle px-3 py-2 text-sm text-text-secondary"
    >
      {inspect.id === null ? (
        'Hover or focus a symbol to light its drawn route to the focus. Click or Enter re-centres the trace on it.'
      ) : (
        <span className="font-mono tabular-nums">{inspectLine(model, inspect.id)}</span>
      )}
    </p>
  );
}

/** A kind's shape cue, centred on (x, y); the hue comes from the caller. */
export function KindGlyph({
  shape,
  x,
  y,
  size = 7,
  className,
  style,
}: {
  shape: KindShape;
  x: number;
  y: number;
  size?: number;
  className?: string;
  style?: CSSProperties;
}) {
  const h = size / 2;
  const common = { className, style };
  switch (shape) {
    case 'circle':
      return <circle cx={x} cy={y} r={h} {...common} />;
    case 'square':
      return <rect x={x - h} y={y - h} width={size} height={size} {...common} />;
    case 'diamond':
      return <path d={`M${x},${y - h - 0.5} L${x + h + 0.5},${y} L${x},${y + h + 0.5} L${x - h - 0.5},${y} Z`} {...common} />;
    case 'triangle':
      return <path d={`M${x},${y - h - 0.5} L${x + h + 0.5},${y + h} L${x - h - 0.5},${y + h} Z`} {...common} />;
    case 'bar':
      return <rect x={x - h - 1} y={y - h / 2} width={size + 2} height={h} {...common} />;
    default: {
      const exhaustive: never = shape;
      return exhaustive;
    }
  }
}

/** A legend entry: a small drawn sample and what it encodes. */
export function LegendEntry({ sample, label }: { sample: ReactNode; label: string }) {
  return (
    <div className="flex min-w-0 items-center gap-1.5">
      <svg aria-hidden width={28} height={12} viewBox="0 0 28 12" className="shrink-0">
        {sample}
      </svg>
      <span className="text-3xs leading-snug text-text-muted">{label}</span>
    </div>
  );
}

/** Opacity for a mark off the inspected route. */
export const DIM = 0.18;

/** A host ref and its width in CSS px; 0 until laid out. */
export function useHostWidth() {
  const ref = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const host = ref.current;
    if (!host) return;
    const measure = () => setWidth(Math.round(host.getBoundingClientRect().width));
    measure();
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    return () => observer.disconnect();
  }, []);
  return { ref, width };
}
