/**
 * ANALYTICS CONTROLS — the Plan 26 control surface for optional adoption
 * analytics: local mode, share staging age, retention/deletion, and egress
 * failures.
 *
 * Two real reads back parts of this surface — `GET /api/settings` for the
 * `user.upload_enabled.v1` profile setting, and `GET /api/storage/findings` for
 * the typed `retention_backlog` status. They are read independently: a failed
 * settings read must not blank the retention evidence, and neither may stand in
 * for the analytics collection mode, which nothing publishes.
 *
 * The rule this surface exists to hold is the one in `analyticsControls.ts`: an
 * unread mode is not `Off`, and an absent exporter is not zero egress failures.
 * Both would be reassuring, and both would be false.
 */
import type { ComponentProps, ReactNode } from 'react';
import { SettingsPayloadV1Schema } from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { useStorageFindings } from '../../data/query/storageFindings.ts';
import { envelopeReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip';
import {
  ANALYTICS_MODE_LADDER,
  DECLARED_RETENTION_LIFECYCLES,
  analyticsModeReading,
  egressFailureReading,
  publishedAnalyticsMode,
  retentionBacklogReading,
  shareStagingReading,
  uploadSettingReading,
  type RetentionBacklogReading,
  type UploadSettingReading,
} from './analyticsControls.ts';

export function AnalyticsControls() {
  const settings = useEnvelope(['settings', 'analytics-controls'], '/api/settings', SettingsPayloadV1Schema);
  const findings = useStorageFindings();

  const settingsState = envelopeReadState(settings.isPending, settings.data, {
    loading: 'requesting profile settings',
    transport: 'profile settings could not be read',
  });
  const findingsState = envelopeReadState(findings.isPending, findings.data, {
    loading: 'requesting doctor storage findings',
    transport: 'doctor storage findings could not be read',
  });

  const mode = analyticsModeReading(publishedAnalyticsMode());
  const staging = shareStagingReading();
  const egress = egressFailureReading();

  return (
    <section className="border-b border-edge-subtle" aria-label="Analytics controls">
      <h2 className="px-4 pt-4 text-sm font-semibold tracking-tight">Analytics controls</h2>
      <p className="px-4 pt-0.5 text-2xs text-text-muted">
        local mode, share staging age, retention and deletion, and egress failures for optional
        adoption analytics
      </p>

      <div className="flex flex-col gap-4 px-4 py-3">
        <Block
          title="collection mode"
          state={mode.state}
          detail={mode.label}
          reason={mode.reason}
          marker="collection_mode"
        >
          <ul className="flex flex-col gap-1.5" aria-label="Analytics collection modes">
            {ANALYTICS_MODE_LADDER.map((entry) => (
              <li
                key={entry.mode}
                className="flex min-w-0 flex-col gap-0.5 border-l-2 border-edge-subtle pl-2"
                data-analytics-mode={entry.mode}
                data-analytics-mode-current="unknown"
              >
                <span className="flex flex-wrap items-baseline gap-1.5">
                  <span className="td-value text-2xs text-text-primary">{entry.label}</span>
                  <span className="text-3xs text-text-muted">
                    {entry.exporter === 'none' ? 'no network exporter' : 'network exporter'}
                    {entry.isDefault ? ' · default' : ''}
                    {entry.requiresOptIn ? ' · explicit opt-in required' : ''}
                  </span>
                </span>
                <span className="text-3xs leading-snug text-text-muted">{entry.sentence}</span>
              </li>
            ))}
          </ul>
          <p className="text-3xs leading-snug text-text-muted">
            The three modes above describe the taxonomy, not this installation. None is marked
            current, because no read route reports which one is in force — and an unread mode is
            not <span className="td-value">Off</span>.
          </p>
        </Block>

        <Block
          title="share staging age"
          state={staging.state}
          detail="not published"
          reason={staging.reason}
          marker="share_staging"
        >
          <p className="text-3xs leading-snug text-text-muted">
            Plan 26 gives staged share data 24 hours after opt-out. No staged-packet age is
            published, so no age — including an age of zero — is shown.
          </p>
        </Block>

        <Block
          title="retention and deletion"
          state={
            findingsState.kind === 'ready'
              ? retentionBacklogReading(findingsState.value.payload.kind_statuses).state
              : findingsState.state
          }
          detail={findingsState.kind === 'ready' ? 'retention backlog' : findingsState.detail}
          reason={null}
          marker="retention"
        >
          {findingsState.kind === 'ready' ? (
            <RetentionBacklog
              reading={retentionBacklogReading(findingsState.value.payload.kind_statuses)}
            />
          ) : (
            <p className="text-3xs leading-snug text-text-muted">
              the retention-backlog finding could not be read, so nothing is stated about it
            </p>
          )}
          <dl
            className="flex flex-col gap-1 border-t border-edge-subtle pt-2 text-3xs leading-snug text-text-muted"
            data-analytics-retention="declared_policy"
            aria-label="Declared analytics retention policy"
          >
            {DECLARED_RETENTION_LIFECYCLES.map((lifecycle) => (
              <div key={lifecycle.id} className="flex min-w-0 gap-1.5">
                <dt className="shrink-0 uppercase tracking-[0.08em]">{lifecycle.label}</dt>
                <dd className="min-w-0 break-words text-text-secondary">
                  {lifecycle.declared} · observed age{' '}
                  {lifecycle.observedAge ?? 'not published'}
                </dd>
              </div>
            ))}
          </dl>
          <p className="text-3xs leading-snug text-text-muted">
            Those four lifetimes are declared policy, not measurements — no observed age is
            published for any of them. Product receipts and run history keep their own lifecycles
            and are never exported as adoption analytics, so their retention is not reported here.
          </p>
        </Block>

        <Block
          title="egress failures"
          state={egress.state}
          detail="not published"
          reason={egress.reason}
          marker="egress"
        >
          <p className="text-3xs leading-snug text-text-muted" data-egress-failures="unpublished">
            No failure count is shown. An absent exporter and an exporter that failed zero times
            are different readings, and only the first is true here.
          </p>
        </Block>

        <Block
          title="profile upload setting"
          state={
            settingsState.kind === 'ready'
              ? uploadSettingReading(settingsState.value.payload).state
              : settingsState.state
          }
          detail={settingsState.kind === 'ready' ? undefined : settingsState.detail}
          reason={null}
          marker="upload_setting"
        >
          {settingsState.kind === 'ready' ? (
            <UploadSetting reading={uploadSettingReading(settingsState.value.payload)} />
          ) : (
            <p className="text-3xs leading-snug text-text-muted">
              the profile settings read did not resolve, so the setting is not stated
            </p>
          )}
        </Block>
      </div>
    </section>
  );
}

function Block({
  title,
  state,
  detail,
  reason,
  marker,
  children,
}: {
  title: string;
  state: ComponentProps<typeof StateChip>['kind'];
  detail?: string | undefined;
  reason: string | null;
  marker: string;
  children: ReactNode;
}) {
  return (
    <section
      className="flex flex-col gap-2 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      aria-label={title}
      data-analytics-control={marker}
      data-analytics-control-state={state}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">{title}</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <StateChip kind={state} detail={detail} />
      </div>
      {reason != null ? (
        <p className="text-2xs leading-relaxed text-text-secondary">{reason}</p>
      ) : null}
      {children}
    </section>
  );
}

function RetentionBacklog({ reading }: { reading: RetentionBacklogReading }) {
  return (
    <dl
      className="flex flex-col gap-1 text-3xs leading-snug text-text-muted"
      data-retention-backlog-published={reading.published ? 'true' : 'false'}
    >
      <div className="flex min-w-0 gap-1.5">
        <dt className="shrink-0 uppercase tracking-[0.08em]">observed entries</dt>
        <dd className="min-w-0 break-words text-text-secondary tabular" data-cell="numeric">
          {reading.observedEntries == null
            ? 'not published'
            : reading.observedEntries.toLocaleString()}
        </dd>
      </div>
      <div className="flex min-w-0 gap-1.5">
        <dt className="shrink-0 uppercase tracking-[0.08em]">source</dt>
        <dd className="min-w-0 break-words text-text-secondary">{reading.reason}</dd>
      </div>
    </dl>
  );
}

function UploadSetting({ reading }: { reading: UploadSettingReading }) {
  return (
    <>
      <dl className="flex flex-col gap-1 text-3xs leading-snug text-text-muted">
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">setting</dt>
          <dd className="min-w-0 break-words text-text-secondary">{reading.settingKey}</dd>
        </div>
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">value</dt>
          <dd
            className="min-w-0 break-words text-text-secondary"
            data-upload-enabled={reading.enabled == null ? 'unknown' : String(reading.enabled)}
          >
            {reading.enabled == null ? 'not reported' : reading.enabled ? 'enabled' : 'disabled'}
          </dd>
        </div>
      </dl>
      <p className="text-3xs leading-snug text-text-muted">{reading.disclaimer}</p>
    </>
  );
}
