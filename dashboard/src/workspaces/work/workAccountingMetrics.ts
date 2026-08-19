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
 * The two Plan 24 integration/stack cards, fed from the mounted
 * `operation.work.topology_metrics` read.
 *
 * Work deliberately mounts no integration apply/review/stack mutation
 * operation: Plan 24 keeps accepted integration lowered only through the
 * Plan 36 native-integration family (typed preflight/approve/apply with
 * durable receipts on its own CLI/MCP surfaces). What the Work workspace owns
 * is the OBSERVED accounting of those receipts — the
 * `work.integration.transition.observed.v1` and
 * `work.github_stack_capability.observed.v1` events Plan 26 projects into
 * `ExecutionTopologyMetricsV1`. These builders decode that projection's own
 * cells and typed absences; nothing here derives a rate, sums a family, or
 * substitutes a policy-carried dimension for measured evidence.
 *
 * The metrics read is a horizon aggregate and is deliberately NOT bound to
 * the topology generation the structural cards join on: the Rust projector is
 * explicit that the two share a name family and nothing else, and joining
 * them would let a policy-carried dimension stand in for measured evidence.
 */

/** The exact Plan 26 descriptor the integration-outcome cells carry. */
export const MERGE_ATTEMPTS_METRIC = 'work_merge_attempts_total';

/** The metrics read's own reason, phrased for a channel. Kept local so this
 * module never invents a state the read did not report. */
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

function coverageSentence(coverage: MetricCoverageV1): string {
  return `${coverage.state} coverage · ${coverage.observed} observed · ${coverage.completed} completed`;
}

const ANCHORS_ABSENCE: WorkChannel<never> = {
  available: false,
  state: 'redacted',
  detail:
    'the metrics read publishes registered observation cursors, not task/run/attempt identities; drill-down resolves them only through the authorized local observability boundary',
};

/** The seven facets, decoded from one metric envelope's own coverage. The
 * descriptor revision is passed as a channel because not every reading carries
 * one: a card must state that absence rather than invent a revision. */
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
    intervalCoverage: { available: true, value: coverageSentence(coverage) },
    horizon: { available: true, value: horizonSentence(model) },
    descriptorRevision,
    anchors: ANCHORS_ABSENCE,
  };
}

/**
 * The facets for a model that answered but whose every cell is a typed
 * absence (support-floor suppression wipes the suppressed cell's own coverage
 * envelope, so its counts are not measurements to print). Each measured facet
 * carries the projector's own reason; the horizon and descriptor revision are
 * model-level facts suppression does not touch, so they stay real.
 */
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

const INTEGRATION_MANDATE = 'observed native fast-forward/merge/cherry-pick outcomes';

/**
 * Observed integration outcomes, cell by cell.
 *
 * Every row is one `work_merge_attempts_total` cell grouped by the
 * projector's own integration kind × outcome dimensions. No cell is summed:
 * the headline states the family's observed and eligible counts off the
 * decoded coverage envelope, never a total this module added up.
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
  // A cell whose value the projector suppressed (support floor, coverage
  // floor, no eligible evidence) is a typed absence, and its own coverage
  // envelope was wiped along with the value. Only a readable cell may lend
  // the card its headline and coverage authority.
  const readableCells = dimensionalCells.filter((measurement) => measurement.value.value != null);
  const authority = readableCells[0];

  const rows: WorkAccountingRow[] = dimensionalCells.map((measurement) => {
    const label = measurement.dimensions
      .map((cellDimension) => humanizeMetric(cellDimension.value))
      .join(' · ');
    const key = measurement.dimensions
      .map((cellDimension) => String(cellDimension.value))
      .join('_');
    return {
      key,
      label,
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
    };
  });

  // The exact descriptor revision every merge cell is stamped with —
  // suppression wipes a cell's value and coverage but never its revision, so
  // this stays real whenever any cell exists at all.
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
    return {
      dimension,
      title: accountingDimensionTitle(dimension),
      mandate: INTEGRATION_MANDATE,
      reading: metricsAbsence(metrics, 'integration-outcome cells'),
      rows,
      matrices: null,
      contradictions: [],
      provenance: absentMetricsProvenance(metrics, 'observed native integrations'),
    };
  }

  if (authority === undefined) {
    // Every cell is a typed absence (or none exists). The headline is the
    // projector's own reason, never an available reading over zero readable
    // cells.
    const reason: WorkChannel<never> =
      dimensionalCells[0] !== undefined
        ? cellAbsence(dimensionalCells[0])
        : cells[0] !== undefined
          ? cellAbsence(cells[0])
          : {
              available: false,
              state: 'unknown',
              detail: 'the projection carried no integration-outcome cells',
            };
    return {
      dimension,
      title: accountingDimensionTitle(dimension),
      mandate: INTEGRATION_MANDATE,
      reading: reason,
      rows,
      matrices: null,
      contradictions: [],
      provenance: unreadableCellProvenance(model, reason, descriptorRevision),
    };
  }

  const coverage = authority.value.coverage;
  const suppressed = dimensionalCells.length - readableCells.length;
  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate: INTEGRATION_MANDATE,
    reading: {
      available: true,
      value: `${coverage.observed} observed native integrations across ${readableCells.length} readable kind/outcome ${readableCells.length === 1 ? 'cell' : 'cells'}${suppressed > 0 ? ` · ${suppressed} ${suppressed === 1 ? 'cell stays a' : 'cells stay'} typed ${suppressed === 1 ? 'absence' : 'absences'}` : ''} — counts are the projector's own cells, never summed here`,
    },
    rows,
    matrices: null,
    contradictions: [],
    provenance: metricsProvenance(
      model,
      coverage,
      descriptorRevision,
      'observed native integrations',
    ),
  };
}

const STACK_CAPABILITY_MANDATE = 'GitHub stack capability state and generic-fallback availability';

/**
 * The latest trustworthy GitHub stacked-PR capability observation.
 *
 * A typed operational state, not a count, so it lives in the headline rather
 * than a metered row. A null field is stated as unobserved — the projector's
 * `None` means no trustworthy observation exists in the horizon, which is a
 * different fact from a fallback that is off.
 *
 * `WorkFallbackTopology` on the execution snapshot is the provider-EXECUTABLE
 * fallback (codex_cli or disabled) and looks like the thing this card wants;
 * it is never read into it. The generic-fallback figures here are the
 * projection's own standard-git and other-forge observations.
 */
export function githubStackCapabilityCard(
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined,
): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'github_stack_capability';
  const model = modelOf(metrics);
  const readingOf = (): WorkChannel<string> => {
    if (model === null) return metricsAbsence(metrics, 'the capability observation');
    const capability = model.github_stack_capability;
    if (capability.capability == null) {
      return {
        available: false,
        state: capability.unavailable === 'store_unavailable' ? 'unavailable' : 'unknown',
        detail: `no trustworthy capability observation exists in the horizon: ${humanizeMetric(capability.unavailable ?? 'the projector published no reason')}`,
      };
    }
    const fallback = (value: boolean | null, name: string) =>
      value == null ? `${name} unobserved` : `${name} ${value ? 'available' : 'not available'}`;
    return {
      available: true,
      value: `capability ${humanizeMetric(capability.capability)} · ${fallback(capability.standard_git_fallback_available, 'standard-git fallback')} · ${fallback(capability.other_forge_fallback_available, 'other-forge fallback')}`,
    };
  };

  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate: STACK_CAPABILITY_MANDATE,
    reading: readingOf(),
    // A capability state is not a countable figure, so this card carries no
    // metered rows; the whole observation is the headline sentence above.
    rows: [],
    matrices: null,
    contradictions: [],
    provenance:
      model === null
        ? absentMetricsProvenance(metrics, 'capability observations')
        : metricsProvenance(
            model,
            model.github_stack_capability.coverage,
            {
              // The capability reading is a typed operational state on the
              // model envelope, not a descriptor cell; it carries no metric
              // descriptor revision and this card does not invent one.
              available: false,
              state: 'unknown',
              detail:
                'the GitHub stack capability reading is a model-level typed state and carries no metric descriptor revision',
            },
            'capability observations',
          ),
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
