import type {
  SavingsPricingClassV1,
  SavingsProviderDayPointV1,
  SavingsProviderModelSpendV1,
  SavingsProviderSpendV1,
  SavingsProviderUsageAttributionV1,
  TokenActualV1,
} from '../../contracts/generated.ts';

/**
 * Provider spend attribution, as pure readings over the canonical projection.
 *
 * Every dollar here arrived priced from the daemon; nothing in this module
 * multiplies a token by a rate. What it does is rank, share, bucket, and —
 * above all — keep the pricing classes apart: a provider whose usage the
 * authority could price completely, one it priced in part, and one it could
 * not price at all are three readings, and a total that folds them together
 * must say which of them it includes.
 */

export type PricingClass = SavingsPricingClassV1;

export interface ProviderRow {
  provider: string;
  pricing: PricingClass;
  usageEvents: number;
  pricedEvents: number;
  unpricedEvents: number;
  unknownModelEvents: number;
  undatedEvents: number;
  models: number;
  pricedModels: number;
  unpricedModels: number;
  sessions: number;
  /** Dollars over the model groups priced completely; null when none were. */
  pricedCostUsd: number | null;
  /** The projector's complete total; null unless every event priced. */
  totalCostUsd: number | null;
  totalTokens: number | null;
  actual: TokenActualV1 | null;
  /** This provider's share of the priced total across providers, 0–1. Null
   * when the row or the total has no priced dollars. */
  share: number | null;
}

export interface ProviderLedger {
  /** Priced spend descending, then usage events descending, then name. */
  rows: ProviderRow[];
  /** Sum of every row's priced dollars; null when no row priced anything. */
  pricedTotalUsd: number | null;
  usageEvents: number;
  pricedEvents: number;
  unpricedEvents: number;
  unknownModelEvents: number;
  undatedEvents: number;
  /** `pricedEvents / usageEvents`, or null when nothing was observed. */
  coverage: number | null;
  /** Every observed event priced, so the priced total is the canonical total. */
  complete: boolean;
  /** Providers by pricing class, in ledger order. */
  classes: Record<PricingClass, string[]>;
  pricingRevision: string | null;
}

function rowFromWire(row: SavingsProviderSpendV1): Omit<ProviderRow, 'share'> {
  return {
    provider: row.provider,
    pricing: row.pricing,
    usageEvents: row.usage_events,
    pricedEvents: row.priced_events,
    unpricedEvents: row.unpriced_events,
    unknownModelEvents: row.unknown_model_events,
    undatedEvents: row.undated_events,
    models: row.models,
    pricedModels: row.priced_models,
    unpricedModels: row.unpriced_models,
    sessions: row.sessions,
    pricedCostUsd: finiteOrNull(row.priced_cost_usd),
    totalCostUsd: finiteOrNull(row.total_cost_usd),
    totalTokens: row.total_tokens,
    actual: row.provider_actual,
  };
}

function finiteOrNull(value: number | null | undefined): number | null {
  return value != null && Number.isFinite(value) ? value : null;
}

function compareRows(a: Omit<ProviderRow, 'share'>, b: Omit<ProviderRow, 'share'>): number {
  const costA = a.pricedCostUsd ?? -1;
  const costB = b.pricedCostUsd ?? -1;
  if (costA !== costB) return costB - costA;
  if (a.usageEvents !== b.usageEvents) return b.usageEvents - a.usageEvents;
  return a.provider.localeCompare(b.provider);
}

/**
 * The provider ledger, or null when the attribution block itself was not
 * served — which is a different fact from a served block with no providers.
 */
export function summarizeProviderLedger(
  attribution: SavingsProviderUsageAttributionV1,
): ProviderLedger | null {
  if (!attribution.available) return null;
  const base = attribution.by_provider.map(rowFromWire).sort(compareRows);
  const priced = base.filter((row) => row.pricedCostUsd != null);
  const pricedTotalUsd =
    priced.length === 0
      ? null
      : priced.reduce((sum, row) => sum + (row.pricedCostUsd ?? 0), 0);
  const rows: ProviderRow[] = base.map((row) => ({
    ...row,
    share:
      row.pricedCostUsd != null && pricedTotalUsd != null && pricedTotalUsd > 0
        ? row.pricedCostUsd / pricedTotalUsd
        : null,
  }));
  const usageEvents = rows.reduce((sum, row) => sum + row.usageEvents, 0);
  const pricedEvents = rows.reduce((sum, row) => sum + row.pricedEvents, 0);
  const unpricedEvents = rows.reduce((sum, row) => sum + row.unpricedEvents, 0);
  const classes: Record<PricingClass, string[]> = { priced: [], partial: [], unpriced: [] };
  for (const row of rows) classes[row.pricing].push(row.provider);
  return {
    rows,
    pricedTotalUsd,
    usageEvents,
    pricedEvents,
    unpricedEvents,
    unknownModelEvents: rows.reduce((sum, row) => sum + row.unknownModelEvents, 0),
    undatedEvents: attribution.undated_events ?? rows.reduce((sum, row) => sum + row.undatedEvents, 0),
    coverage: usageEvents > 0 ? pricedEvents / usageEvents : null,
    complete: usageEvents > 0 && unpricedEvents === 0,
    classes,
    pricingRevision: attribution.pricing_revision,
  };
}

/** The sum of the token totals the providers reported, and how many did not.
 * A provider with no total is left out and counted, never coalesced to zero. */
export function sumReportedTokens(rows: readonly ProviderRow[]): {
  tokens: number | null;
  reported: number;
  unreported: number;
} {
  let tokens: number | null = null;
  let reported = 0;
  let unreported = 0;
  for (const row of rows) {
    if (row.totalTokens == null) {
      unreported += 1;
      continue;
    }
    reported += 1;
    tokens = (tokens ?? 0) + row.totalTokens;
  }
  return { tokens, reported, unreported };
}

export interface SeriesPoint {
  /** Priced dollars that day; null when the provider had nothing priced. */
  pricedCostUsd: number | null;
  usageEvents: number;
  unpricedEvents: number;
}

export interface ProviderSeries {
  provider: string;
  /** One entry per `days` index; null where the provider had no usage. */
  points: (SeriesPoint | null)[];
}

export interface SpendSeries {
  /** UTC day starts, seconds, ascending. */
  days: number[];
  series: ProviderSeries[];
  /** (day, provider) buckets carrying at least one unpriced event. */
  unpricedBuckets: number;
  /** Buckets where the drawn dollar figure covers only part of the usage. */
  partialBuckets: number;
}

/**
 * The dated series behind the spend field, one line per provider in ledger
 * order. Buckets come from the daemon as UTC days and are drawn as given —
 * this never re-bins, interpolates, or carries a value across a gap.
 */
export function buildSpendSeries(
  points: readonly SavingsProviderDayPointV1[],
  providerOrder: readonly string[],
): SpendSeries | null {
  if (points.length === 0) return null;
  const days = [...new Set(points.map((point) => point.day))].sort((a, b) => a - b);
  const index = new Map(days.map((day, position) => [day, position] as const));
  const known = new Set(providerOrder);
  const extra = [...new Set(points.map((point) => point.provider))]
    .filter((provider) => !known.has(provider))
    .sort();
  const providers = [...providerOrder, ...extra];
  const byProvider = new Map<string, (SeriesPoint | null)[]>(
    providers.map((provider) => [provider, days.map(() => null)]),
  );
  let unpricedBuckets = 0;
  let partialBuckets = 0;
  for (const point of points) {
    const row = byProvider.get(point.provider);
    const position = index.get(point.day);
    if (!row || position === undefined) continue;
    const pricedCostUsd = finiteOrNull(point.priced_cost_usd);
    if (point.unpriced_events > 0) {
      unpricedBuckets += 1;
      if (pricedCostUsd != null) partialBuckets += 1;
    }
    row[position] = {
      pricedCostUsd,
      usageEvents: point.usage_events,
      unpricedEvents: point.unpriced_events,
    };
  }
  return {
    days,
    series: providers
      .map((provider) => ({ provider, points: byProvider.get(provider) ?? [] }))
      .filter((entry) => entry.points.some((point) => point !== null)),
    unpricedBuckets,
    partialBuckets,
  };
}

export interface ModelRow {
  provider: string;
  /** Null when the usage observation carried no model identity. */
  model: string | null;
  usageEvents: number;
  costUsd: number | null;
  totalTokens: number | null;
  actual: TokenActualV1 | null;
}

/** The exact provider/model rows behind one provider, priced groups first. */
export function providerModelRows(
  byModel: readonly SavingsProviderModelSpendV1[],
  provider: string,
): ModelRow[] {
  return byModel
    .filter((row) => row.provider === provider)
    .map((row) => ({
      provider: row.provider,
      model: row.model,
      usageEvents: row.usage_events,
      costUsd: finiteOrNull(row.cost_usd),
      totalTokens: row.total_tokens,
      actual: row.provider_actual,
    }))
    .sort((a, b) => {
      const costA = a.costUsd ?? -1;
      const costB = b.costUsd ?? -1;
      if (costA !== costB) return costB - costA;
      if (a.usageEvents !== b.usageEvents) return b.usageEvents - a.usageEvents;
      return (a.model ?? '').localeCompare(b.model ?? '');
    });
}

/** A content-addressed pricing revision trimmed for a legend; the full digest
 * stays on the authority panel. */
export function shortRevision(revision: string | null): string {
  if (revision === null) return 'not reported';
  const separator = revision.indexOf(':');
  if (separator < 0) return revision.length > 14 ? `${revision.slice(0, 14)}…` : revision;
  return `${revision.slice(0, separator + 1)}${revision.slice(separator + 1, separator + 11)}…`;
}

/** A UTC day start (seconds) as its calendar date. */
export function formatUtcDay(daySeconds: number): string {
  return new Date(daySeconds * 1000).toISOString().slice(0, 10);
}

/** Dollars, two places, with the sign the ledger recorded. `null` in, em dash
 * out: an unpriced figure is not a free one. */
export function formatUsd(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—';
  return `$${value.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

/** A 0–1 fraction as a whole-number percentage, or an em dash. */
export function formatShare(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—';
  return `${(value * 100).toLocaleString(undefined, { maximumFractionDigits: 1 })}%`;
}

export function pricingClassLabel(pricing: PricingClass): string {
  switch (pricing) {
    case 'priced':
      return 'priced';
    case 'partial':
      return 'partially priced';
    case 'unpriced':
      return 'unpriced';
    default: {
      const unhandled: never = pricing;
      return unhandled;
    }
  }
}

/** What a pricing class means for the dollar figure beside it. */
export function pricingClassSentence(row: ProviderRow): string {
  switch (row.pricing) {
    case 'priced':
      return `every one of ${row.usageEvents.toLocaleString()} usage events priced against the canonical table`;
    case 'partial':
      return `${row.pricedEvents.toLocaleString()} of ${row.usageEvents.toLocaleString()} usage events priced; the dollar figure excludes the other ${row.unpricedEvents.toLocaleString()}`;
    case 'unpriced':
      return `none of ${row.usageEvents.toLocaleString()} usage events could be priced, so this provider has no dollar figure`;
    default: {
      const unhandled: never = row.pricing;
      return unhandled;
    }
  }
}
