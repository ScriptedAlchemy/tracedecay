/** Plan 26 analytics controls over canonical settings and Observatory reads. */
import type {
  AnalyticsModeV1,
  AnalyticsModeReadModelV1,
  MetricValueV1,
  SettingsPayloadV1,
  StorageFindingKindStatusV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

export type AnalyticsCollectionModeV1 = AnalyticsModeV1;

export interface AnalyticsModeDescription {
  mode: AnalyticsCollectionModeV1;
  label: string;
  /** Whether this mode has a network exporter at all. */
  exporter: 'none' | 'network';
  /** Whether this is the default mode. */
  isDefault: boolean;
  /** Whether reaching this mode requires an explicit opt-in. */
  requiresOptIn: boolean;
  sentence: string;
}

/**
 * The three modes, described in the plan's own terms.
 *
 * This is a description of the taxonomy, not a reading of the installation. No
 * entry is ever marked "current" unless the wire says which one is.
 */
export const ANALYTICS_MODE_LADDER: readonly AnalyticsModeDescription[] = [
  {
    mode: 'off',
    label: 'Off',
    exporter: 'none',
    isDefault: false,
    requiresOptIn: false,
    sentence: 'Optional adoption collection stops. Nothing optional is recorded.',
  },
  {
    mode: 'local_only',
    label: 'Local only',
    exporter: 'none',
    isDefault: true,
    requiresOptIn: false,
    sentence:
      'The default mode. Optional analytics are collected and retained locally; this mode has no network exporter.',
  },
  {
    mode: 'aggregate_share',
    label: 'Aggregate share',
    exporter: 'network',
    isDefault: false,
    requiresOptIn: true,
    sentence:
      'Requires explicit opt-in. Only aggregate cells above the contribution floor may leave, and opting out stops egress before its configuration operation succeeds.',
  },
];

export interface ModeReading {
  /** The published mode, or `null` when nothing publishes one. */
  mode: AnalyticsCollectionModeV1 | null;
  label: string;
  state: DomainStateKind;
  reason: string | null;
}

/**
 * The current mode, or an honest statement that none is published.
 *
 * A `null` mode is unknown, not `off`. Reading an absent mode as `Off`
 * would assert that collection is stopped, which is a claim about the running
 * system that nothing on the wire supports.
 */
export function analyticsModeReading(read: AnalyticsModeReadModelV1): ModeReading {
  const mode = read.current;
  if (mode == null || read.coverage.state !== 'known') {
    return {
      mode: null,
      label: 'unavailable',
      state: 'unknown',
      reason: read.unavailable_reason ?? 'the canonical analytics-mode projection is unavailable',
    };
  }
  const described = ANALYTICS_MODE_LADDER.find((entry) => entry.mode === mode);
  return {
    mode,
    label: described?.label ?? mode,
    state: 'ready',
    reason: null,
  };
}

export interface DeclaredRetentionLifecycle {
  id: string;
  label: string;
  /** The declared lifetime, from Plan 26. Policy, never a measurement. */
  declared: string;
  /** The observed age when the applicable lifecycle authority publishes one. */
  observedAge: string | null;
}

/** The lifetimes Plan 26 declares for optional analytics. Declared policy — the
 * view labels them as such and never presents them as observed ages. */
export const DECLARED_RETENTION_LIFECYCLES: readonly DeclaredRetentionLifecycle[] = [
  {
    id: 'local_detail',
    label: 'optional local detail',
    declared: 'expires after 30 days',
    observedAge: null,
  },
  {
    id: 'local_rollups',
    label: 'local rollups',
    declared: 'expire after 395 days',
    observedAge: null,
  },
  {
    id: 'share_staging',
    label: 'share staging after opt-out',
    declared: 'expires within 24 hours',
    observedAge: null,
  },
  {
    id: 'backup_copies',
    label: 'backup copies',
    declared: 'expire within 30 days',
    observedAge: null,
  },
];

export interface RetentionBacklogReading {
  state: DomainStateKind;
  /** Entries the finding source actually observed, or `null` when the source
   * publishes no count. Never coerced to 0. */
  observedEntries: number | null;
  /** The source's own reason, verbatim. */
  reason: string;
  /** Whether the wire carried a retention-backlog status at all. */
  published: boolean;
}

/**
 * The one retention signal with a real read route behind it.
 *
 * `/api/storage/findings` publishes a typed status per Doctor storage finding
 * kind. `retention_backlog` says whether retention work is falling behind, and
 * its `state` distinguishes a real reading from a partial one from a kind this
 * build does not support — three different things that a single count would
 * flatten.
 */
export function retentionBacklogReading(
  statuses: readonly StorageFindingKindStatusV1[],
): RetentionBacklogReading {
  const status = statuses.find((candidate) => candidate.kind === 'retention_backlog');
  if (status === undefined) {
    return {
      state: 'unsupported',
      observedEntries: null,
      reason: 'the storage findings payload carried no retention-backlog status',
      published: false,
    };
  }
  return {
    state: findingSourceState(status.state),
    observedEntries: status.state === 'unsupported' ? null : status.observed_entries,
    reason: status.reason,
    published: true,
  };
}

/** The findings source state, in the shared domain-state vocabulary. `real` is
 * `ready` only in the sense that the source answered — it says nothing about
 * whether the backlog itself is healthy, and nothing here grades it. */
function findingSourceState(state: StorageFindingKindStatusV1['state']): DomainStateKind {
  switch (state) {
    case 'real':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'unsupported':
      return 'unsupported';
  }
}

export interface EgressReading {
  /** Failures observed, or `null` when nothing publishes them. Never 0. */
  failures: number | null;
  state: DomainStateKind;
  reason: string;
}

/**
 * Egress failures.
 *
 * The distinction this function exists to hold: "no exporter ran, so nothing
 * can have failed" is not "the exporter ran and failed zero times". Only the
 * second is a measurement, and nothing publishes it, so `failures` stays
 * `null` and the state is `unsupported`.
 */
export function egressFailureReading(metrics: readonly MetricValueV1[]): EgressReading {
  const metric = metrics.find((candidate) => candidate.metric === 'analytics_egress_failures');
  if (metric?.value != null) {
    return {
      failures: metric.value,
      state: 'ready',
      reason: '',
    };
  }
  return {
    failures: null,
    state: metric == null ? 'unsupported' : 'unknown',
    reason: metric?.unavailable_reason ?? 'the canonical egress-failure projection is absent',
  };
}

export interface ShareStagingReading {
  /** Age of the oldest staged share packet, or `null` when unpublished. */
  ageSeconds: number | null;
  state: DomainStateKind;
  reason: string;
}

/** Share staging age from the canonical Observatory metric. */
export function shareStagingReading(metrics: readonly MetricValueV1[]): ShareStagingReading {
  const metric = metrics.find(
    (candidate) => candidate.metric === 'analytics_share_staging_age_seconds',
  );
  if (metric?.value == null) {
    return {
      ageSeconds: null,
      state: metric == null ? 'unsupported' : 'unknown',
      reason: metric?.unavailable_reason ?? 'the canonical share-staging projection is absent',
    };
  }
  return { ageSeconds: metric.value, state: 'ready', reason: '' };
}

export interface UploadSettingReading {
  /** The profile setting value, or `null` when the settings read carried none. */
  enabled: boolean | null;
  settingKey: string;
  state: DomainStateKind;
  /** The one sentence that keeps this from being mistaken for the analytics
   * collection mode. */
  disclaimer: string;
}

/** The `user.upload_enabled.v1` profile setting from `/api/settings`. Real,
 * wire-published, and explicitly not `AnalyticsModeV1`. */
export function uploadSettingReading(
  settings: SettingsPayloadV1 | undefined,
): UploadSettingReading {
  const enabled = settings?.user.upload_enabled ?? null;
  return {
    enabled,
    settingKey: 'user.upload_enabled.v1',
    state: enabled == null ? 'unknown' : 'ready',
    disclaimer:
      'This is the profile upload setting, not the Plan 26 analytics collection mode. It does not select Off, LocalOnly, or AggregateShare and does not govern adoption analytics.',
  };
}
