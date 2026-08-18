/**
 * REJECTED ARGUMENTS — Plan 26 frequency view over dispatcher rejections.
 *
 * Counts come from `GET /api/observatory` (`rejected_arguments`). The card
 * never computes a rate in the browser: when the server withheld
 * `rejection_rate`, the cell stays a typed absence.
 */
import { assertNever } from '../../contracts/generated.ts';
import type {
  CoverageStateV1,
  RejectedArgumentAnalyticsV1,
  RejectedArgumentErrorClassV1,
  RejectedArgumentGroupV1,
  RejectedArgumentNameV1,
  RejectedArgumentSurfaceV1,
} from '../../contracts/generated.ts';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';
import { EnvelopeSection } from '../../ui/ReadSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import type { ObservatoryAccountingReads } from './accountingReads.ts';

export function RejectedArguments({ reads }: { reads: ObservatoryAccountingReads }) {
  const { observatory: read } = reads;
  return (
    <EnvelopeSection
      title="Rejected arguments"
      blurb="dispatcher argument rejections grouped by surface, operation, and error class — rates stay absent when the attempt denominator is unknown"
      result={read.result}
      pending={read.pending}
      loadingDetail="requesting rejected-argument measurements"
      transportDetail="rejected-argument measurements could not be read"
    >
      {(envelope) => (
        <div className="flex flex-col gap-2">
          <EnvelopeTruth
            envelope={envelope}
            refreshing={read.refreshing}
            onRefresh={read.refresh}
          />
          <RejectedArgumentReadModel model={envelope.payload.rejected_arguments} />
        </div>
      )}
    </EnvelopeSection>
  );
}

function RejectedArgumentReadModel({ model }: { model: RejectedArgumentAnalyticsV1 }) {
  const state = coverageKind(model.coverage.state);
  if (model.rejected_total == null) {
    return (
      <div data-rejected-arguments="unavailable">
        <StateChip
          kind={state}
          detail={model.unavailable_reason ?? 'rejected-argument observations are unavailable'}
        />
      </div>
    );
  }
  if (model.groups.length === 0) {
    return (
      <div data-rejected-arguments="empty">
        <StateChip kind="ready" detail="no rejected-argument observations in this window" />
        <p className="text-2xs text-text-muted">
          a measured empty window is not a fabricated rate — the attempt denominator is{' '}
          {model.eligible_attempts == null
            ? 'unknown'
            : model.eligible_attempts.toLocaleString()}
        </p>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2" data-rejected-arguments="populated">
      <dl className="grid gap-x-4 gap-y-1 text-3xs sm:grid-cols-2">
        <Field label="rejected total">{model.rejected_total.toLocaleString()}</Field>
        <Field label="eligible attempts">
          {model.eligible_attempts == null ? '—' : model.eligible_attempts.toLocaleString()}
        </Field>
        <Field label="rejection rate">
          {model.rejection_rate == null ? '—' : model.rejection_rate.toFixed(4)}
        </Field>
        <Field label="redacted names">{model.redacted_name_count.toLocaleString()}</Field>
      </dl>
      <RejectedArgumentTable groups={model.groups} />
      <p className="text-3xs leading-relaxed text-text-muted">
        projector {model.projector_revision} · watermark {model.watermark} · rates are
        server-published and stay blank when the eligible-attempt denominator is unknown
      </p>
    </div>
  );
}

function RejectedArgumentTable({ groups }: { groups: readonly RejectedArgumentGroupV1[] }) {
  return (
    <table className="w-full border-collapse text-2xs">
      <caption className="sr-only">
        Rejected-argument counts by surface, operation, argument, and error class.
      </caption>
      <thead>
        <tr className="td-legend border-b border-edge-subtle text-left">
          <th scope="col" className="py-1 pr-2 font-normal">
            surface
          </th>
          <th scope="col" className="py-1 pr-2 font-normal">
            operation
          </th>
          <th scope="col" className="py-1 pr-2 font-normal">
            argument
          </th>
          <th scope="col" className="py-1 pr-2 font-normal">
            error class
          </th>
          <th scope="col" className="py-1 pr-2 text-right font-normal">
            count
          </th>
          <th scope="col" className="py-1 text-right font-normal">
            rate
          </th>
        </tr>
      </thead>
      <tbody>
        {groups.map((group) => (
          <tr
            key={`${group.surface}:${group.operation}:${group.argument}:${group.error_class}`}
            className="border-b border-edge-subtle last:border-b-0"
          >
            <th scope="row" className="py-1 pr-2 text-left font-normal text-text-primary">
              {surfaceLabel(group.surface)}
            </th>
            <td className="py-1 pr-2">{group.operation}</td>
            <td className="py-1 pr-2">{argumentLabel(group.argument)}</td>
            <td className="py-1 pr-2">{errorClassLabel(group.error_class)}</td>
            <td className="tabular py-1 pr-2 text-right" data-cell="numeric">
              {group.count.toLocaleString()}
            </td>
            <td className="tabular py-1 text-right" data-cell="numeric">
              {group.rate == null ? '—' : group.rate.toFixed(4)}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function coverageKind(state: CoverageStateV1): DomainStateKind {
  switch (state) {
    case 'known':
      return 'ready';
    case 'partial':
    case 'sampled':
    case 'capped':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'unknown':
      return 'unknown';
    default: {
      const exhaustive: never = state;
      return assertNever(exhaustive);
    }
  }
}

function surfaceLabel(surface: RejectedArgumentSurfaceV1): string {
  switch (surface) {
    case 'cli':
      return 'cli';
    case 'mcp':
      return 'mcp';
    case 'http':
      return 'http';
    case 'unknown':
      return 'unknown';
    default: {
      const exhaustive: never = surface;
      return assertNever(exhaustive);
    }
  }
}

function argumentLabel(argument: RejectedArgumentNameV1): string {
  switch (argument) {
    case 'request_body':
      return 'request_body';
    case 'pagination':
      return 'pagination';
    case 'request_handle':
      return 'request_handle';
    case 'operation':
      return 'operation';
    case 'lifecycle':
      return 'lifecycle';
    case 'unknown':
      return 'unknown';
    default: {
      const exhaustive: never = argument;
      return assertNever(exhaustive);
    }
  }
}

function errorClassLabel(errorClass: RejectedArgumentErrorClassV1): string {
  switch (errorClass) {
    case 'missing':
      return 'missing';
    case 'invalid_shape':
      return 'invalid_shape';
    case 'out_of_bounds':
      return 'out_of_bounds';
    case 'unsupported':
      return 'unsupported';
    case 'unauthorized':
      return 'unauthorized';
    case 'stale':
      return 'stale';
    case 'unknown':
      return 'unknown';
    default: {
      const exhaustive: never = errorClass;
      return assertNever(exhaustive);
    }
  }
}
