import type { SavingsProviderModelSpendV1 } from '../../contracts/generated.ts';
import { formatCount } from '../../ui/format.ts';
import { Fact, Meter } from '../../ui/instrument.tsx';
import {
  formatShare,
  formatUsd,
  pricingClassLabel,
  pricingClassSentence,
  providerModelRows,
  shortRevision,
  type ProviderLedger,
} from './attribution.ts';

/**
 * The workspace-owned inspector: exact evidence for the provider under the
 * pointer or focus, or for the scoped provider when nothing is inspected.
 *
 * Inspection reveals; it never scopes. Every figure names its source class
 * and grade: provider and model identity are `EXACT` from the usage
 * observation, the rate is `EXACT` from the bundled pricing revision when it
 * applied, and a model with no applicable rate is `UNAVAILABLE` — a named
 * gap, not a zero.
 */
export function CostsInspector({
  ledger,
  byModel,
  inspected,
  selected,
  pricingRevision,
}: {
  ledger: ProviderLedger;
  byModel: readonly SavingsProviderModelSpendV1[];
  inspected: string | null;
  selected: string | null;
  pricingRevision: string | null;
}) {
  const focus = inspected ?? selected;
  const row = focus === null ? null : ledger.rows.find((entry) => entry.provider === focus) ?? null;

  if (row === null) {
    return (
      <div className="flex flex-col gap-3 text-2xs leading-relaxed text-text-secondary" data-costs-inspector="idle">
        <p>
          Hover or focus a provider in the legend or the ledger to inspect its exact usage and
          pricing rows here. Inspection never changes the query; Enter scopes it, Escape clears it.
        </p>
        <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
          <Fact label="providers" value={ledger.rows.length.toLocaleString()} />
          <Fact label="usage events" value={ledger.usageEvents.toLocaleString()} />
          <Fact label="priced events" value={ledger.pricedEvents.toLocaleString()} />
          <Fact label="unpriced events" value={ledger.unpricedEvents.toLocaleString()} />
          <Fact label="priced total" value={formatUsd(ledger.pricedTotalUsd)} />
          <Fact label="coverage" value={formatShare(ledger.coverage)} />
        </dl>
        <p className="text-3xs text-text-muted">
          identity · OBSERVED / EXACT · rate · BUNDLED PRICING / EXACT where applied
        </p>
      </div>
    );
  }

  const models = providerModelRows(byModel, row.provider);
  const ceiling = models.reduce((max, model) => Math.max(max, model.costUsd ?? 0), 0);
  return (
    <div
      className="flex min-w-0 flex-col gap-3"
      data-costs-inspector={inspected === null ? 'selected' : 'inspected'}
      data-provider={row.provider}
    >
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <h3 className="td-value text-base text-text-primary">{row.provider}</h3>
        <span className="td-legend">
          {inspected === null ? 'scoped' : 'inspecting'} · {pricingClassLabel(row.pricing)}
        </span>
      </div>
      <p className="text-2xs leading-relaxed text-text-secondary">{pricingClassSentence(row)}.</p>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
        <Fact label="priced spend" value={formatUsd(row.pricedCostUsd)} />
        <Fact label="share of priced" value={formatShare(row.share)} />
        <Fact label="complete total" value={formatUsd(row.totalCostUsd)} muted={row.totalCostUsd === null} />
        <Fact label="sessions" value={row.sessions.toLocaleString()} />
        <Fact label="models" value={row.models.toLocaleString()} />
        <Fact
          label="no model identity"
          value={`${row.unknownModelEvents.toLocaleString()} events`}
          muted={row.unknownModelEvents === 0}
        />
        <Fact label="input tokens" value={formatCount(row.actual?.input_tokens, 1_000)} />
        <Fact label="output tokens" value={formatCount(row.actual?.output_tokens, 1_000)} />
        <Fact label="cache read" value={formatCount(row.actual?.cache_read_tokens, 1_000)} />
        <Fact label="cache write" value={formatCount(row.actual?.cache_write_tokens, 1_000)} />
        <Fact
          label="undated events"
          value={row.undatedEvents.toLocaleString()}
          muted={row.undatedEvents === 0}
        />
        <Fact label="identity" value="OBSERVED / EXACT" />
      </dl>

      <ModelRows models={models} ceiling={ceiling} pricingRevision={pricingRevision} />
    </div>
  );
}

function ModelRows({
  models,
  ceiling,
  pricingRevision,
}: {
  models: ReturnType<typeof providerModelRows>;
  ceiling: number;
  pricingRevision: string | null;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1.5 border-t border-edge-subtle pt-2">
      <div className="flex items-center gap-2">
        <span className="td-legend">models · exact provider/model rows</span>
        <span aria-hidden className="td-rule" />
        <span className="td-value text-3xs text-text-muted" data-cell="numeric">
          {models.length.toLocaleString()}
        </span>
      </div>
      {models.length === 0 ? (
        <p className="text-2xs text-text-muted">
          the projection carried no model rows for this provider
        </p>
      ) : (
        <ul className="flex flex-col divide-y divide-edge-subtle" data-costs-model-rows>
          {models.map((model) => (
            <li
              key={model.model ?? '∅'}
              className="flex flex-col gap-1 py-1.5 text-2xs"
              data-model={model.model ?? undefined}
              data-model-pricing={model.costUsd === null ? 'unpriced' : 'priced'}
            >
              <div className="flex items-baseline gap-2">
                <span
                  className="min-w-0 flex-1 truncate font-mono text-text-primary"
                  title={model.model ?? undefined}
                >
                  {model.model ?? 'model not recorded'}
                </span>
                <span className="td-value shrink-0 text-text-secondary" data-cell="numeric">
                  {model.costUsd === null ? 'unpriced' : formatUsd(model.costUsd)}
                </span>
              </div>
              <div className="flex items-center gap-2">
                <Meter
                  fraction={model.costUsd !== null && ceiling > 0 ? model.costUsd / ceiling : null}
                  height="row"
                  className="w-24 shrink-0 max-sm:hidden"
                />
                <span className="min-w-0 flex-1 truncate text-3xs text-text-muted">
                  {model.usageEvents.toLocaleString()} events · {formatCount(model.totalTokens, 1_000)}{' '}
                  tokens ·{' '}
                  {model.model === null
                    ? 'identity UNAVAILABLE · no model to price'
                    : model.costUsd === null
                      ? 'rate UNAVAILABLE · no applicable canonical rate'
                      : `rate EXACT · ${pricingRevision === null ? 'bundled table' : shortRevision(pricingRevision)}`}
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
