/**
 * The Settings inspector: the exact evidence behind the inspected row, then
 * the identity of the configuration snapshot every row came from, then the two
 * operational planes — multi-root and Remote Brain — that sit beside
 * configuration without being configuration values.
 *
 * Inspection is a reading. It follows hover and focus, changes nothing, and
 * states only what the wire stated: a provenance the API did not serve is
 * printed as `unserved`, an origin it did not name as not served.
 */

import type { ReactNode } from 'react';
import type {
  CodeIndexWorkerStatusV1,
  DashboardEnvelopeV1,
  SettingsPayloadV1,
} from '../../contracts/generated.ts';
import type { ScopeWritability } from '../../data/scope/store.ts';
import { Legend } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { MultiRootPanel } from './MultiRootPanel.tsx';
import { RemoteBrainPanel } from './RemoteBrainPanel.tsx';
import { settingsReviewOf, type SettingsEditorState } from './settingsEditorMachine.ts';
import type { WritableScopes } from './settingsGates.ts';
import { settingsRevisionId, type SettingsModel } from './settingsModel.ts';
import {
  applyRequirement,
  bindingFor,
  fieldEdited,
  scopeNoun,
  writeCapability,
  type EffectiveRow,
} from './settingsRows.ts';
import {
  ApplyRequirementText,
  ORIGIN_WORD,
  ProvenanceChip,
  ValueCell,
  WriteCell,
  provenanceSentence,
  reviewStatusWord,
  type RowProvenance,
} from './SettingsValues.tsx';

export function SettingsInspector({
  envelope,
  model,
  row,
  gates,
  state,
  readUrl,
  writability,
  workerStatus,
}: {
  envelope: DashboardEnvelopeV1<SettingsPayloadV1>;
  model: SettingsModel;
  row: EffectiveRow | null;
  gates: WritableScopes;
  state: SettingsEditorState;
  readUrl: string;
  writability: ScopeWritability;
  workerStatus: CodeIndexWorkerStatusV1 | null;
}) {
  return (
    <div className="flex flex-col">
      <section aria-label="Provenance / review" className="border-b border-edge-subtle">
        <header className="flex min-h-10 items-center gap-2.5 border-b border-edge-subtle px-2.5 py-2">
          <span className="flex min-w-0 flex-col gap-0.5">
            <span className="text-2xs uppercase tracking-[0.08em] text-text-muted">
              {row ? 'inspecting' : 'configuration snapshot'}
            </span>
            <h2 className="td-title truncate">Provenance / review</h2>
          </span>
          <span aria-hidden className="td-rule" />
        </header>
        <div className="flex flex-col gap-4 p-2.5">
          {row ? (
            <RowInspection row={row} gates={gates} state={state} workerStatus={workerStatus} />
          ) : null}
          <SnapshotFacts
            envelope={envelope}
            model={model}
            readUrl={readUrl}
            writability={writability}
            state={state}
            emphasized={row === null}
          />
        </div>
      </section>
      <MultiRootPanel />
      <RemoteBrainPanel />
    </div>
  );
}

function RowInspection({
  row,
  gates,
  state,
  workerStatus,
}: {
  row: EffectiveRow;
  gates: WritableScopes;
  state: SettingsEditorState;
  workerStatus: CodeIndexWorkerStatusV1 | null;
}) {
  const binding = bindingFor(row.key);
  const capability = writeCapability(row.key, gates, state.status !== 'editor_unavailable');
  const edited =
    binding !== null &&
    state.status !== 'editor_unavailable' &&
    fieldEdited(state.draft, state.authority, binding);
  const provenance: RowProvenance = edited ? 'edited' : row.row.provenance;
  const revision =
    binding && state.status !== 'editor_unavailable'
      ? settingsRevisionId(state.authority, binding.scope)
      : null;
  return (
    <section aria-label={`Inspecting ${row.key}`} data-inspected-key={row.key} className="flex flex-col gap-3">
      <dl className="flex flex-col gap-2.5">
        <Fact term="key">
          <span className="td-value break-all text-2xs text-text-primary">{row.key}</span>
        </Fact>
        <Fact term="section">
          <span className="text-2xs text-text-secondary">
            {row.section.title}
            <span className="text-text-muted"> · {ORIGIN_WORD[row.section.origin]}</span>
          </span>
        </Fact>
        <Fact term="effective value">
          <ValueCell row={row.row} query="" />
          <span className="mt-0.5 block text-3xs text-text-muted">kind · {row.row.kind}</span>
        </Fact>
        {row.row.description ? (
          <Fact term="described by the daemon">
            <span className="text-2xs leading-relaxed text-text-secondary">{row.row.description}</span>
          </Fact>
        ) : null}
        <Fact term="provenance">
          <ProvenanceChip kind={provenance} />
          <span className="mt-1 block text-3xs leading-relaxed text-text-muted">
            {provenanceSentence(provenance)}
          </span>
        </Fact>
        <Fact term="origin">
          {row.section.location ? (
            <span className="td-value break-all text-2xs text-text-secondary">{row.section.location}</span>
          ) : (
            <span className="text-2xs text-text-muted">
              not served — the payload names no source for this group
            </span>
          )}
        </Fact>
        <Fact term="write">
          <WriteCell capability={capability} />
          <span className="mt-1 block text-3xs leading-relaxed text-text-muted">
            {writeSentence(capability, workerStatus)}
          </span>
        </Fact>
        {binding ? (
          <Fact term="apply requirement">
            <ApplyRequirementText requirement={applyRequirement(binding)} />
          </Fact>
        ) : null}
        {revision !== null && binding ? (
          <Fact term={`${scopeNoun(binding.scope)} revision (cas)`}>
            <span className="td-value break-all text-2xs text-text-secondary">{revision}</span>
          </Fact>
        ) : null}
      </dl>
    </section>
  );
}

function writeSentence(
  capability: ReturnType<typeof writeCapability>,
  workerStatus: CodeIndexWorkerStatusV1 | null,
): string {
  switch (capability.kind) {
    case 'writable':
      return capability.binding.scope === 'code_index_workers' && workerStatus === null
        ? `Applies to ${capability.target}. Current admission limits are unavailable, so an exact count is judged at restart.`
        : `Applies to ${capability.target} through the compare-and-swap write path.`;
    case 'locked':
      return capability.reason;
    case 'no_write_path':
      return 'No settings PATCH route addresses this key; it is reported, not edited, here.';
    default: {
      const exhaustive: never = capability;
      return exhaustive;
    }
  }
}

/**
 * The identity of the snapshot on screen: where it was read, when the daemon
 * observed it, the compare-and-swap revision of each independently revisioned
 * resource, and what the current scope permits.
 */
function SnapshotFacts({
  envelope,
  model,
  readUrl,
  writability,
  state,
  emphasized,
}: {
  envelope: DashboardEnvelopeV1<SettingsPayloadV1>;
  model: SettingsModel;
  readUrl: string;
  writability: ScopeWritability;
  state: SettingsEditorState;
  emphasized: boolean;
}) {
  const { payload, freshness } = envelope;
  return (
    <section aria-label="Configuration snapshot" className="flex flex-col gap-3">
      {!emphasized ? <Legend>Configuration snapshot</Legend> : null}
      <dl className="flex flex-col gap-2.5">
        <Fact term="config source">
          <span className="td-value break-all text-2xs text-text-secondary">GET {readUrl}</span>
          <span className="mt-0.5 block text-3xs text-text-muted">
            effective-only · per-key layers are not on this wire
          </span>
        </Fact>
        <Fact term="evaluated at">
          <span className="td-value text-2xs text-text-secondary">
            {freshness.observed_at_micros != null
              ? formatMicrosUtc(freshness.observed_at_micros)
              : 'not stated'}
          </span>
          <span className="mt-1 block">
            <StateChip kind={freshnessKind(freshness.state)} detail={`freshness ${freshness.state}`} />
          </span>
        </Fact>
        <Fact term="config revision (cas)">
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-0.5">
            <RevisionLine label="project" value={payload.project.configuration_revision_id} />
            <RevisionLine label="user" value={payload.user.configuration_revision_id} />
            <RevisionLine
              label="workers"
              value={payload.user.code_index_worker_configuration_revision_id}
            />
          </dl>
        </Fact>
        <Fact term="snapshot">
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-0.5">
            <RevisionLine label="project" value={payload.project.configuration_snapshot_id} />
            <RevisionLine label="user" value={payload.user.configuration_snapshot_id} />
            <RevisionLine
              label="workers"
              value={payload.user.code_index_worker_configuration_snapshot_id}
            />
          </dl>
        </Fact>
        <Fact term="scope">
          <ScopeFact writability={writability} />
        </Fact>
        <HeldReviewFact state={state} />
        {model.overrides.length > 0 ? (
          <Fact term="environment overrides">
            <span className="td-value text-2xs text-text-secondary" data-cell="numeric">
              {model.activeOverrides} of {model.overrides.length} in force
            </span>
          </Fact>
        ) : null}
        <Fact term="concurrent edit detection">
          <span className="text-2xs leading-relaxed text-text-muted">
            A proposal is validated against the revision it was planned from and checked again
            immediately before apply. If the revision changes first, the patch is rejected as a
            conflict and nothing is written.
          </span>
        </Fact>
      </dl>
    </section>
  );
}

/**
 * The one review the editor can hold, stated here so a verdict — a conflict,
 * a withdrawn authority, a failed write — stays visible after its row's panel
 * is closed, rather than surviving only as a word in the register.
 */
function HeldReviewFact({ state }: { state: SettingsEditorState }) {
  const review = settingsReviewOf(state);
  if (review === null) return null;
  return (
    <Fact term="held review">
      <span className="text-2xs leading-relaxed text-text-secondary" data-settings-held-review={state.status}>
        <span className="td-value text-text-primary">{reviewStatusWord(state.status)}</span> ·{' '}
        {scopeNoun(review.scope)} change against revision{' '}
        <span className="td-value">{review.expectedRevisionId}</span>. Select a{' '}
        {scopeNoun(review.scope)} key to resolve it.
      </span>
    </Fact>
  );
}

function ScopeFact({ writability }: { writability: ScopeWritability }) {
  switch (writability.state) {
    case 'writable':
      return (
        <span className="text-2xs text-text-secondary" data-settings-scope="writable">
          <span className="td-value text-text-primary">writable</span> · writes land on{' '}
          {writability.target}
        </span>
      );
    case 'read_only':
      return (
        <span className="text-2xs leading-relaxed text-text-secondary" data-settings-scope="read_only">
          <span className="td-value text-state-locked">read-only</span> · {writability.reason}
        </span>
      );
    case 'unknown':
      return (
        <span className="text-2xs leading-relaxed text-text-secondary" data-settings-scope="unknown">
          <span className="td-value text-state-unknown">unknown</span> · {writability.reason}
        </span>
      );
    default: {
      const exhaustive: never = writability;
      return exhaustive;
    }
  }
}

function freshnessKind(state: DashboardEnvelopeV1<unknown>['freshness']['state']) {
  switch (state) {
    case 'fresh':
      return 'ready' as const;
    case 'stale':
      return 'stale' as const;
    case 'absent':
      return 'unavailable' as const;
    case 'unknown':
      return 'unknown' as const;
    case 'unsupported':
      return 'unsupported' as const;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

function RevisionLine({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="td-legend pt-px">{label}</dt>
      <dd className="td-value min-w-0 break-all text-2xs text-text-secondary">
        {value.length > 0 ? value : <span className="text-text-muted">not stated</span>}
      </dd>
    </>
  );
}

function Fact({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <dt className="td-legend">{term}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}
