import {
  AnalyticsHintsPayloadV1Schema,
  type AnalyticsHintCategoryV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { EnvelopeSection } from '../../ui/ReadSection.tsx';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';

/**
 * Hook hints as the daemon measured them: per category, how many hints the
 * hooks emitted and what the receiving agent then did — followed, ignored, or
 * suppressed. The counts come from `analytics_api::typed_hint_summary` over
 * durable analytics events; nothing here is derived in the browser.
 *
 * The payload names its own `source` and carries an `error` sentence when the
 * hint store could not be read; both are printed rather than smoothed over. A
 * category's four counts are independent tallies, not a partition of
 * `emitted` — a hint can be emitted in one window and acted on in a later one
 * — so no percentage or funnel is drawn from them.
 */
export function HookHints() {
  const hints = useEnvelope(
    ['analytics', 'hints'],
    '/api/plugins/analytics/hints',
    AnalyticsHintsPayloadV1Schema,
  );
  return (
    <EnvelopeSection
      title="Hook hints"
      pending={hints.isPending}
      result={hints.data}
      loadingDetail="reading hint analytics"
      transportDetail="hint analytics could not be read"
    >
      {(envelope) => {
        const payload = envelope.payload;
        if (payload == null) {
          return (
            <p className="text-2xs text-text-muted">
              the daemon sent no hint payload with this envelope
            </p>
          );
        }
        return (
          <div className="flex flex-col gap-2">
            <EnvelopeTruth
              envelope={envelope}
              refreshing={hints.isFetching}
              onRefresh={() => void hints.refetch()}
            />
            {payload.error != null ? (
              <p role="status" className="text-2xs leading-relaxed text-state-error">
                {payload.error}
              </p>
            ) : null}
            {!payload.available ? (
              <p className="text-2xs text-text-muted">
                the hint analytics source is unavailable, so no counts can be shown
              </p>
            ) : payload.by_category.length === 0 ? (
              <p className="text-2xs text-text-muted">
                no hook hints have been recorded in the analytics window
              </p>
            ) : (
              <HintTable categories={payload.by_category} />
            )}
            <p className="text-3xs leading-relaxed text-text-muted">
              source: {payload.source} · counts are independent tallies within the analytics
              window, not a funnel
            </p>
          </div>
        );
      }}
    </EnvelopeSection>
  );
}

function HintTable({ categories }: { categories: readonly AnalyticsHintCategoryV1[] }) {
  return (
    <table className="w-full border-collapse text-2xs">
      <caption className="sr-only">
        Hook hint counts by category: emitted, followed, ignored, and suppressed.
      </caption>
      <thead>
        <tr className="td-legend border-b border-edge-subtle text-left">
          <th scope="col" className="py-1 pr-2 font-normal">
            category
          </th>
          <th scope="col" className="py-1 pr-2 text-right font-normal">
            emitted
          </th>
          <th scope="col" className="py-1 pr-2 text-right font-normal">
            followed
          </th>
          <th scope="col" className="py-1 pr-2 text-right font-normal">
            ignored
          </th>
          <th scope="col" className="py-1 text-right font-normal">
            suppressed
          </th>
        </tr>
      </thead>
      <tbody>
        {categories.map((category) => (
          <tr key={category.category} className="border-b border-edge-subtle last:border-b-0">
            <th scope="row" className="py-1 pr-2 text-left font-normal text-text-primary">
              {category.category}
            </th>
            <td className="tabular py-1 pr-2 text-right" data-cell="numeric">
              {category.emitted.toLocaleString()}
            </td>
            <td className="tabular py-1 pr-2 text-right" data-cell="numeric">
              {category.followed.toLocaleString()}
            </td>
            <td className="tabular py-1 pr-2 text-right" data-cell="numeric">
              {category.ignored.toLocaleString()}
            </td>
            <td className="tabular py-1 text-right" data-cell="numeric">
              {category.suppressed.toLocaleString()}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
