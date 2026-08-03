/**
 * The shape every Plan 26 canonical read model is rendered in.
 *
 * Observatory and Costs serve different projections, but the surface around
 * them is one thing and must stay one thing: the envelope's own truth header,
 * the server's omission reasons verbatim, the horizon the figures were measured
 * over, and then the metrics grouped by producing source. A surface that drifted
 * from this would be quietly asserting that its numbers are a different kind of
 * claim than the others', which they are not.
 *
 * Nothing here derives a health grade, and nothing renders a missing metric as
 * zero — `MetricGroups` prints the composer's own reason where the figure would
 * be. What varies between call sites is the projection, the window it names,
 * and what a reader is told when the payload is empty, so those are props.
 */
import { useQuery } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { MetricValueV1 } from '../contracts/generated.ts';
import { fetchEnvelope } from '../data/query/envelope.ts';
import type { WireSchema } from '../data/query/wireSchema.ts';
import { scopeKey, scopedUrl, useScope } from '../data/scope/store.ts';
import { EnvelopeTruth, OmissionReasons } from './EnvelopeTruth.tsx';
import { EnvelopeSection } from './ReadSection.tsx';
import { MetricGroups } from './MetricPlate.tsx';
import { cn } from './cn';

export function CanonicalReadModelSection<T>({
  title,
  blurb,
  queryKey,
  url,
  schema,
  refetchInterval,
  loadingDetail,
  className,
  metrics,
  emptyLabel,
  horizonAttributes,
  horizonFields,
  footer,
}: {
  title: string;
  blurb: ReactNode;
  /** The stable head of the query key; the active scope is appended, because
   * two scopes are two different reads of the same route. */
  queryKey: readonly string[];
  url: string;
  schema: WireSchema<T>;
  refetchInterval: number;
  loadingDetail: string;
  className?: string;
  metrics: (model: T) => MetricValueV1[];
  /** Said when the payload carried no measurements — which is a payload with no
   * metrics, never a set of zeroes. */
  emptyLabel: string;
  /** `data-*` markers the horizon line carries for this projection. */
  horizonAttributes: (model: T) => Record<string, string>;
  horizonFields: (model: T) => ReactNode;
  /** Rendered under the metrics, for a surface that has something further to
   * state about what it does not measure. */
  footer?: ReactNode;
}) {
  const scope = useScope((s) => s.scope);
  const read = useQuery({
    queryKey: [...queryKey, scopeKey(scope)],
    queryFn: () => fetchEnvelope(scopedUrl(scope, url), schema),
    refetchInterval,
  });

  return (
    <EnvelopeSection
      title={title}
      blurb={blurb}
      className={className}
      result={read.data}
      pending={read.isPending}
      loadingDetail={loadingDetail}
    >
      {(envelope) => {
        const model = envelope.payload;
        return (
          <>
            <EnvelopeTruth
              envelope={envelope}
              refreshing={read.isFetching}
              onRefresh={() => void read.refetch()}
            />
            <OmissionReasons coverage={envelope.coverage} />
            <dl
              className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
              {...horizonAttributes(model)}
            >
              {horizonFields(model)}
            </dl>
            <div className={cn('px-4 py-3', footer && 'flex flex-col gap-4')}>
              <MetricGroups metrics={metrics(model)} emptyLabel={emptyLabel} />
              {footer}
            </div>
          </>
        );
      }}
    </EnvelopeSection>
  );
}
