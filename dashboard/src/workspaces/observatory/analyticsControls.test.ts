import { describe, expect, it } from 'vitest';
import type {
  AnalyticsModeReadModelV1,
  MetricValueV1,
  SettingsPayloadV1,
  StorageFindingKindStatusV1,
} from '../../contracts/generated.ts';
import {
  ANALYTICS_MODE_LADDER,
  DECLARED_RETENTION_LIFECYCLES,
  analyticsModeReading,
  egressFailureReading,
  retentionBacklogReading,
  shareStagingReading,
  uploadSettingReading,
} from './analyticsControls.ts';

/**
 * Plan 26 §"Adoption analytics and retention" defines three modes and one
 * default. The tests below pin the two claims this surface could most easily
 * make falsely: that an unread mode is `Off`, and that an absent exporter means
 * zero egress failures. Neither is true, and neither is representable here.
 */

describe('analytics mode ladder', () => {
  it('carries exactly the three modes the plan defines', () => {
    expect(ANALYTICS_MODE_LADDER.map((entry) => entry.mode)).toEqual([
      'off',
      'local_only',
      'aggregate_share',
    ]);
  });

  it('marks local-only as the default with no network exporter', () => {
    const local = ANALYTICS_MODE_LADDER.find((entry) => entry.mode === 'local_only');
    expect(local?.isDefault).toBe(true);
    expect(local?.exporter).toBe('none');
    expect(local?.requiresOptIn).toBe(false);
  });

  it('marks aggregate share as the only mode with an exporter and the only opt-in', () => {
    const withExporter = ANALYTICS_MODE_LADDER.filter((entry) => entry.exporter === 'network');
    expect(withExporter.map((entry) => entry.mode)).toEqual(['aggregate_share']);
    const optIn = ANALYTICS_MODE_LADDER.filter((entry) => entry.requiresOptIn);
    expect(optIn.map((entry) => entry.mode)).toEqual(['aggregate_share']);
  });

  it('says opting out stops egress before its configuration operation succeeds', () => {
    const share = ANALYTICS_MODE_LADDER.find((entry) => entry.mode === 'aggregate_share');
    expect(share?.sentence).toContain('before its configuration operation succeeds');
  });

  it('records exactly one default mode', () => {
    expect(ANALYTICS_MODE_LADDER.filter((entry) => entry.isDefault)).toHaveLength(1);
  });
});

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

  it('reports a published mode by its own label', () => {
    const reading = analyticsModeReading(mode('aggregate_share', 'known', null));
    expect(reading.state).toBe('ready');
    expect(reading.label).toBe('Aggregate share');
    expect(reading.reason).toBeNull();
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

  it('reports a published age when one is supplied', () => {
    expect(
      shareStagingReading([observationMetric('analytics_share_staging_age_seconds', 3_600)]),
    ).toMatchObject({ ageSeconds: 3_600, state: 'ready' });
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

describe('declared retention lifecycles', () => {
  it('carries the four Plan 26 lifetimes with no observed age', () => {
    expect(DECLARED_RETENTION_LIFECYCLES.map((entry) => entry.id)).toEqual([
      'local_detail',
      'local_rollups',
      'share_staging',
      'backup_copies',
    ]);
    // Declared policy is not a measurement, and nothing here pretends it is.
    for (const entry of DECLARED_RETENTION_LIFECYCLES) {
      expect(entry.observedAge).toBeNull();
    }
  });
});

describe('uploadSettingReading', () => {
  it('reads the profile upload setting from the settings payload', () => {
    const reading = uploadSettingReading(settings(true));
    expect(reading.enabled).toBe(true);
    expect(reading.settingKey).toBe('user.upload_enabled.v1');
    expect(reading.state).toBe('ready');
  });

  it('reports an unread setting as unknown rather than as disabled', () => {
    const reading = uploadSettingReading(undefined);
    expect(reading.enabled).toBeNull();
    expect(reading.enabled).not.toBe(false);
    expect(reading.state).toBe('unknown');
  });

  it('states that it is not the analytics collection mode', () => {
    // Proximity on the surface must not imply this setting selects Off,
    // LocalOnly, or AggregateShare.
    const { disclaimer } = uploadSettingReading(settings(false));
    expect(disclaimer).toContain('not the Plan 26 analytics collection mode');
    expect(disclaimer).toContain('does not govern adoption analytics');
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

function settings(uploadEnabled: boolean): SettingsPayloadV1 {
  return {
    user: {
      configuration_snapshot_id: 'snapshot-1',
      configuration_revision_id: 'revision-1',
      upload_enabled: uploadEnabled,
      watcher_debounce: '500ms',
      extraction_timeout_secs: 30,
      installed_agents: ['claude'],
    },
  } as SettingsPayloadV1;
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
