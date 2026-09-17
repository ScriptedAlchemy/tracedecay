import { useMemo, useRef, type KeyboardEvent } from 'react';
import type { EChartsOption } from 'echarts';
import { Chart } from '../../viz/chart/Chart.tsx';
import { seriesLineStyle, seriesSwatchClass } from '../../viz/chart/series.ts';
import { cn } from '../../ui/cn.ts';
import { Corners } from '../../ui/instrument.tsx';
import {
  formatShare,
  formatUsd,
  formatUtcDay,
  pricingClassLabel,
  type ProviderLedger,
  type ProviderRow,
  type SpendSeries,
} from './attribution.ts';

/**
 * The spend field: one dated line per provider, priced dollars per UTC day,
 * beside a legend that is also the provider control.
 *
 * Hover and focus inspect — the other lines dim, the inspected provider is
 * reported to the inspector — and never change scope. Enter or Space on a
 * legend row scopes the query to that provider; Escape clears the scope. The
 * exact series stays available as a table under the field, so nothing the
 * canvas draws is canvas-only.
 */
export function ProviderSpendField({
  ledger,
  series,
  rangeNote,
  inspected,
  selected,
  onInspect,
  onSelect,
}: {
  ledger: ProviderLedger;
  series: SpendSeries | null;
  rangeNote: string;
  inspected: string | null;
  selected: string | null;
  onInspect: (provider: string | null) => void;
  onSelect: (provider: string | null) => void;
}) {
  const emphasised = inspected ?? selected;
  const option = useMemo<EChartsOption | null>(() => {
    if (!series) return null;
    return {
      grid: { left: 8, right: 28, top: 18, bottom: 6, containLabel: true },
      xAxis: {
        type: 'category',
        data: series.days.map(formatUtcDay),
        boundaryGap: false,
        axisTick: { show: false },
        axisLabel: { hideOverlap: true },
      },
      yAxis: {
        type: 'value',
        axisLabel: {
          formatter: (value: number) => `$${value.toLocaleString(undefined, { maximumFractionDigits: 0 })}`,
        },
      },
      tooltip: {
        trigger: 'axis',
        valueFormatter: (value) =>
          typeof value === 'number' ? formatUsd(value) : 'unpriced',
      },
      series: series.series.map((entry, index) => {
        const dimmed = emphasised !== null && emphasised !== entry.provider;
        return {
          type: 'line',
          name: entry.provider,
          // Gaps stay gaps: a day the provider had nothing priced is not a
          // day it spent nothing, so the line breaks rather than touching zero.
          connectNulls: false,
          showSymbol: true,
          symbolSize: 4,
          data: entry.points.map((point) => point?.pricedCostUsd ?? null),
          lineStyle: {
            width: dimmed ? 1 : emphasised === entry.provider ? 2 : 1.5,
            type: seriesLineStyle(index),
            opacity: dimmed ? 0.25 : 1,
          },
          itemStyle: { opacity: dimmed ? 0.25 : 1 },
          emphasis: { focus: 'series' },
        };
      }),
    };
  }, [series, emphasised]);

  const first = series?.days[0];
  const last = series?.days.at(-1);
  const drawn = series?.series.length ?? 0;
  const describe =
    series && first !== undefined && last !== undefined
      ? `Priced provider spend per UTC day, ${formatUtcDay(first)} to ${formatUtcDay(last)}, ${drawn} provider series over ${series.days.length} day buckets`
      : 'No dated priced spend to draw';

  return (
    // Container-queried, not viewport-queried: the legend sits beside the
    // field only when this panel is wide enough to give the chart room, which
    // depends on the grid column it landed in, not on the window. The query
    // lives on a wrapper because an element cannot query its own size.
    <div className="@container min-w-0">
      <div className="flex min-w-0 flex-col gap-3 @3xl:flex-row">
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
            <span className="td-legend">priced spend · USD per UTC day</span>
            <span
              className="td-value flex flex-wrap items-baseline gap-x-1.5 text-sm text-text-primary"
              data-cell="numeric"
              data-priced-total
            >
              {formatUsd(ledger.pricedTotalUsd)}
              <span className="td-unit">
                {ledger.usageEvents === 0
                  ? 'no usage in range'
                  : ledger.complete
                    ? 'priced total · complete'
                    : `priced total · ${formatShare(ledger.coverage)} coverage`}
              </span>
            </span>
          </div>
          <span className="text-3xs text-text-muted">{rangeNote}</span>
          {option ? (
            <Chart option={option} height={280} ariaLabel={describe} />
          ) : (
            <div
              className="td-optic td-graticule flex min-h-[280px] items-center justify-center px-4 text-center text-2xs leading-relaxed text-text-secondary"
              role="img"
              aria-label={describe}
            >
              {ledger.usageEvents === 0
                ? 'no usage observations in this range — an empty series, not a zero bill'
                : 'usage was observed but none of it carries a timestamp, so there is no dated series to draw'}
            </div>
          )}
          <SeriesDisclosure ledger={ledger} series={series} />
        </div>
        <ProviderLegend
          ledger={ledger}
          inspected={inspected}
          selected={selected}
          onInspect={onInspect}
          onSelect={onSelect}
        />
      </div>
    </div>
  );
}

/** What the drawn dollars do and do not include, and the exact values as text. */
function SeriesDisclosure({
  ledger,
  series,
}: {
  ledger: ProviderLedger;
  series: SpendSeries | null;
}) {
  const notes: string[] = [];
  if (series) {
    notes.push(`${series.days.length.toLocaleString()} UTC day buckets, aggregated by the daemon`);
    if (series.unpricedBuckets > 0) {
      notes.push(
        `${series.unpricedBuckets.toLocaleString()} provider-day buckets carry unpriced usage` +
          (series.partialBuckets > 0
            ? ` (${series.partialBuckets.toLocaleString()} of them are drawn from their priced part only)`
            : ' and are drawn as gaps'),
      );
    }
  }
  if (ledger.undatedEvents > 0) {
    notes.push(
      `${ledger.undatedEvents.toLocaleString()} usage events carry no timestamp and are attributed to providers but absent from the series`,
    );
  }
  return (
    <div className="flex flex-col gap-1.5 text-3xs leading-relaxed text-text-muted">
      {notes.length > 0 ? <p>{notes.join(' · ')}</p> : null}
      {series ? (
        <details className="group">
          <summary className="td-hit -ml-2 cursor-pointer select-none px-2 text-text-secondary underline-offset-2 hover:underline">
            series as table
          </summary>
          <div
            className="mt-1 max-h-64 overflow-auto border border-edge-subtle"
            role="region"
            aria-label="Priced spend series as a table"
            tabIndex={0}
          >
            <table className="w-full min-w-max border-collapse text-2xs">
              <thead className="sticky top-0 bg-surface-2">
                <tr>
                  <th scope="col" className="td-legend px-2 py-1.5 text-left font-medium">
                    UTC day
                  </th>
                  {series.series.map((entry) => (
                    <th
                      key={entry.provider}
                      scope="col"
                      className="td-legend px-2 py-1.5 text-right font-medium"
                    >
                      {entry.provider}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {series.days.map((day, position) => (
                  <tr key={day} className="border-t border-edge-subtle">
                    <th scope="row" className="td-value px-2 py-1 text-left font-normal">
                      {formatUtcDay(day)}
                    </th>
                    {series.series.map((entry) => {
                      const point = entry.points[position] ?? null;
                      return (
                        <td
                          key={entry.provider}
                          className="td-value px-2 py-1 text-right"
                          data-cell="numeric"
                        >
                          {point === null
                            ? 'no usage'
                            : point.pricedCostUsd === null
                              ? `unpriced · ${point.usageEvents.toLocaleString()} ev`
                              : point.unpricedEvents > 0
                                ? `${formatUsd(point.pricedCostUsd)} · ${point.unpricedEvents.toLocaleString()} unpriced`
                                : formatUsd(point.pricedCostUsd)}
                        </td>
                      );
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </details>
      ) : null}
    </div>
  );
}

/**
 * The provider legend, doubling as the scope control. One button per
 * provider with a roving tabindex: Tab lands on the scoped (or first) row,
 * arrows move, Enter/Space scopes, Escape clears. Hover and focus inspect.
 */
export function ProviderLegend({
  ledger,
  inspected,
  selected,
  onInspect,
  onSelect,
}: {
  ledger: ProviderLedger;
  inspected: string | null;
  selected: string | null;
  onInspect: (provider: string | null) => void;
  onSelect: (provider: string | null) => void;
}) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);
  const rows = ledger.rows;
  const activeIndex = Math.max(
    0,
    rows.findIndex((row) => row.provider === selected),
  );

  const focusRow = (index: number) => {
    const count = rows.length;
    if (count === 0) return;
    const target = ((index % count) + count) % count;
    buttons.current[target]?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowRight':
        event.preventDefault();
        focusRow(index + 1);
        break;
      case 'ArrowUp':
      case 'ArrowLeft':
        event.preventDefault();
        focusRow(index - 1);
        break;
      case 'Home':
        event.preventDefault();
        focusRow(0);
        break;
      case 'End':
        event.preventDefault();
        focusRow(rows.length - 1);
        break;
      case 'Escape':
        if (selected !== null) {
          event.preventDefault();
          onSelect(null);
        }
        break;
      default:
        break;
    }
  };

  return (
    <div
      role="group"
      aria-label="Provider legend and scope"
      className="relative flex w-full shrink-0 flex-col border border-edge-subtle bg-surface-1 @3xl:w-64"
      onMouseLeave={() => onInspect(null)}
    >
      <Corners />
      <div className="flex h-8 items-center gap-2 border-b border-edge-subtle px-2.5">
        <span className="td-legend">providers</span>
        <span aria-hidden className="td-rule" />
        <span className="td-value text-3xs text-text-muted" data-cell="numeric">
          {rows.length.toLocaleString()}
        </span>
      </div>
      {rows.length === 0 ? (
        <p className="px-2.5 py-3 text-2xs text-text-muted">
          no provider recorded usage in this range
        </p>
      ) : (
        <ul className="flex flex-col">
          {rows.map((row, index) => (
            <li key={row.provider}>
              <LegendRow
                ref={(node) => {
                  buttons.current[index] = node;
                }}
                row={row}
                index={index}
                tabIndex={index === activeIndex ? 0 : -1}
                inspected={inspected === row.provider}
                selected={selected === row.provider}
                dimmed={
                  (inspected ?? selected) !== null && (inspected ?? selected) !== row.provider
                }
                onInspect={onInspect}
                onSelect={onSelect}
                onKeyDown={(event) => onKeyDown(event, index)}
              />
            </li>
          ))}
        </ul>
      )}
      <p className="border-t border-edge-subtle px-2.5 py-1.5 text-3xs leading-relaxed text-text-muted">
        {selected === null
          ? 'hover or focus inspects · Enter scopes · Escape clears'
          : `scoped to ${selected} · Escape clears`}
      </p>
    </div>
  );
}

function LegendRow({
  ref,
  row,
  index,
  tabIndex,
  inspected,
  selected,
  dimmed,
  onInspect,
  onSelect,
  onKeyDown,
}: {
  ref: (node: HTMLButtonElement | null) => void;
  row: ProviderRow;
  index: number;
  tabIndex: number;
  inspected: boolean;
  selected: boolean;
  dimmed: boolean;
  onInspect: (provider: string | null) => void;
  onSelect: (provider: string | null) => void;
  onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => void;
}) {
  const lineStyle = seriesLineStyle(index);
  return (
    <button
      ref={ref}
      type="button"
      tabIndex={tabIndex}
      aria-pressed={selected}
      aria-label={`${row.provider}, ${pricingClassLabel(row.pricing)}, ${
        row.pricedCostUsd === null ? 'no priced spend' : formatUsd(row.pricedCostUsd)
      }${row.share === null ? '' : `, ${formatShare(row.share)} of priced total`}`}
      data-provider={row.provider}
      data-inspected={inspected ? 'true' : undefined}
      data-selected={selected ? 'true' : undefined}
      className={cn(
        'relative flex min-h-[44px] w-full items-center gap-2.5 border-b border-edge-subtle px-2.5 text-left transition-[background-color,opacity] duration-[var(--dur-state)]',
        'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
        inspected && 'td-raised',
        selected && 'bg-surface-2',
        dimmed && 'opacity-50',
      )}
      onMouseEnter={() => onInspect(row.provider)}
      onFocus={() => onInspect(row.provider)}
      onBlur={() => onInspect(null)}
      onClick={() => onSelect(selected ? null : row.provider)}
      onKeyDown={onKeyDown}
    >
      {/* Persistent selection is a position bar, identifiable without glow. */}
      <span
        aria-hidden
        className={cn('absolute inset-y-0 left-0 w-[3px]', selected ? 'bg-accent' : 'bg-transparent')}
      />
      {/* The line's own mark: hue by palette index, style by line-style index,
       * so the swatch identifies the series without relying on colour alone. */}
      <span
        aria-hidden
        data-line-style={lineStyle}
        className={cn('h-[2px] w-5 shrink-0', seriesSwatchClass(index))}
        style={
          lineStyle === 'solid'
            ? undefined
            : {
                maskImage:
                  lineStyle === 'dashed'
                    ? 'repeating-linear-gradient(to right, black 0 5px, transparent 5px 8px)'
                    : 'repeating-linear-gradient(to right, black 0 2px, transparent 2px 4px)',
              }
        }
      />
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex items-baseline gap-2">
          <span className="min-w-0 truncate text-xs text-text-primary">{row.provider}</span>
          <span className="td-legend shrink-0 text-text-muted">{pricingClassLabel(row.pricing)}</span>
        </span>
        <span className="flex items-baseline gap-2 text-2xs">
          <span className="td-value text-text-secondary" data-cell="numeric">
            {formatUsd(row.pricedCostUsd)}
          </span>
          <span className="td-value text-text-muted" data-cell="numeric">
            {formatShare(row.share)}
          </span>
        </span>
      </span>
    </button>
  );
}
