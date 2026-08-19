/** `ExecutionTopologyMetricsV1` fixtures, shaped like the Rust projector's
 * output; consumers parse them with the generated schema. */

const HORIZON = {
  since_micros: 1_753_000_000_000_000,
  until_micros: 1_753_003_600_000_000,
};

interface CoverageSpec {
  eligible?: number | null;
  observed?: number;
  completed?: number;
  censored?: number;
  unknown?: number;
  state?: string;
}

function coverage(spec: CoverageSpec = {}) {
  return {
    eligible: spec.eligible ?? null,
    observed: spec.observed ?? 0,
    completed: spec.completed ?? 0,
    censored: spec.censored ?? 0,
    unknown: spec.unknown ?? 0,
    excluded: 0,
    state: spec.state ?? 'unknown',
  };
}

export function topologyMeasurement(spec: {
  metric: string;
  value: number | null;
  unit: string;
  denominator: string;
  dimensions: readonly { dimension: string; value: string }[];
  coverage?: CoverageSpec;
  unavailable?: string;
}) {
  const unavailable = spec.unavailable ?? null;
  const cellCoverage = coverage(spec.coverage);
  return {
    dimensions: spec.dimensions,
    unavailable,
    value: {
      descriptor_revision: 'execution-topology-metrics.v1',
      metric: spec.metric,
      value: spec.value,
      unit: spec.unit,
      denominator: spec.denominator,
      denominator_value: cellCoverage.eligible,
      coverage: cellCoverage,
      evidence_class: 'measurement',
      provenance: {
        source: 'observability_envelope',
        source_revision: 'observability-envelope.v1',
        projector_revision: 'execution-topology-projector.v1',
        watermark: 'observability:topology:41',
      },
      cohort: {
        descriptor_revision: `${spec.denominator}.v1`,
        eligible_population: spec.denominator,
      },
      temporal: { horizon: HORIZON, baseline_watermark: null, delta: null },
      uncertainty: { lower: spec.value, upper: spec.value, reason: unavailable },
      calibration: null,
      unavailable_reason: unavailable,
    },
  };
}

export interface TopologyMetricsSpec {
  measurements?: readonly ReturnType<typeof topologyMeasurement>[];
  githubStackCapability?: {
    capability: string | null;
    standard_git_fallback_available: boolean | null;
    other_forge_fallback_available: boolean | null;
    coverage?: CoverageSpec;
    unavailable?: string | null;
  };
  coverage?: CoverageSpec;
}

export function topologyMetricsModel(spec: TopologyMetricsSpec = {}) {
  const capability = spec.githubStackCapability;
  return {
    authorized_scope_ref: 'project.tracedecay',
    horizon: HORIZON,
    watermark: 'observability:topology:41',
    observed_at_micros: HORIZON.until_micros,
    current: true,
    coverage: coverage(spec.coverage ?? { observed: 9, completed: 9, state: 'known' }),
    emission_coverage: { emitted: 9, delayed: 0, dropped: 0, sampled_events: 0 },
    github_stack_capability: {
      capability: capability?.capability ?? null,
      standard_git_fallback_available: capability?.standard_git_fallback_available ?? null,
      other_forge_fallback_available: capability?.other_forge_fallback_available ?? null,
      coverage: coverage(capability?.coverage ?? { unknown: 1 }),
      unavailable:
        capability === undefined ? 'no_eligible_evidence' : (capability.unavailable ?? null),
    },
    drill_anchors: [{ cursor: 'topology-observation-41' }],
    measurements: spec.measurements ?? [],
  };
}
