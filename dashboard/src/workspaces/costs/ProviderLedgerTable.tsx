import { useRef, type KeyboardEvent, type ReactNode } from 'react';
import { cn } from '../../ui/cn.ts';
import { formatCount } from '../../ui/format.ts';
import {
  formatShare,
  formatUsd,
  pricingClassLabel,
  sumReportedTokens,
  type ProviderLedger,
  type ProviderRow,
} from './attribution.ts';

/**
 * Provider spend detail: the exact accounting behind the field, one row per
 * provider, and a total row that says what it includes.
 *
 * Rows inspect on hover and focus and scope on Enter, the same contract as
 * the legend, so the two views are one selection seen twice. The total is a
 * priced total: it sums only the dollars the authority could price, and the
 * coverage column under it says how much of the observed usage that is.
 */
export function ProviderLedgerTable({
  ledger,
  inspected,
  selected,
  canonicalTotalUsd,
  onInspect,
  onSelect,
}: {
  ledger: ProviderLedger;
  inspected: string | null;
  selected: string | null;
  /** The all-time complete total the overview serves, when the range is the
   * whole ledger and every event priced; null otherwise. */
  canonicalTotalUsd: number | null;
  onInspect: (provider: string | null) => void;
  onSelect: (provider: string | null) => void;
}) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);
  const rows = ledger.rows;
  const activeIndex = Math.max(
    0,
    rows.findIndex((row) => row.provider === selected),
  );
  const tokens = sumReportedTokens(rows);
  const emphasised = inspected ?? selected;

  const focusRow = (index: number) => {
    const count = rows.length;
    if (count === 0) return;
    buttons.current[((index % count) + count) % count]?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        focusRow(index + 1);
        break;
      case 'ArrowUp':
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
    <div className="flex min-w-0 flex-col gap-2">
      <div
        className="min-w-0 overflow-x-auto"
        role="region"
        aria-label="Provider spend detail table"
        tabIndex={0}
        onMouseLeave={() => onInspect(null)}
      >
        <table className="w-full min-w-[720px] border-collapse text-sm" data-provider-ledger>
          <thead>
            <tr className="border-b border-edge-subtle">
              <Th align="left">provider</Th>
              <Th align="left">pricing</Th>
              <Th>priced spend</Th>
              <Th>share</Th>
              <Th>events</Th>
              <Th>priced ev.</Th>
              <Th>input</Th>
              <Th>output</Th>
              <Th>cache read</Th>
              <Th>total tokens</Th>
              <Th>sessions</Th>
              <Th>models</Th>
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 ? (
              <tr>
                <td colSpan={12} className="px-2 py-3 text-center text-text-muted">
                  no provider recorded usage in this range
                </td>
              </tr>
            ) : (
              rows.map((row, index) => (
                <LedgerRow
                  key={row.provider}
                  ref={(node) => {
                    buttons.current[index] = node;
                  }}
                  row={row}
                  tabIndex={index === activeIndex ? 0 : -1}
                  inspected={inspected === row.provider}
                  selected={selected === row.provider}
                  dimmed={emphasised !== null && emphasised !== row.provider}
                  onInspect={onInspect}
                  onSelect={onSelect}
                  onKeyDown={(event) => onKeyDown(event, index)}
                />
              ))
            )}
          </tbody>
          <tfoot>
            <tr className="border-t border-edge-strong bg-surface-2">
              <th scope="row" className="td-legend px-2 py-2 text-left font-medium">
                total
              </th>
              <td className="td-legend px-2 py-2 text-left text-text-muted">
                {ledger.complete ? 'priced' : ledger.pricedEvents > 0 ? 'partially priced' : 'unpriced'}
              </td>
              <Td strong>{formatUsd(ledger.pricedTotalUsd)}</Td>
              <Td>{ledger.pricedTotalUsd === null ? '—' : '100%'}</Td>
              <Td>{ledger.usageEvents.toLocaleString()}</Td>
              <Td>{ledger.pricedEvents.toLocaleString()}</Td>
              <Td>{formatCount(sumField(rows, 'input_tokens'), 1_000)}</Td>
              <Td>{formatCount(sumField(rows, 'output_tokens'), 1_000)}</Td>
              <Td>{formatCount(sumField(rows, 'cache_read_tokens'), 1_000)}</Td>
              <Td>{formatCount(tokens.tokens, 1_000)}</Td>
              <Td>—</Td>
              <Td>{rows.reduce((sum, row) => sum + row.models, 0).toLocaleString()}</Td>
            </tr>
          </tfoot>
        </table>
      </div>
      <p className="text-sm leading-relaxed text-text-muted" data-ledger-coverage>
        {ledger.usageEvents === 0
          ? 'The total has no denominator: no usage events were observed in this range.'
          : ledger.complete
            ? `The total covers every one of ${ledger.usageEvents.toLocaleString()} observed usage events (${formatShare(ledger.coverage)} pricing coverage).`
            : `The total includes ${ledger.pricedEvents.toLocaleString()} of ${ledger.usageEvents.toLocaleString()} observed usage events (${formatShare(ledger.coverage)} pricing coverage); the other ${ledger.unpricedEvents.toLocaleString()} are unpriced and contribute no dollars.`}
        {tokens.unreported > 0
          ? ` ${tokens.unreported.toLocaleString()} of ${rows.length.toLocaleString()} providers reported no token total, so the token columns sum the other ${tokens.reported.toLocaleString()}.`
          : ''}
        {canonicalTotalUsd !== null
          ? ` The canonical all-time projection agrees: ${formatUsd(canonicalTotalUsd)}.`
          : ''}
        {' '}Sessions are distinct per provider and are not summed.
      </p>
    </div>
  );
}

function LedgerRow({
  ref,
  row,
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
  tabIndex: number;
  inspected: boolean;
  selected: boolean;
  dimmed: boolean;
  onInspect: (provider: string | null) => void;
  onSelect: (provider: string | null) => void;
  onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => void;
}) {
  return (
    <tr
      data-provider={row.provider}
      data-inspected={inspected ? 'true' : undefined}
      data-selected={selected ? 'true' : undefined}
      className={cn(
        'border-b border-edge-subtle transition-[background-color,opacity] duration-[var(--dur-state)]',
        inspected && 'td-raised',
        selected && 'bg-surface-2',
        dimmed && 'opacity-50',
      )}
      onMouseEnter={() => onInspect(row.provider)}
    >
      <th scope="row" className="relative p-0 text-left font-normal">
        <span
          aria-hidden
          className={cn('absolute inset-y-0 left-0 w-[3px]', selected ? 'bg-accent' : 'bg-transparent')}
        />
        <button
          ref={ref}
          type="button"
          tabIndex={tabIndex}
          aria-pressed={selected}
          className="flex min-h-[44px] w-full items-center px-3 text-left text-xs text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
          onFocus={() => onInspect(row.provider)}
          onBlur={() => onInspect(null)}
          onClick={() => onSelect(selected ? null : row.provider)}
          onKeyDown={onKeyDown}
        >
          {row.provider}
        </button>
      </th>
      <td className="td-legend px-2 py-2 text-left text-text-muted">{pricingClassLabel(row.pricing)}</td>
      <Td strong>{formatUsd(row.pricedCostUsd)}</Td>
      <Td>{formatShare(row.share)}</Td>
      <Td>{row.usageEvents.toLocaleString()}</Td>
      <Td>{row.pricedEvents.toLocaleString()}</Td>
      <Td>{formatCount(row.actual?.input_tokens, 1_000)}</Td>
      <Td>{formatCount(row.actual?.output_tokens, 1_000)}</Td>
      <Td>{formatCount(row.actual?.cache_read_tokens, 1_000)}</Td>
      <Td>{formatCount(row.totalTokens, 1_000)}</Td>
      <Td>{row.sessions.toLocaleString()}</Td>
      <Td>
        {row.models.toLocaleString()}
        {row.unpricedModels > 0 ? (
          <span className="text-text-muted"> · {row.unpricedModels.toLocaleString()} unpriced</span>
        ) : null}
      </Td>
    </tr>
  );
}

function Th({ children, align = 'right' }: { children: string; align?: 'left' | 'right' }) {
  return (
    <th
      scope="col"
      className={cn('td-legend px-2 py-2 font-medium', align === 'left' ? 'text-left' : 'text-right')}
    >
      {children}
    </th>
  );
}

function Td({ children, strong }: { children: ReactNode; strong?: boolean }) {
  return (
    <td
      className={cn('td-value px-2 py-2 text-right', strong ? 'text-text-primary' : 'text-text-secondary')}
      data-cell="numeric"
    >
      {children}
    </td>
  );
}

/** A token class summed across providers that reported it; null when none did. */
function sumField(
  rows: readonly ProviderRow[],
  field: 'input_tokens' | 'output_tokens' | 'cache_read_tokens' | 'cache_write_tokens',
): number | null {
  let total: number | null = null;
  for (const row of rows) {
    const value = row.actual?.[field];
    if (value == null) continue;
    total = (total ?? 0) + value;
  }
  return total;
}
