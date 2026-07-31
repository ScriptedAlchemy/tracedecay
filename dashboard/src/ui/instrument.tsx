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
  elevation = 'face',
}: {
  legend: string;
  actions?: ReactNode;
  footer?: ReactNode;
  children: ReactNode;
  className?: string;
  bodyClassName?: string;
  tone?: 'edge' | 'signal';
  /** Which plane of the chassis the panel body occupies. `well` recesses the
   * body so the region reads as something you look into — use it for lists,
   * logs and canvases, not for prose. */
  elevation?: 'face' | 'well';
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
      <header className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <h2 className="td-title truncate">{legend}</h2>
        <span aria-hidden className="td-rule" />
        {actions}
      </header>
      <div
        className={cn(
          'min-w-0 flex-1 p-3',
          elevation === 'well' && 'td-well',
          bodyClassName,
        )}
      >
        {children}
      </div>
      {footer ? (
        <footer className="shrink-0 border-t border-edge-subtle px-2.5 py-1.5">{footer}</footer>
      ) : null}
    </section>
  );
}

export type ReadoutSize = 'sm' | 'md' | 'lg' | 'xl' | 'display';

/** `sm`–`lg` stay on the text scale for cells that sit inside prose rhythm.
 * `xl` and `display` switch to `.td-display`, which retunes tracking and
 * weight for large monospaced figures — the two tiers are visually different
 * kinds of object, not just different sizes of the same one. */
const VALUE_SIZE: Record<ReadoutSize, string> = {
  sm: 'td-value text-xs',
  md: 'td-value text-base font-medium',
  lg: 'td-value text-xl font-medium',
  xl: 'td-display text-2xl',
  display: 'td-display text-3xl',
};

/** One measured quantity. Legend above in letterspaced caps, the number in
 * tabular mono, the unit set small and quiet on the same baseline.
 *
 * `fraction` adds the magnitude rail: the same number expressed a second time
 * as a length, so a column of readouts can be ranked without reading any
 * digits. It is decorative reinforcement of a value that is already printed
 * beside it, so it stays out of the accessibility tree. */
export function Readout({
  label,
  value,
  unit,
  note,
  fraction,
  size = 'md',
  align = 'left',
  className,
}: {
  label: string;
  value: ReactNode;
  unit?: string | undefined;
  note?: ReactNode;
  fraction?: number | null | undefined;
  size?: ReadoutSize;
  align?: 'left' | 'right';
  className?: string;
}) {
  const large = size === 'xl' || size === 'display';
  const rail =
    fraction == null || !Number.isFinite(fraction)
      ? null
      : Math.max(0, Math.min(1, fraction));
  return (
    <div
      className={cn(
        'flex min-w-0 flex-col',
        large ? 'gap-2' : 'gap-1.5',
        align === 'right' && 'items-end text-right',
        className,
      )}
    >
      <span className="td-legend truncate">{label}</span>
      <span className="flex min-w-0 items-baseline gap-1">
        <span className={cn('truncate', VALUE_SIZE[size])} data-cell="numeric">
          {value}
        </span>
        {unit ? (
          <span
            className={cn('td-unit shrink-0 leading-none', large && 'text-2xs')}
          >
            {unit}
          </span>
        ) : null}
      </span>
      {rail != null ? (
        <span aria-hidden className="td-meter h-px w-full">
          <span className="td-meter-fill" style={{ width: `${rail * 100}%` }} />
        </span>
      ) : null}
      {note ? <span className="truncate text-3xs text-text-muted">{note}</span> : null}
    </div>
  );
}

type MeterHeight = 'standard' | 'hairline' | 'row';

const METER_HEIGHT: Record<MeterHeight, string> = {
  standard: 'h-1',
  hairline: 'h-px',
  row: 'h-[3px]',
};

/** The row-scale magnitude rail: a quantity given a length so a column of them
 * reads as a distribution. Pass `ariaLabel` only when the number is NOT also
 * printed beside the meter; when it is, the meter is redundant to a screen
 * reader and stays hidden. */
export function Meter({
  fraction,
  className,
  tone,
  align = 'left',
  ariaLabel,
  height = 'standard',
}: {
  fraction: number | null | undefined;
  className?: string;
  /** Utility class for the fill, when the bar carries a state hue. */
  tone?: string;
  /** Which edge the fill grows from. A meter under a right-aligned figure has
   * to grow leftward, or the number and its own bar share no edge and the
   * column reads as two unrelated ragged things instead of one measurement. */
  align?: 'left' | 'right';
  ariaLabel?: string;
  /**
   * `standard` is the 4px measurement bar; `hairline` is the 1px rule dense
   * lists need, where a 4px bar per row reads as a chart rather than as an
   * annotation; `row` is the 3px rail in between, for a ruled list of
   * distribution rows (see `MeterRow`).
   *
   * This is a prop rather than a `className` override because `cn` is a plain
   * class joiner with no conflict resolution: passing `h-px` would leave both
   * `h-1` and `h-px` on the element and let stylesheet order pick the winner,
   * which is not something a call site can depend on. Track colour, by
   * contrast, is safely overridable — `td-meter` lives in `@layer components`,
   * so a `bg-*` utility from the caller reliably wins.
   */
  height?: MeterHeight;
}) {
  const clamped =
    fraction == null || !Number.isFinite(fraction)
      ? null
      : Math.max(0, Math.min(1, fraction));
  const a11y = ariaLabel
    ? ({ role: 'img', 'aria-label': ariaLabel } as const)
    : ({ 'aria-hidden': true } as const);
  return (
    <span {...a11y} className={cn('td-meter', METER_HEIGHT[height], className)}>
      {clamped != null ? (
        <span
          className={cn('td-meter-fill', align === 'right' && 'left-auto right-0', tone)}
          style={{ width: `${clamped * 100}%` }}
        />
      ) : null}
    </span>
  );
}

/** One row of a distribution: a name, the same quantity given a length, and the
 * figure right-aligned in its own column so a stack of rows reads as one ruled
 * table rather than as ragged sentences.
 *
 * The rail hides itself below `sm`, where 80px of bar would take the width the
 * name needs. `fraction: null` draws the track and no fill — a row whose share
 * of the whole is unknown must not borrow a length. The meter stays out of the
 * accessibility tree because the figure beside it is the same number. */
export function MeterRow({
  label,
  value,
  fraction,
  tone,
  title,
  leading,
  figureWidth = 'standard',
  className,
}: {
  label: ReactNode;
  value: ReactNode;
  fraction: number | null;
  /** Utility class for the fill, when the row carries a state hue. */
  tone?: string;
  /** Hover text for the label, which the row truncates. */
  title?: string;
  /** A narrow column ahead of the label, for a row that is filed under
   * something (a kind, a source) as well as named. */
  leading?: ReactNode;
  /** `wide` widens the figure column by half a rem, for values that carry a
   * unit suffix and would otherwise wrap the row. */
  figureWidth?: 'standard' | 'wide';
  className?: string;
}) {
  return (
    <div className={cn('flex items-center gap-2 text-xs', className)}>
      {leading}
      <span className="min-w-0 flex-1 truncate text-text-primary" title={title}>
        {label}
      </span>
      <Meter
        fraction={fraction}
        tone={tone}
        height="row"
        className="w-20 shrink-0 max-sm:hidden"
      />
      <span
        className={cn(
          'td-value shrink-0 text-right text-2xs text-text-secondary',
          figureWidth === 'wide' ? 'w-14' : 'w-12',
        )}
        data-cell="numeric"
      >
        {value}
      </span>
    </div>
  );
}

/** The right-hand end of a dense list row: a figure with its quiet unit, and
 * directly under it the same quantity as a length.
 *
 * The rail is right-aligned and grows leftward so the number and its bar share
 * the row's trailing edge — a left-growing bar under a right-set figure reads
 * as two unrelated ragged things. `fraction: null` draws the track and no fill,
 * because a row whose share of the whole is unknown must not borrow a length.
 * The meter stays out of the accessibility tree: the figure above it is the
 * same number. */
export function FigureRail({
  value,
  unit,
  fraction,
  width = 'standard',
  tone,
  className,
}: {
  value: ReactNode;
  unit?: string | undefined;
  fraction: number | null;
  /** `wide` gives a full extra rem to figures whose digits plus unit would
   * otherwise wrap the row. */
  width?: 'standard' | 'wide';
  /** Utility class for the fill, when the rail carries a state hue. */
  tone?: string;
  className?: string;
}) {
  return (
    <span
      className={cn(
        'flex shrink-0 flex-col items-end gap-1',
        width === 'wide' ? 'w-24' : 'w-20',
        className,
      )}
    >
      <span className="td-value text-2xs leading-none text-text-secondary" data-cell="numeric">
        {value}
        {unit ? <span className="td-unit ml-1">{unit}</span> : null}
      </span>
      <Meter fraction={fraction} height="row" className="w-full" align="right" tone={tone} />
    </span>
  );
}

/** One labelled term in a definition grid: the label engraved above in caps,
 * the value below, wrapping rather than truncating. Used for the horizon lines
 * where the value is a scope reference or a pair of timestamps — strings a
 * reader has to be able to read in full, not scan. */
export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="uppercase tracking-[0.08em] text-text-muted">{label}</dt>
      <dd className="min-w-0 break-words text-text-secondary tabular">{children}</dd>
    </div>
  );
}

/** A definition-list term for a single short string of provenance — a ref, an
 * id, a stamp. Tighter than `Field` and truncating rather than wrapping,
 * because these sit in a grid under a record where the label carries the
 * meaning and the full value is on the record itself. `muted` files the term as
 * secondary detail without giving it a second layout. */
export function Fact({
  label,
  value,
  muted,
}: {
  label: string;
  value: string;
  muted?: boolean;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend">{label}</dt>
      <dd
        className={
          muted
            ? 'truncate text-3xs text-text-muted'
            : 'truncate text-3xs text-text-secondary'
        }
      >
        {value}
      </dd>
    </div>
  );
}

export interface ReadoutItem {
  label: string;
  value: ReactNode;
  unit?: string | undefined;
  note?: ReactNode;
  /** 0–1 magnitude for the readout's rail; omit when the quantity has no
   * meaningful ceiling to be measured against. */
  fraction?: number | null | undefined;
}

/** A ruled row of readouts divided by hairlines — the instrument's answer to a
 * row of stat cards. Cells share one bezel instead of each owning a box.
 *
 * At `elevation="raised"` the bar becomes the surface's headline: it lifts off
 * the face on the standard one-highlight-one-shadow recipe, so the eye lands
 * there first instead of scanning a uniform grid for somewhere to start. */
export function ReadoutBar({
  items,
  size = 'md',
  className,
  label,
  elevation = 'face',
}: {
  items: readonly ReadoutItem[];
  size?: ReadoutSize;
  className?: string;
  label?: string;
  elevation?: 'face' | 'raised';
}) {
  if (items.length === 0) return null;
  const large = size === 'xl' || size === 'display';
  return (
    <div
      aria-label={label}
      className={cn(
        'relative flex flex-wrap border-y border-edge-subtle',
        elevation === 'raised' ? 'td-raised' : 'bg-surface-1',
        className,
      )}
    >
      {items.map((item) => (
        <div
          key={item.label}
          className={cn(
            'min-w-0 flex-1 border-l border-edge-subtle px-3 first:border-l-0',
            large ? 'basis-44 py-3.5' : 'basis-32 py-2.5',
          )}
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
    // The gate's handle on this one line. Every workspace's header is this
    // element, so the clipping assertion in `e2e/responsive.ts` can hold all of
    // them to the same invariant — nothing placed here may render outside this
    // element's padding box — without each surface growing a marker of its own.
    // `min-h-9` rather than `h-9`, and wrapping: at the widths where the line
    // fits, both render exactly as the fixed height did. Where it does not
    // fit, a fixed height had no way to be honest — the overflowing child was
    // painted outside the box and, on `/settings`, 276px of it off-screen. A
    // header that grows keeps the content it was given.
    <header
      data-workspace-header
      className="flex min-h-9 shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-edge-subtle bg-surface-1 py-1 px-3"
    >
      <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
        {channelNumber(path)}
      </span>
      <h1 className="shrink-0 text-2xs font-semibold uppercase tracking-[0.2em] text-text-primary">
        {title}
      </h1>
      {/* Withdrawn below `sm`, where the line has no width to spend on filler.
       * Decorative and `aria-hidden`, this hairline was nonetheless the whole
       * overflow at 320 CSS px: its 8px box plus one 10.5px gap took a header
       * offering 254px of content box to 273px of children, pushing the state
       * chip 19px outside that box with its label flush against the screen
       * edge. Above `sm` the rule earns its width, so wide layout is unchanged. */}
      <span aria-hidden className="td-rule max-sm:hidden" />
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
