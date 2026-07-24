import type { ReactNode } from 'react';
import { cn } from './cn';
import { channelNumber } from '../app/channels.ts';

/**
 * Instrument primitives — the vocabulary the whole dashboard is drawn in.
 *
 * The grammar is deliberately small and repeated everywhere, because that
 * repetition is what makes a console read as one machined object rather than a
 * pile of cards:
 *
 *   Corners   four hairline brackets that frame a region like a bezel
 *   Panel     bracketed region with an engraved legend and a fill rule
 *   Readout   one measured quantity: legend above, mono value, quiet unit
 *   ReadoutBar a ruled row of readouts divided by hairlines
 *   Lamp      the only element allowed to signal liveness with colour
 *   Ticks     a graduated edge, so a region has a measuring scale
 *
 * Nothing here fabricates a value. Every component renders exactly what it is
 * handed, and renders an em dash when it is handed nothing.
 */

/** Four corner brackets, drawn just outside a region's own hairline so the
 * region reads as a machined bezel rather than a rounded card. */
export function Corners({
  className,
  tone = 'edge',
  size = 6,
}: {
  className?: string;
  /** `signal` marks a region that is instrumented live (the graph field). */
  tone?: 'edge' | 'signal';
  size?: number;
}) {
  const color = tone === 'signal' ? 'border-accent' : 'border-edge-strong';
  const common = 'pointer-events-none absolute';
  const style = { width: size, height: size };
  return (
    <span aria-hidden className={cn('contents', className)}>
      <span className={cn(common, '-left-px -top-px border-l border-t', color)} style={style} />
      <span className={cn(common, '-right-px -top-px border-r border-t', color)} style={style} />
      <span className={cn(common, '-bottom-px -left-px border-b border-l', color)} style={style} />
      <span className={cn(common, '-bottom-px -right-px border-b border-r', color)} style={style} />
    </span>
  );
}

/** An engraved legend followed by a hairline that runs to the edge of its
 * region. The single most repeated mark in this design. */
export function Legend({
  children,
  trailing,
  className,
}: {
  children: ReactNode;
  trailing?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex min-w-0 items-center gap-2', className)}>
      <span className="td-legend truncate text-text-secondary">{children}</span>
      <span aria-hidden className="td-rule" />
      {trailing}
    </div>
  );
}

/** A graduated edge: evenly spaced hairline ticks, every fifth one long. Gives
 * a region a measuring scale instead of a blank border. */
export function Ticks({
  count = 40,
  className,
  edge = 'top',
}: {
  count?: number;
  className?: string;
  edge?: 'top' | 'bottom';
}) {
  return (
    <span
      aria-hidden
      className={cn(
        'pointer-events-none absolute inset-x-0 flex items-end justify-between overflow-hidden',
        edge === 'top' ? 'top-0 items-start' : 'bottom-0 items-end',
        className,
      )}
      style={{ height: 7 }}
    >
      {Array.from({ length: count }, (_, index) => (
        <span
          key={index}
          className="w-px shrink-0 bg-edge-strong"
          style={{ height: index % 5 === 0 ? 6 : 3, opacity: index % 5 === 0 ? 0.85 : 0.45 }}
        />
      ))}
    </span>
  );
}

/** A bracketed instrument panel: hairline bezel, engraved legend on a fill
 * rule, square corners. Replaces the rounded card everywhere. */
export function Panel({
  legend,
  actions,
  footer,
  children,
  className,
  bodyClassName,
  tone = 'edge',
}: {
  legend: string;
  actions?: ReactNode;
  footer?: ReactNode;
  children: ReactNode;
  className?: string;
  bodyClassName?: string;
  tone?: 'edge' | 'signal';
}) {
  return (
    <section
      aria-label={legend}
      className={cn(
        'relative flex min-w-0 flex-col border border-edge-subtle bg-surface-1',
        className,
      )}
    >
      <Corners tone={tone} />
      <header className="flex h-7 shrink-0 items-center gap-2 border-b border-edge-subtle px-2.5">
        <h2 className="td-legend truncate text-text-secondary">{legend}</h2>
        <span aria-hidden className="td-rule" />
        {actions}
      </header>
      <div className={cn('min-w-0 flex-1 p-3', bodyClassName)}>{children}</div>
      {footer ? (
        <footer className="shrink-0 border-t border-edge-subtle px-2.5 py-1.5">{footer}</footer>
      ) : null}
    </section>
  );
}

export type ReadoutSize = 'sm' | 'md' | 'lg';

const VALUE_SIZE: Record<ReadoutSize, string> = {
  sm: 'text-xs',
  md: 'text-base font-medium',
  lg: 'text-xl font-medium',
};

/** One measured quantity. Legend above in letterspaced caps, the number in
 * tabular mono, the unit set small and quiet on the same baseline. */
export function Readout({
  label,
  value,
  unit,
  note,
  size = 'md',
  align = 'left',
  className,
}: {
  label: string;
  value: ReactNode;
  unit?: string | undefined;
  note?: ReactNode;
  size?: ReadoutSize;
  align?: 'left' | 'right';
  className?: string;
}) {
  return (
    <div
      className={cn(
        'flex min-w-0 flex-col gap-1.5',
        align === 'right' && 'items-end text-right',
        className,
      )}
    >
      <span className="td-legend truncate">{label}</span>
      <span className="flex min-w-0 items-baseline gap-1">
        <span
          className={cn('td-value truncate leading-none', VALUE_SIZE[size])}
          data-cell="numeric"
        >
          {value}
        </span>
        {unit ? <span className="td-unit shrink-0 leading-none">{unit}</span> : null}
      </span>
      {note ? <span className="truncate text-3xs text-text-muted">{note}</span> : null}
    </div>
  );
}

export interface ReadoutItem {
  label: string;
  value: ReactNode;
  unit?: string | undefined;
  note?: ReactNode;
}

/** A ruled row of readouts divided by hairlines — the instrument's answer to a
 * row of stat cards. Cells share one bezel instead of each owning a box. */
export function ReadoutBar({
  items,
  size = 'md',
  className,
  label,
}: {
  items: readonly ReadoutItem[];
  size?: ReadoutSize;
  className?: string;
  label?: string;
}) {
  if (items.length === 0) return null;
  return (
    <div
      aria-label={label}
      className={cn(
        'relative flex flex-wrap border-y border-edge-subtle bg-surface-1',
        className,
      )}
    >
      {items.map((item) => (
        <div
          key={item.label}
          className="min-w-0 flex-1 basis-32 border-l border-edge-subtle px-3 py-2.5 first:border-l-0"
        >
          <Readout {...item} size={size} />
        </div>
      ))}
    </div>
  );
}

/** The one element allowed to signal liveness with colour alone — and it never
 * does so alone: a lamp always sits beside its own label. `live` adds the slow
 * flash, which `prefers-reduced-motion` pins fully lit. */
export function Lamp({
  tone,
  live,
  className,
}: {
  tone: string;
  live?: boolean;
  className?: string;
}) {
  return (
    <span
      aria-hidden
      className={cn('size-1.5 shrink-0', tone, live && 'td-signal', className)}
    />
  );
}

/** Workspace header: channel number, name, fill rule, and a quiet annotation.
 * Every one of the twelve surfaces opens with this exact geometry. */
export function WorkspaceHeader({
  path,
  title,
  note,
  actions,
}: {
  path: string;
  title: string;
  note?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="flex h-9 shrink-0 items-center gap-3 border-b border-edge-subtle bg-surface-1 px-3">
      <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
        {channelNumber(path)}
      </span>
      <h1 className="shrink-0 text-2xs font-semibold uppercase tracking-[0.2em] text-text-primary">
        {title}
      </h1>
      <span aria-hidden className="td-rule" />
      {note ? (
        <span className="min-w-0 truncate text-3xs tracking-[0.04em] text-text-muted">
          {note}
        </span>
      ) : null}
      {actions}
    </header>
  );
}

/** A proportional bar rendered as a graduated gauge: hairline track, ticked
 * scale, filled to the measured fraction. Renders nothing but the track when
 * the fraction is unknown — an empty gauge is honest, a guessed one is not. */
export function Gauge({
  fraction,
  className,
  ariaLabel,
  tone = 'bg-accent',
}: {
  fraction: number | null;
  className?: string;
  ariaLabel: string;
  tone?: string;
}) {
  const clamped = fraction == null ? null : Math.max(0, Math.min(1, fraction));
  return (
    <div
      className={cn('relative h-2 w-full border border-edge-subtle bg-surface-0', className)}
      role="img"
      aria-label={ariaLabel}
    >
      {clamped != null ? (
        <div
          className={cn('absolute inset-y-0 left-0', tone)}
          style={{ width: `${clamped * 100}%` }}
        />
      ) : null}
      <span
        aria-hidden
        className="absolute inset-0 flex justify-between opacity-70"
        style={{
          backgroundImage:
            'repeating-linear-gradient(to right, var(--raw-edge-strong) 0 1px, transparent 1px 25%)',
        }}
      />
    </div>
  );
}
