import { useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import {
  SavingsModelsPayloadV1Schema,
  SavingsOverviewPayloadV1Schema,
  type DashboardEnvelopeV1,
  type SavingsLedgerSummaryV1,
  type SavingsModelsPayloadV1,
  type SavingsOverviewPayloadV1,
  type SavingsSumV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { usePayload } from '../../data/query/usePayload.ts';
import { cn } from '../../ui/cn.ts';
import { formatCount, splitCount } from '../../ui/format.ts';
import { Meter, Panel, Readout, WorkspaceHeader } from '../../ui/instrument.tsx';
import {
  ReadModelState,
  envelopeReadState,
  payloadReadState,
  type PayloadReadState,
  type ReadState,
} from '../../ui/ReadSection.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import {
  buildSpendSeries,
  formatShare,
  sumReportedTokens,
  summarizeProviderLedger,
  type ProviderLedger,
} from './attribution.ts';
import { CanonicalCosts } from './CanonicalCosts.tsx';
import { CostsInspector } from './CostsInspector.tsx';
import {
  COSTS_RANGES,
  costsRangeLabel,
  costsRangeNote,
  useCostsQuery,
  type CostsRange,
} from './costsQuery.ts';
import { PricingAuthority } from './PricingAuthority.tsx';
import { ProviderLedgerTable } from './ProviderLedgerTable.tsx';
import { ProviderSpendField } from './ProviderSpendField.tsx';
import { TopologyMetricsCosts } from './TopologyMetricsCosts.tsx';

const BASE = '/api/plugins/savings';

/**
 * Costs, channel 11: actual provider spend, attributed.
 *
 * Two independent reads compose the surface. `/overview` carries the all-time
 * usage totals, the savings ledger windows, the pricing authority's identity,
 * and the canonical cost projection; `/models?range=` carries the exact
 * provider usage of the selected range priced by that same authority, grouped
 * by provider, model, and UTC day. A failure in one must not blank the other,
 * so every panel resolves its own read and reports its own typed state.
 *
 * The one invariant every panel shares: no dollar figure is manufactured.
 * Priced, partially priced, unpriced, null-identity, undated, and unavailable
 * usage stay independently visible, and every total says what it includes.
 */
export function CostsPage() {
  const query = useCostsQuery();
  const [inspected, setInspected] = useState<string | null>(null);

  const overview = useEnvelope(
    ['savings', 'overview'],
    `${BASE}/overview`,
    SavingsOverviewPayloadV1Schema,
  );
  const models = usePayload(
    ['savings', 'models', query.range],
    `${BASE}/models?range=${query.range}`,
    SavingsModelsPayloadV1Schema,
  );

  const overviewRead: OverviewRead = envelopeReadState(overview.isPending, overview.data, {
    loading: 'reading the savings overview',
    transport: 'the savings overview could not be read',
  });
  const attribution = useMemo(
    () => resolveAttribution(payloadReadState(models.isPending, models.data)),
    [models.isPending, models.data],
  );
  const ledger = attribution.kind === 'ready' ? attribution.ledger : null;
  const series = useMemo(
    () =>
      attribution.kind === 'ready'
        ? buildSpendSeries(
            attribution.payload.provider_usage.by_provider_day,
            attribution.ledger.rows.map((row) => row.provider),
          )
        : null,
    [attribution],
  );

  // A scoped provider the current range no longer carries is still a real
  // query, the address says so, but nothing here can be scoped to it, so
  // the selection is shown as such rather than silently dropped.
  const selectedPresent =
    query.provider !== null && ledger !== null
      ? ledger.rows.some((row) => row.provider === query.provider)
      : query.provider !== null;

  const overviewPayload = overviewRead.kind === 'ready' ? overviewRead.value.payload : null;
  const canonicalTotalUsd =
    query.range === 'all' &&
    overviewPayload?.provider_usage.available === true &&
    overviewPayload.provider_usage.status === 'complete'
      ? overviewPayload.provider_usage.total_cost_usd
      : null;

  return (
    <div className="flex h-full flex-col" role="region" aria-label="Costs content">
      <WorkspaceHeader
        path="costs"
        title="Costs"
        note="actual provider spend · canonical pricing · usage facts independent of price"
      />
      <QueryRegister
        range={query.range}
        onRange={query.setRange}
        provider={query.provider}
        selectedPresent={selectedPresent}
        onClearProvider={() => query.setProvider(null)}
        states={registerStates(overviewRead, attribution)}
      />

      <div className="grid gap-2 p-2 md:grid-cols-2 xl:grid-cols-12">
        <Panel legend="Actual provider spend" className="md:col-span-2 xl:col-span-7">
          <AttributionBody attribution={attribution}>
            {(ready) => (
              <ProviderSpendField
                ledger={ready.ledger}
                series={series}
                rangeNote={costsRangeNote(query.range)}
                inspected={inspected}
                selected={query.provider}
                onInspect={setInspected}
                onSelect={query.setProvider}
              />
            )}
          </AttributionBody>
        </Panel>

        <Panel legend="Usage" className="xl:col-span-2">
          <UsageReadouts
            ledger={ledger}
            attribution={attribution}
            overview={overviewRead}
            range={query.range}
          />
        </Panel>

        <Panel legend="Provider pricing authority" className="xl:col-span-3">
          <OverviewBody read={overviewRead}>
            {(payload) => (
              <PricingAuthority
                pricing={payload.pricing}
                ledger={ledger}
                coverageStatus={
                  attribution.kind === 'ready'
                    ? attribution.payload.provider_usage_coverage
                    : attribution.kind === 'unavailable'
                      ? attribution.payload?.provider_usage_coverage ?? null
                      : null
                }
              />
            )}
          </OverviewBody>
        </Panel>

        <Panel
          legend="Provider spend detail · canonical pricing"
          className="md:col-span-2 xl:col-span-9"
          elevation="well"
          bodyClassName="p-0"
        >
          <AttributionBody attribution={attribution}>
            {(ready) => (
              <div className="flex flex-col gap-2 p-3">
                <ProviderLedgerTable
                  ledger={ready.ledger}
                  inspected={inspected}
                  selected={query.provider}
                  canonicalTotalUsd={canonicalTotalUsd}
                  onInspect={setInspected}
                  onSelect={query.setProvider}
                />
              </div>
            )}
          </AttributionBody>
        </Panel>

        <Panel legend="Inspector" className="md:col-span-2 xl:col-span-3">
          <AttributionBody attribution={attribution}>
            {(ready) => (
              <CostsInspector
                ledger={ready.ledger}
                byModel={ready.payload.provider_usage.by_model}
                inspected={inspected}
                selected={selectedPresent ? query.provider : null}
                pricingRevision={ready.payload.provider_usage.pricing_revision}
              />
            )}
          </AttributionBody>
        </Panel>
      </div>

      <CanonicalCosts />
      <TopologyMetricsCosts />
    </div>
  );
}

/* ------------------------------------------------------------------------ */
/* Attribution read                                                          */
/* ------------------------------------------------------------------------ */

type OverviewRead = ReadState<DashboardEnvelopeV1<SavingsOverviewPayloadV1>>;

type AttributionRead =
  | { kind: 'ready'; payload: SavingsModelsPayloadV1; ledger: ProviderLedger }
  | {
      kind: 'unavailable';
      state: DomainStateKind;
      detail: string;
      payload: SavingsModelsPayloadV1 | null;
    }
  | { kind: 'blocked'; state: DomainStateKind; detail: string | undefined };

/**
 * The `/models` payload resolved to what the attribution panels can render.
 *
 * Three ways it fails to yield a ledger, and they are told apart: the read
 * itself blocked (transport, refusal, schema); the payload arrived and says
 * the store is not mounted or the read failed; the payload arrived and the
 * provider-usage aggregate behind it could not serve exact deltas, the
 * session store is there, but its usage projection is partial or absent.
 */
function resolveAttribution(read: PayloadReadState<SavingsModelsPayloadV1>): AttributionRead {
  if (read.kind === 'blocked') {
    if (read.state === 'unavailable' && read.payload !== undefined) {
      return {
        kind: 'unavailable',
        state: read.payload.status === 'read_failed' ? 'error' : 'unavailable',
        detail: read.payload.error ?? read.detail ?? 'the daemon could not serve provider attribution',
        payload: read.payload,
      };
    }
    return { kind: 'blocked', state: read.state, detail: read.detail };
  }
  const payload = read.value;
  if (!payload.available) {
    return {
      kind: 'unavailable',
      state: payload.status === 'read_failed' ? 'error' : 'unavailable',
      detail:
        payload.error ??
        (payload.status === 'read_failed'
          ? 'the session store read failed'
          : 'the session store is not mounted for this scope'),
      payload,
    };
  }
  const ledger = summarizeProviderLedger(payload.provider_usage);
  if (ledger === null) {
    const coverage = payload.provider_usage_coverage;
    return {
      kind: 'unavailable',
      state: coverage === 'partial' ? 'partial' : 'unavailable',
      detail:
        coverage === null
          ? 'no exact provider usage scope resolved for this dashboard'
          : `the provider usage aggregate is ${coverage}; exact per-provider attribution needs a complete aggregate`,
      payload,
    };
  }
  return { kind: 'ready', payload, ledger };
}

function AttributionBody({
  attribution,
  children,
}: {
  attribution: AttributionRead;
  children: (ready: Extract<AttributionRead, { kind: 'ready' }>) => ReactNode;
}) {
  if (attribution.kind === 'ready') return <>{children(attribution)}</>;
  return <ReadModelState kind={attribution.state} detail={attribution.detail} />;
}

function OverviewBody({
  read,
  children,
}: {
  read: OverviewRead;
  children: (payload: SavingsOverviewPayloadV1) => ReactNode;
}) {
  if (read.kind === 'ready') return <>{children(read.value.payload)}</>;
  return <ReadModelState kind={read.state} detail={read.detail} />;
}

/* ------------------------------------------------------------------------ */
/* Query register: range control + authority states                          */
/* ------------------------------------------------------------------------ */

interface RegisterState {
  label: string;
  kind: DomainStateKind;
  detail?: string;
}

function registerStates(overview: OverviewRead, attribution: AttributionRead): RegisterState[] {
  const spend: RegisterState =
    attribution.kind === 'ready'
      ? spendState(attribution.ledger)
      : { label: 'provider spend', kind: attribution.state, detail: attribution.detail };

  const usage: RegisterState =
    overview.kind !== 'ready'
      ? { label: 'usage', kind: overview.state, detail: overview.detail }
      : usageState(overview.value.payload);

  const savings: RegisterState =
    overview.kind !== 'ready'
      ? { label: 'savings ledger', kind: overview.state }
      : overview.value.payload.savings.available
        ? { label: 'savings ledger', kind: 'ready' }
        : overview.value.payload.savings.error != null
          ? { label: 'savings ledger', kind: 'error', detail: overview.value.payload.savings.error }
          : { label: 'savings ledger', kind: 'unavailable', detail: 'not mounted' };

  return [spend, usage, savings];
}

/** A served ledger with no usage is a measured empty, not a ready reading. */
function spendState(ledger: ProviderLedger): RegisterState {
  if (ledger.usageEvents === 0) {
    return { label: 'provider spend', kind: 'complete_zero_findings', detail: 'no usage in range' };
  }
  return {
    label: 'provider spend',
    kind: ledger.complete ? 'ready' : 'partial',
    detail: `${formatShare(ledger.coverage)} priced`,
  };
}

function usageState(payload: SavingsOverviewPayloadV1): RegisterState {
  const usage = payload.provider_usage;
  if (!usage.available) {
    return usage.status === 'read_failed' || usage.error != null
      ? { label: 'usage', kind: 'error', detail: usage.error ?? 'read failed' }
      : { label: 'usage', kind: 'unavailable', detail: usage.status ?? 'not served' };
  }
  switch (usage.status) {
    case 'complete':
      return { label: 'usage', kind: 'ready', detail: 'complete aggregate' };
    case 'partial':
      return { label: 'usage', kind: 'partial', detail: 'partial aggregate' };
    case null:
      // Served without its coverage word: neither claimed complete nor partial.
      return { label: 'usage', kind: 'unknown', detail: 'aggregate coverage not reported' };
    default:
      return { label: 'usage', kind: 'unknown', detail: usage.status };
  }
}

function QueryRegister({
  range,
  onRange,
  provider,
  selectedPresent,
  onClearProvider,
  states,
}: {
  range: CostsRange;
  onRange: (range: CostsRange) => void;
  provider: string | null;
  selectedPresent: boolean;
  onClearProvider: () => void;
  states: RegisterState[];
}) {
  return (
    <div
      className="flex min-h-[52px] shrink-0 flex-wrap items-center gap-x-4 gap-y-2 border-b border-edge-subtle bg-surface-1 px-3 py-1"
      data-costs-register
    >
      <RangeControl range={range} onRange={onRange} />
      <span className="min-w-0 truncate text-3xs text-text-muted">{costsRangeNote(range)}</span>
      <span aria-hidden className="td-rule max-md:hidden" />
      <ul className="flex flex-wrap items-center gap-x-3 gap-y-1" aria-label="Costs authority states">
        {states.map((state) => (
          <li key={state.label} className="flex items-center gap-1.5">
            <span className="td-legend">{state.label}</span>
            <StateChip kind={state.kind} detail={state.detail} />
          </li>
        ))}
        <li className="flex items-center gap-1.5" data-costs-selection={provider ?? 'all'}>
          <span className="td-legend">selection</span>
          <span className="td-value text-2xs text-text-secondary">
            Costs / {provider ?? 'all'}
            {provider !== null && !selectedPresent ? (
              <span className="text-text-muted"> · not in this range</span>
            ) : null}
          </span>
          {provider !== null ? (
            <button
              type="button"
              className="td-hit -my-2 px-1 text-2xs text-text-secondary underline-offset-2 hover:underline"
              onClick={onClearProvider}
              aria-label={`Clear provider scope ${provider}`}
            >
              clear
            </button>
          ) : null}
        </li>
      </ul>
    </div>
  );
}

/** The range tabs: ARIA tablist with a roving tabindex, the same pattern the
 * Observatory wings use, so the keyboard contract is one contract. */
function RangeControl({
  range,
  onRange,
}: {
  range: CostsRange;
  onRange: (range: CostsRange) => void;
}) {
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const move = (from: number, delta: number) => {
    const count = COSTS_RANGES.length;
    const to = (from + delta + count) % count;
    const next = COSTS_RANGES[to];
    if (next === undefined) return;
    onRange(next);
    tabs.current[to]?.focus();
  };
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, position: number) => {
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        event.preventDefault();
        move(position, 1);
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        event.preventDefault();
        move(position, -1);
        break;
      case 'Home':
        event.preventDefault();
        move(0, 0);
        break;
      case 'End':
        event.preventDefault();
        move(COSTS_RANGES.length - 1, 0);
        break;
      default:
        break;
    }
  };
  return (
    <div
      role="tablist"
      aria-label="Spend range"
      aria-orientation="horizontal"
      className="flex flex-wrap items-center gap-1 border border-edge-subtle bg-surface-1 p-1"
      data-costs-range={range}
    >
      {COSTS_RANGES.map((candidate, position) => {
        const selected = candidate === range;
        return (
          <button
            key={candidate}
            ref={(node) => {
              tabs.current[position] = node;
            }}
            type="button"
            role="tab"
            aria-selected={selected}
            tabIndex={selected ? 0 : -1}
            onClick={() => onRange(candidate)}
            onKeyDown={(event) => onKeyDown(event, position)}
            className={cn(
              'flex min-h-[44px] items-center gap-2 whitespace-nowrap border px-3 text-2xs',
              'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
              selected
                ? 'border-edge-strong bg-surface-3 text-text-primary'
                : 'border-transparent text-text-secondary hover:bg-surface-2',
            )}
          >
            <span
              aria-hidden
              className={cn('h-3 w-px shrink-0', selected ? 'bg-accent' : 'bg-edge-strong')}
            />
            {costsRangeLabel(candidate)}
          </button>
        );
      })}
    </div>
  );
}

/* ------------------------------------------------------------------------ */
/* Usage readouts                                                            */
/* ------------------------------------------------------------------------ */

function UsageReadouts({
  ledger,
  attribution,
  overview,
  range,
}: {
  ledger: ProviderLedger | null;
  attribution: AttributionRead;
  overview: OverviewRead;
  range: CostsRange;
}) {
  const tokens = ledger === null ? null : sumReportedTokens(ledger.rows);
  const savings = overview.kind === 'ready' ? overview.value.payload.savings : null;
  const windows = savings?.available ? savings.ledger : null;
  return (
    <div className="flex flex-col gap-4">
      <p className="text-3xs leading-relaxed text-text-muted">
        facts independent of price: counts and tokens the providers reported for this range
      </p>
      {ledger === null ? (
        <div role="status">
          <StateChip
            kind={attribution.kind === 'ready' ? 'unavailable' : attribution.state}
            detail={attribution.kind === 'ready' ? undefined : attribution.detail}
          />
        </div>
      ) : (
        <>
          <div className="flex flex-col gap-1.5">
            <Readout label="usage events" size="xl" value={ledger.usageEvents.toLocaleString()} />
            <ul className="flex flex-col gap-0.5 text-3xs text-text-muted" data-usage-breakdown>
              <li>{ledger.pricedEvents.toLocaleString()} priced</li>
              <li>{ledger.unpricedEvents.toLocaleString()} unpriced</li>
              <li>{ledger.undatedEvents.toLocaleString()} undated</li>
            </ul>
          </div>
          <div className="flex flex-col gap-1.5">
            <Readout label="tokens consumed" size="xl" {...splitCount(tokens?.tokens, 1_000)} />
            <p className="text-3xs leading-relaxed text-text-muted">
              {tokens === null || tokens.reported === 0
                ? 'no provider reported a token total'
                : tokens.unreported > 0
                  ? `input + output over ${tokens.reported.toLocaleString()} of ${ledger.rows.length.toLocaleString()} providers`
                  : 'input + output, provider-reported'}
            </p>
          </div>
        </>
      )}

      <div className="flex flex-col gap-2 border-t border-edge-subtle pt-3">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span className="td-legend">saved tokens</span>
          <span aria-hidden className="td-rule" />
          <span className="td-legend text-text-muted">count only</span>
        </div>
        {savings === null ? (
          <ReadModelState
            kind={overview.kind === 'ready' ? 'unknown' : overview.state}
            detail={overview.kind === 'ready' ? undefined : overview.detail}
          />
        ) : !savings.available || windows == null ? (
          <div role="status">
            <StateChip
              kind={savings.error != null ? 'error' : 'unavailable'}
              detail={
                savings.error != null
                  ? `savings ledger read failed: ${savings.error}`
                  : 'the savings ledger is not mounted'
              }
            />
          </div>
        ) : (
          <SavedWindows windows={windows} range={range} />
        )}
        <p className="text-3xs leading-relaxed text-text-muted">
          Saved tokens are counts from the savings ledger. They are not priced: no authority prices
          the avoided usage on the same basis as the observed usage.
        </p>
      </div>
    </div>
  );
}

const WINDOW_ORDER: readonly { key: keyof SavingsLedgerSummaryV1; label: string; range: CostsRange }[] = [
  { key: 'today', label: 'today', range: 'today' },
  { key: 'last_7d', label: '7d', range: '7d' },
  { key: 'last_30d', label: '30d', range: '30d' },
  { key: 'all_time', label: 'all time', range: 'all' },
];

/** The four nested windows as one accumulating quantity seen at four depths;
 * the window matching the range control is the headline. */
function SavedWindows({
  windows,
  range,
}: {
  windows: SavingsLedgerSummaryV1;
  range: CostsRange;
}) {
  const active = WINDOW_ORDER.find((entry) => entry.range === range) ?? WINDOW_ORDER[3];
  const headline = active === undefined ? null : windows[active.key];
  const ceiling = windows.all_time.saved_tokens;
  return (
    <div className="flex flex-col gap-2">
      {headline && active ? (
        <Readout
          label={`saved · ${active.label}`}
          size="lg"
          {...splitCount(headline.saved_tokens, 1_000)}
          note={perCall(headline)}
        />
      ) : null}
      <ul className="flex flex-col gap-1">
        {WINDOW_ORDER.map((entry) => {
          const window = windows[entry.key];
          return (
            <li key={entry.key} className="flex items-center gap-2 text-2xs" data-saved-window={entry.key}>
              <span className={cn('w-14 shrink-0 td-legend', entry.range === range && 'text-accent')}>
                {entry.label}
              </span>
              <Meter
                fraction={ceiling > 0 ? window.saved_tokens / ceiling : null}
                height="row"
                className="min-w-0 flex-1"
              />
              <span className="td-value w-16 shrink-0 text-right text-text-secondary" data-cell="numeric">
                {formatCount(window.saved_tokens, 1_000)}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function perCall(window: SavingsSumV1): string {
  if (!Number.isFinite(window.calls) || window.calls <= 0) return 'no calls recorded';
  return `${window.calls.toLocaleString()} calls · ${formatCount(
    Math.round(window.saved_tokens / window.calls),
    1_000,
  )}/call`;
}
