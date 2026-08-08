/**
 * `analytics-controls` — the Plan 26 required view (§"Required product views":
 * "`analytics-controls` shows local mode, share staging age, retention/deletion,
 * and egress failures").
 *
 * THE MODES, EXACTLY AS THE PLAN DEFINES THEM
 *
 * Plan 26 §"Adoption analytics and retention": "`AnalyticsModeV1` remains
 * `Off | LocalOnly | AggregateShare`; `LocalOnly` is the default and
 * `AggregateShare` requires explicit opt-in. `Off` stops optional adoption
 * collection, `LocalOnly` has no network exporter, and opt-out stops egress
 * before its configuration operation succeeds. Ordinary bounded retention and
 * deletion apply to optional analytics; owning product receipts and run history
 * retain their existing lifecycles and are never exported as adoption
 * analytics."
 *
 * Two things in that paragraph are easy to render untruthfully and are handled
 * deliberately here.
 *
 * First, `LocalOnly` having no network exporter is a property of the *mode*,
 * not an observation about this installation. Saying "no data has left this
 * machine" would be a claim about the current mode, and the current mode is not
 * published (below). So the ladder describes what each mode is, and the current
 * mode is reported as unavailable.
 *
 * Second, product receipts and run history are NOT adoption analytics. This
 * surface therefore refuses to present the retention of savings ledgers,
 * sessions, or automation runs as an analytics retention figure, because Plan
 * 26 says those lifecycles are their own and are never exported as adoption
 * data. The one retention signal it does bind to is the Doctor
 * `retention_backlog` finding kind, which is a real, typed, wire-published
 * observation about retention work not being done.
 *
 * WHAT IS BEHIND THIS SURFACE TODAY
 *
 * - Current collection mode: NOT PUBLISHED. `AnalyticsModeV1` exists in the
 *   domain and `record_analytics_consent` records a transition (previous,
 *   current, share staging age) as an `AnalyticsConsentChangedV1` observation,
 *   but no configuration setting stores the mode and no read route projects it.
 * - Share staging age: NOT PUBLISHED, for the same reason — the field is on the
 *   consent observation, and nothing projects the observation.
 * - Retention/deletion: PARTIALLY PUBLISHED. `GET /api/storage/findings`
 *   carries a typed `retention_backlog` finding-kind status with its own source
 *   state and observed-entry count. The declared analytics lifetimes (30-day
 *   local detail, 395-day rollups, 24-hour share staging after opt-out, 30-day
 *   backups) are plan-declared policy, not measurements, and are labelled as
 *   such — no observed age is published.
 * - Egress failures: NOT PUBLISHED. There is no exporter in the default mode
 *   and no read route publishes egress attempts or failures. This must never
 *   render as "0 failures": zero failures and no exporter are different claims,
 *   and only one of them is true here.
 * - Profile upload setting: PUBLISHED. `GET /api/settings` carries the
 *   `user.upload_enabled.v1` profile setting. It is a real, editable, wire-
 *   published control and it is NOT `AnalyticsModeV1`; the view says so rather
 *   than letting proximity imply it governs adoption analytics.
 */
import type {
  SettingsPayloadV1,
  StorageFindingKindStatusV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

/** The closed mode set. Wire-cased to match `AnalyticsModeV1`'s serde. */
export type AnalyticsCollectionModeV1 = 'off' | 'local_only' | 'aggregate_share';

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
 * A `null` mode is `unsupported`, not `off`. Reading an absent mode as `Off`
 * would assert that collection is stopped, which is a claim about the running
 * system that nothing on the wire supports.
 */
export function analyticsModeReading(mode: AnalyticsCollectionModeV1 | null): ModeReading {
  if (mode == null) {
    return {
      mode: null,
      label: 'not published',
      state: 'unsupported',
      reason:
        'AnalyticsModeV1 is a domain type with no configuration setting and no read route; the current mode cannot be read, and an unread mode is not Off',
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

/**
 * The mode this dashboard can currently read.
 *
 * Deliberately a function returning `null` rather than a bare constant: when a
 * read route lands, this is the single place that changes, and every consumer
 * already handles the published case.
 */
export function publishedAnalyticsMode(): AnalyticsCollectionModeV1 | null {
  return null;
}

export interface DeclaredRetentionLifecycle {
  id: string;
  label: string;
  /** The declared lifetime, from Plan 26. Policy, never a measurement. */
  declared: string;
  /** The observed age, when one is published. Always `null` today. */
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
export function egressFailureReading(): EgressReading {
  return {
    failures: null,
    state: 'unsupported',
    reason:
      'no read route publishes egress attempts or failures. The default mode has no network exporter, so an absence of failures here is not a measurement of zero failures',
  };
}

export interface ShareStagingReading {
  /** Age of the oldest staged share packet, or `null` when unpublished. */
  ageSeconds: number | null;
  state: DomainStateKind;
  reason: string;
}

/** Share staging age. Recorded on the consent observation as
 * `share_staging_age_seconds`; projected by nothing. */
export function shareStagingReading(ageSeconds: number | null = null): ShareStagingReading {
  if (ageSeconds == null) {
    return {
      ageSeconds: null,
      state: 'unsupported',
      reason:
        'share_staging_age_seconds is recorded on AnalyticsConsentChangedV1, but no read route projects the consent observation',
    };
  }
  return { ageSeconds, state: 'ready', reason: '' };
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
