import type { SavingsPricingSummaryV1 } from '../../contracts/generated.ts';
import { Fact } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn.ts';
import { formatShare, shortRevision, type ProviderLedger } from './attribution.ts';

/**
 * Who priced what.
 *
 * The pricing authority is one bundled, content-addressed table; this panel
 * names it and then files every provider that recorded usage under how much
 * of that usage the table could price. The classes are the brief's, kept
 * apart on purpose: priced, partially priced, unpriced, usage with no model
 * identity to price against, and the block being unavailable at all.
 */
export function PricingAuthority({
  pricing,
  ledger,
  coverageStatus,
}: {
  pricing: SavingsPricingSummaryV1;
  ledger: ProviderLedger | null;
  /** The daemon's own word for the provider-usage aggregate behind the ledger. */
  coverageStatus: string | null;
}) {
  const source = stringOrNull(pricing.source);
  const revision = stringOrNull(pricing.revision);
  const modelCount = numberOrNull(pricing.model_count);
  const offline = pricing.offline === true ? 'offline' : pricing.offline === false ? 'online' : null;
  const fetchedAt = stringOrNull(pricing.fetched_at);
  return (
    <div className="flex min-w-0 flex-col gap-3">
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
        <Fact label="source" value={source ?? 'not reported'} muted={source === null} />
        <Fact label="mode" value={offline ?? 'not reported'} muted={offline === null} />
        <Fact
          label="models priced"
          value={modelCount === null ? 'not reported' : modelCount.toLocaleString()}
          muted={modelCount === null}
        />
        <Fact
          label="last fetch"
          value={fetchedAt ?? 'never fetched · bundled snapshot'}
          muted={fetchedAt === null}
        />
        <div className="col-span-2 flex min-w-0 flex-col gap-0.5">
          <dt className="td-legend">revision</dt>
          <dd className="td-value break-all text-3xs text-text-secondary" title={revision ?? undefined}>
            {revision ?? 'not reported'}
          </dd>
        </div>
      </dl>

      {ledger === null ? (
        <div role="status">
          <StateChip
            kind="unavailable"
            detail={
              coverageStatus === null
                ? 'provider usage attribution was not served for this range'
                : `provider usage aggregate reported ${coverageStatus}; no provider can be classed`
            }
          />
        </div>
      ) : (
        <>
          <div className="flex items-baseline justify-between gap-2 border-t border-edge-subtle pt-2">
            <span className="td-legend">pricing coverage</span>
            <span className="td-value text-sm text-text-primary" data-cell="numeric">
              {formatShare(ledger.coverage)}
            </span>
          </div>
          <p className="text-3xs leading-relaxed text-text-muted">
            {ledger.usageEvents === 0
              ? 'no usage events observed in this range, so coverage has no denominator'
              : `${ledger.pricedEvents.toLocaleString()} of ${ledger.usageEvents.toLocaleString()} usage events priced against revision ${shortRevision(ledger.pricingRevision)}`}
          </p>
          <ClassColumn
            label="priced"
            tone="bg-state-ready"
            providers={ledger.classes.priced}
            empty="no provider fully priced"
          />
          <ClassColumn
            label="partial"
            tone="bg-state-partial"
            pattern="hatched"
            providers={ledger.classes.partial}
            empty="no provider partially priced"
          />
          <ClassColumn
            label="unpriced"
            tone="bg-state-offline"
            pattern="dashed"
            providers={ledger.classes.unpriced}
            empty="no provider unpriced"
          />
          <div className="flex items-baseline justify-between gap-2 border-t border-edge-subtle pt-2">
            <span className="td-legend">null identity</span>
            <span className="td-value text-2xs text-text-secondary" data-cell="numeric">
              {ledger.unknownModelEvents.toLocaleString()} events
            </span>
          </div>
          <p className="text-3xs leading-relaxed text-text-muted">
            usage events whose observation named no model. They are counted and tokenised, and
            cannot be priced — there is no rate for an unknown model.
          </p>
          {coverageStatus !== null && coverageStatus !== 'complete' ? (
            <div role="status" className="border-t border-edge-subtle pt-2">
              <StateChip
                kind={coverageStatus === 'partial' ? 'partial' : 'unavailable'}
                detail={`provider usage aggregate ${coverageStatus}`}
              />
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}

function ClassColumn({
  label,
  tone,
  pattern,
  providers,
  empty,
}: {
  label: string;
  tone: string;
  pattern?: 'hatched' | 'dashed';
  providers: readonly string[];
  empty: string;
}) {
  return (
    <div className="flex flex-col gap-1 border-t border-edge-subtle pt-2" data-pricing-class={label}>
      <div className="flex items-center gap-2">
        <span
          aria-hidden
          className={cn('h-2 w-2 shrink-0', tone)}
          style={
            pattern === 'hatched'
              ? { maskImage: 'repeating-linear-gradient(45deg, black 0 1px, transparent 1px 3px)' }
              : pattern === 'dashed'
                ? { maskImage: 'repeating-linear-gradient(90deg, black 0 2px, transparent 2px 4px)' }
                : undefined
          }
        />
        <span className="td-legend">{label}</span>
        <span aria-hidden className="td-rule" />
        <span className="td-value text-3xs text-text-muted" data-cell="numeric">
          {providers.length.toLocaleString()}
        </span>
      </div>
      {providers.length === 0 ? (
        <p className="pl-4 text-3xs text-text-muted">{empty}</p>
      ) : (
        <ul className="flex flex-col gap-0.5 pl-4 text-2xs text-text-secondary">
          {providers.map((provider) => (
            <li key={provider} className="truncate" title={provider}>
              {provider}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function stringOrNull(value: unknown): string | null {
  return typeof value === 'string' && value !== '' ? value : null;
}

function numberOrNull(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}
