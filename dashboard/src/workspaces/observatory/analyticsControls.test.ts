import { describe, expect, it } from 'vitest';
import type { AnalyticsModeReadModelV1, MetricValueV1, StorageFindingKindStatusV1 } from '../../contracts/generated.ts';
import { analyticsModeReading, egressFailureReading, retentionBacklogReading, shareStagingReading, uploadSettingReading } from './analyticsControls.ts';

describe('analyticsModeReading', () => {
  it('reports an unavailable mode as unknown and explicitly not Off', () => {
    const reading = analyticsModeReading(mode(null, 'unknown', 'analytics_consent_not_observed'));
    expect(reading.mode).toBeNull();
    expect(reading.state).toBe('unknown');
    expect(reading.label).toBe('unavailable');
    // The specific falsification: an unread mode read back as "collection is off".
    expect(reading.label).not.toBe('Off');
    expect(reading.reason).toBe('analytics_consent_not_observed');
  });

  it('does not coerce an incomplete retained transition into a current mode', () => {
    expect(analyticsModeReading(mode('aggregate_share', 'partial', 'source_partial')).mode).toBeNull();
  });
});

describe('shareStagingReading', () => {
  it('reports an unpublished staging age as unsupported, never as zero', () => {
    const reading = shareStagingReading([observationMetric('analytics_share_staging_age_seconds', null)]);
    expect(reading.ageSeconds).toBeNull();
    expect(reading.ageSeconds).not.toBe(0);
    expect(reading.state).toBe('unknown');
    expect(reading.reason).toBe('not_observed');
  });
});

describe('egressFailureReading', () => {
  it('never reports zero failures for an exporter that does not exist', () => {
    const reading = egressFailureReading([observationMetric('analytics_egress_failures', null)]);
    expect(reading.failures).toBeNull();
    expect(reading.failures).not.toBe(0);
    expect(reading.state).toBe('unknown');
    expect(reading.reason).toBe('not_observed');
  });
});

describe('retentionBacklogReading', () => {
  it('reads a real retention-backlog status from the findings payload', () => {
    const reading = retentionBacklogReading([
      status('retention_backlog', 'real', 4, 'retention sweep observed 4 entries'),
      status('orphan_store', 'real', 0, 'no orphan stores'),
    ]);
    expect(reading.published).toBe(true);
    expect(reading.state).toBe('ready');
    expect(reading.observedEntries).toBe(4);
    expect(reading.reason).toBe('retention sweep observed 4 entries');
  });

  it('keeps a partial source state as partial rather than as a clean reading', () => {
    expect(retentionBacklogReading([status('retention_backlog', 'partial', 2, 'r')]).state).toBe(
      'partial',
    );
  });

  it('refuses to report an entry count for a kind this build does not support', () => {
    // `observed_entries` on an unsupported kind is not an observation of that
    // kind, so it does not become one here.
    const reading = retentionBacklogReading([status('retention_backlog', 'unsupported', 0, 'r')]);
    expect(reading.state).toBe('unsupported');
    expect(reading.observedEntries).toBeNull();
  });

  it('reports an absent status as unpublished rather than as zero entries', () => {
    const reading = retentionBacklogReading([status('orphan_store', 'real', 0, 'r')]);
    expect(reading.published).toBe(false);
    expect(reading.observedEntries).toBeNull();
    expect(reading.state).toBe('unsupported');
  });
});

describe('uploadSettingReading', () => {

  it('reports an unread setting as unknown rather than as disabled', () => {
    const reading = uploadSettingReading(undefined);
    expect(reading.enabled).toBeNull();
    expect(reading.enabled).not.toBe(false);
    expect(reading.state).toBe('unknown');
  });
});

function status(
  kind: StorageFindingKindStatusV1['kind'],
  state: StorageFindingKindStatusV1['state'],
  observedEntries: number,
  reason: string,
): StorageFindingKindStatusV1 {
  return { kind, state, observed_entries: observedEntries, reason };
}

function mode(
  current: AnalyticsModeReadModelV1['current'],
  state: AnalyticsModeReadModelV1['coverage']['state'],
  unavailableReason: string | null,
): AnalyticsModeReadModelV1 {
  return {
    current,
    transition_watermark: current == null ? null : 'producer:7',
    coverage: {
      eligible: state === 'known' ? 1 : null,
      observed: state === 'known' ? 1 : 0,
      completed: state === 'known' ? 1 : 0,
      censored: 0,
      unknown: state === 'known' ? 0 : 1,
      excluded: 0,
      state,
    },
    unavailable_reason: unavailableReason,
  };
}

function observationMetric(name: string, value: number | null): MetricValueV1 {
  return {
    descriptor_revision: 'analytics-controls.v1',
    metric: name,
    value,
    unit: name.includes('age') ? 'seconds' : 'events',
    denominator: 'analytics_observations',
    denominator_value: value == null ? null : 1,
    coverage: {
      eligible: value == null ? null : 1,
      observed: value == null ? 0 : 1,
      completed: value == null ? 0 : 1,
      censored: 0,
      unknown: value == null ? 1 : 0,
      excluded: 0,
      state: value == null ? 'unknown' : 'known',
    },
    evidence_class: 'measurement',
    provenance: {
      source: 'observability_envelope',
      source_revision: 'observability-envelope.v1',
      projector_revision: 'observatory-plan26-projector.v1',
      watermark: 'analytics:7',
    },
    cohort: { descriptor_revision: 'analytics.v1', eligible_population: 'analytics_observations' },
    temporal: {
      horizon: { since_micros: 1, until_micros: 2 },
      baseline_watermark: null,
      delta: null,
    },
    uncertainty: { lower: value, upper: value, reason: value == null ? 'not_observed' : null },
    calibration: null,
    unavailable_reason: value == null ? 'not_observed' : null,
  };
}
