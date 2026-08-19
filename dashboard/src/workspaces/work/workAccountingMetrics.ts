import type {
  ExecutionTopologyMeasurementV1,
  ExecutionTopologyMetricsV1,
  MetricCoverageV1,
} from '../../contracts/index.ts';
import { formatMicrosUtc } from '../../ui/format.ts';
import { humanizeMetric } from '../../ui/metricModel.ts';
import type { WorkResult } from './workApi.ts';
import type { WorkChannel } from './workChannel.ts';
import {
  accountingDimensionTitle,
  type WorkAccountingCard,
  type WorkAccountingDimension,
  type WorkAccountingProvenance,
  type WorkAccountingRow,
} from './workAccountingModel.ts';

/**
 * The Plan 24 integration/stack cards, fed from the mounted
 * `operation.work.topology_metrics` read.
 *
 * Work mounts no integration apply/review/stack mutation operation — Plan 24
 * keeps accepted integration lowered only through the Plan 36
 * native-integration family. These builders decode the observed accounting
 * the projection publishes, cell by cell: nothing here derives a rate, sums a
 * family, or joins the horizon aggregate to the topology generation.
 */

/** The Plan 26 descriptor the integration-outcome cells carry. */
export const MERGE_ATTEMPTS_METRIC = 'work_merge_attempts_total';

/** The metrics read's own reason, phrased for a channel. */
function metricsAbsence(
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined,
  measure: string,
): WorkChannel<never> {
  if (metrics === undefined) {
    return {
      available: false,
      state: 'loading',
      detail: `the topology-metrics read has not answered yet, so ${measure} is not drawn`,
    };
  }
  if (metrics.outcome === 'refused') {
    return {
      available: false,
      state: metrics.state,
      detail: `${measure} is read from the mounted topology-metrics operation, and that read was refused: ${metrics.detail}`,
    };
  }
  return {
    available: false,
    state: 'unknown',
    detail: `the topology-metrics read answered without ${measure}`,
  };
}

function modelOf(
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined,
): ExecutionTopologyMetricsV1 | null {
  return metrics !== undefined && metrics.outcome === 'value' ? metrics.value : null;
}

/** The daemon's typed absence for one measurement cell, verbatim. */
function cellAbsence(measurement: ExecutionTopologyMeasurementV1): WorkChannel<never> {
  const reason =
    measurement.unavailable ??
    measurement.value.unavailable_reason ??
    'the projector published no reason';
  return {
    available: false,
    state: reason === 'store_unavailable' ? 'unavailable' : 'unknown',
    detail: `the projector published this cell as a typed absence: ${humanizeMetric(reason)}`,
  };
}

function horizonSentence(model: ExecutionTopologyMetricsV1): string {
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });
  return `${stamp(model.horizon.since_micros)} → ${stamp(model.horizon.until_micros)} · watermark ${model.watermark}`;
}

const ANCHORS_ABSENCE: WorkChannel<never> = {
  available: false,
  state: 'redacted',
  detail:
    'the metrics read publishes registered observation cursors, not task/run/attempt identities; drill-down resolves them only through the authorized local observability boundary',
};

/** The seven facets, decoded from one metric envelope's own coverage. The
 * descriptor revision is a channel: a reading without one states the absence
 * rather than inventing a revision. */
function metricsProvenance(
  model: ExecutionTopologyMetricsV1,
  coverage: MetricCoverageV1,
  descriptorRevision: WorkAccountingProvenance['descriptorRevision'],
  population: string,
): WorkAccountingProvenance {
  return {
    support: {
      available: true,
      value: {
        value: coverage.observed,
        unit: 'cases',
        note: `${population} the projector observed in the horizon`,
      },
    },
    eligible:
      coverage.eligible == null
        ? {
            available: false,
            state: 'partial',
            detail: `the projector did not prove the eligible denominator for ${population}, so the observed count is a floor rather than a total`,
          }
        : {
            available: true,
            value: { value: coverage.eligible, unit: 'cases', note: population },
          },
    censoring: {
      available: true,
      value: {
        censored: coverage.censored,
        unknown: coverage.unknown,
        note: 'censored and unknown counts are the projector\u2019s own, decoded from the metric envelope',
      },
    },
    intervalCoverage: {
      available: true,
      value: `${coverage.state} coverage · ${coverage.observed} observed · ${coverage.completed} completed`,
    },
    horizon: { available: true, value: horizonSentence(model) },
    descriptorRevision,
    anchors: ANCHORS_ABSENCE,
  };
}

/** Facets for a model whose every cell is a typed absence: suppression wipes
 * a cell's value and coverage, so the counted facets carry the projector's
 * reason while the untouched horizon and descriptor revision stay real. */
function unreadableCellProvenance(
  model: ExecutionTopologyMetricsV1,
  reason: WorkChannel<never>,
  descriptorRevision: WorkAccountingProvenance['descriptorRevision'],
): WorkAccountingProvenance {
  return {
    support: reason,
    eligible: reason,
    censoring: reason,
    intervalCoverage: reason,
    horizon: { available: true, value: horizonSentence(model) },
    descriptorRevision,
    anchors: ANCHORS_ABSENCE,
  };
}

/** Every facet carrying the metrics read's own absence. */
function absentMetricsProvenance(
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined,
  population: string,
): WorkAccountingProvenance {
  return {
    support: metricsAbsence(metrics, `the ${population} support count`),
    eligible: metricsAbsence(metrics, `the ${population} eligible denominator`),
    censoring: metricsAbsence(metrics, 'the censored and unknown counts'),
    intervalCoverage: metricsAbsence(metrics, 'interval coverage'),
    horizon: metricsAbsence(metrics, 'the observation horizon'),
    descriptorRevision: metricsAbsence(metrics, 'the descriptor revision'),
    anchors: metricsAbsence(metrics, 'safe drill anchors'),
  };
}

function card(
  dimension: WorkAccountingDimension,
  mandate: string,
  reading: WorkChannel<string>,
  rows: readonly WorkAccountingRow[],
  provenance: WorkAccountingProvenance,
): WorkAccountingCard {
  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate,
    reading,
    rows,
    matrices: null,
    contradictions: [],
    provenance,
  };
}

const INTEGRATION_MANDATE = 'observed native fast-forward/merge/cherry-pick outcomes';

/**
 * Observed integration outcomes: one row per `work_merge_attempts_total`
 * kind × outcome cell. Suppressed cells (null value, wiped coverage) stay the
 * projector's typed absences; only a readable cell may lend the card its
 * headline and coverage authority, and no cell is ever summed.
 */
export function integrationOutcomesCard(
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined,
): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'integration_outcomes';
  const model = modelOf(metrics);

  const cells =
    model?.measurements.filter(
      (measurement) => measurement.value.metric === MERGE_ATTEMPTS_METRIC,
    ) ?? [];
  const dimensionalCells = cells.filter((measurement) => measurement.dimensions.length > 0);
  const readableCells = dimensionalCells.filter((measurement) => measurement.value.value != null);
  const authority = readableCells[0];

  const rows: WorkAccountingRow[] = dimensionalCells.map((measurement) => ({
    key: measurement.dimensions.map((cell) => String(cell.value)).join('_'),
    label: measurement.dimensions.map((cell) => humanizeMetric(cell.value)).join(' · '),
    channel:
      measurement.value.value == null
        ? cellAbsence(measurement)
        : {
            available: true,
            value: {
              value: measurement.value.value,
              unit: 'cases',
              note: 'observed native integrations with this kind and outcome, decoded from one projector cell',
            },
          },
  }));

  // Suppression never wipes a cell's descriptor revision, so it stays real
  // whenever any cell exists.
  const descriptorRevision: WorkAccountingProvenance['descriptorRevision'] =
    cells[0] === undefined
      ? {
          available: false,
          state: 'unknown',
          detail: 'the projection carried no merge-attempt cell to read a descriptor revision from',
        }
      : {
          available: true,
          value: { kind: 'metric_descriptor', value: cells[0].value.descriptor_revision },
        };

  if (model === null) {
    return card(
      dimension,
      INTEGRATION_MANDATE,
      metricsAbsence(metrics, 'integration-outcome cells'),
      rows,
      absentMetricsProvenance(metrics, 'observed native integrations'),
    );
  }

  if (authority === undefined) {
    const first = dimensionalCells[0] ?? cells[0];
    const reason: WorkChannel<never> =
      first !== undefined
        ? cellAbsence(first)
        : {
            available: false,
            state: 'unknown',
            detail: 'the projection carried no integration-outcome cells',
          };
    return card(
      dimension,
      INTEGRATION_MANDATE,
      reason,
      rows,
      unreadableCellProvenance(model, reason, descriptorRevision),
    );
  }

  const coverage = authority.value.coverage;
  const suppressed = dimensionalCells.length - readableCells.length;
  const suppressedNote =
    suppressed === 0
      ? ''
      : ` · ${suppressed} ${suppressed === 1 ? 'cell stays a typed absence' : 'cells stay typed absences'}`;
  return card(
    dimension,
    INTEGRATION_MANDATE,
    {
      available: true,
      value: `${coverage.observed} observed native integrations across ${readableCells.length} readable kind/outcome ${readableCells.length === 1 ? 'cell' : 'cells'}${suppressedNote} — counts are the projector's own cells, never summed here`,
    },
    rows,
    metricsProvenance(model, coverage, descriptorRevision, 'observed native integrations'),
  );
}

const STACK_CAPABILITY_MANDATE = 'GitHub stack capability state and generic-fallback availability';

/**
 * The latest trustworthy GitHub stacked-PR capability observation: a typed
 * operational state, not a count, so it is the headline and carries no
 * metered rows. A null field is unobserved, never coerced to off or on.
 * `WorkFallbackTopology` is the provider-executable fallback and is never
 * read into this card.
 */
export function githubStackCapabilityCard(
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined,
): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'github_stack_capability';
  const model = modelOf(metrics);

  if (model === null) {
    return card(
      dimension,
      STACK_CAPABILITY_MANDATE,
      metricsAbsence(metrics, 'the capability observation'),
      [],
      absentMetricsProvenance(metrics, 'capability observations'),
    );
  }

  const capability = model.github_stack_capability;
  const fallback = (value: boolean | null, name: string) =>
    value == null ? `${name} unobserved` : `${name} ${value ? 'available' : 'not available'}`;
  const reading: WorkChannel<string> =
    capability.capability == null
      ? {
          available: false,
          state: capability.unavailable === 'store_unavailable' ? 'unavailable' : 'unknown',
          detail: `no trustworthy capability observation exists in the horizon: ${humanizeMetric(capability.unavailable ?? 'the projector published no reason')}`,
        }
      : {
          available: true,
          value: `capability ${humanizeMetric(capability.capability)} · ${fallback(capability.standard_git_fallback_available, 'standard-git fallback')} · ${fallback(capability.other_forge_fallback_available, 'other-forge fallback')}`,
        };

  return card(
    dimension,
    STACK_CAPABILITY_MANDATE,
    reading,
    [],
    metricsProvenance(
      model,
      capability.coverage,
      {
        available: false,
        state: 'unknown',
        detail:
          'the GitHub stack capability reading is a model-level typed state and carries no metric descriptor revision',
      },
      'capability observations',
    ),
  );
}
