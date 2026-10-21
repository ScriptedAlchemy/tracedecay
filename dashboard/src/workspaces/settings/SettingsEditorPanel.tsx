/**
 * The editor for one selected key, drawn inline under its row.
 *
 * It renders the current draft and reports edits and Save intent to the
 * controller. The effective value remains authoritative until the write is
 * applied and read back.
 */

import { Lock, X } from 'lucide-react';
import type { ReactNode } from 'react';
import type { CodeIndexWorkerStatusV1 } from '../../contracts/generated.ts';
import { Corners } from '../../ui/instrument.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { secondarySettingsButtonClass, settingsButtonClass } from './settingsChrome.ts';
import type { SettingsEditorHandle } from './SettingsEditorController.tsx';
import {
  settingsApplied,
  settingsRejection,
  settingsScopePlan,
  settingsSubmissionOf,
  type SettingsEditorState,
} from './settingsEditorMachine.ts';
import type { SettingsScope, SettingsValidationError } from './settingsModel.ts';
import { SettingsRowEditor } from './SettingsRowEditor.tsx';
import {
  applyRequirement,
  draftValue,
  fieldEdited,
  scopeNoun,
  type EffectiveRow,
  type SettingsBinding,
  type WriteCapability,
} from './settingsRows.ts';
import {
  ApplyRequirementText,
  ORIGIN_WORD,
  ValueCell,
  saveStatusWord,
} from './SettingsValues.tsx';

export function SettingsEditorPanel({
  row,
  capability,
  editor,
  workerStatus,
  query,
  closable,
  onClose,
}: {
  row: EffectiveRow;
  capability: WriteCapability;
  editor: SettingsEditorHandle;
  workerStatus: CodeIndexWorkerStatusV1 | null;
  query: string;
  /** False while a write is in flight so its outcome stays visible. */
  closable: boolean;
  onClose: () => void;
}) {
  return (
    <section
      aria-label={`Settings editor for ${row.key}`}
      data-settings-editor={row.key}
      className="relative m-2 border border-edge-strong bg-surface-1 td-raised"
    >
      <Corners tone={capability.kind === 'writable' ? 'signal' : 'edge'} />
      <header className="flex min-h-9 items-center gap-2.5 border-b border-edge-subtle px-3">
        <h3 className="td-title truncate">
          {capability.kind === 'writable' ? 'Edit' : 'Inspect'}
          <span className="ml-2 font-mono normal-case tracking-normal text-text-primary">
            {row.key}
          </span>
        </h3>
        <span aria-hidden className="td-rule" />
        <button
          type="button"
          onClick={onClose}
          disabled={!closable}
          aria-label="Close editor"
          className="td-hit group -my-2 -mr-2 shrink-0 disabled:cursor-wait disabled:opacity-50"
        >
          <span className="inline-flex size-6 items-center justify-center text-text-muted group-hover:text-text-primary">
            <X aria-hidden size={14} />
          </span>
        </button>
      </header>
      <div className="grid gap-3 p-3">
        <PanelBody
          row={row}
          capability={capability}
          editor={editor}
          workerStatus={workerStatus}
          query={query}
        />
      </div>
    </section>
  );
}

function PanelBody({
  row,
  capability,
  editor,
  workerStatus,
  query,
}: {
  row: EffectiveRow;
  capability: WriteCapability;
  editor: SettingsEditorHandle;
  workerStatus: CodeIndexWorkerStatusV1 | null;
  query: string;
}) {
  switch (capability.kind) {
    case 'no_write_path':
      return (
        <>
          <Readouts>
            <Readout label="effective value">
              <ValueCell row={row.row} query={query} />
            </Readout>
            <Readout label="origin">
              <OriginText row={row} />
            </Readout>
            <Readout label="write">
              <span className="td-value text-sm text-text-muted">no write path</span>
            </Readout>
          </Readouts>
          <p className="text-body leading-relaxed text-text-muted" data-settings-gate="no_write_path">
            <span className="text-text-secondary">Not editable here.</span> Change this value in
            the configuration source that owns it.
          </p>
        </>
      );
    case 'locked':
      return (
        <>
          <Readouts>
            <Readout label="effective value">
              <ValueCell row={row.row} query={query} />
            </Readout>
            <Readout label="target layer">
              <span className="td-value text-sm text-text-secondary">
                {scopeNoun(capability.binding.scope)}
              </span>
            </Readout>
            <Readout label="write">
              <span className="inline-flex items-center gap-1 text-body text-state-locked">
                <Lock aria-hidden size={11} />
                locked · {capability.gate.replace('_', ' ')}
              </span>
            </Readout>
          </Readouts>
          <p
            className="text-body leading-relaxed text-text-muted"
            data-settings-gate={capability.gate}
          >
            <span className="text-text-secondary">Read-only · </span>
            {capability.reason}
          </p>
          <HeldSubmissionUnderLock scope={capability.binding.scope} editor={editor} />
        </>
      );
    case 'writable':
      return (
        <WritableBody
          row={row}
          binding={capability.binding}
          target={capability.target}
          editor={editor}
          workerStatus={workerStatus}
          query={query}
        />
      );
    default: {
      const exhaustive: never = capability;
      return exhaustive;
    }
  }
}

/**
 * A save that outlived its gate remains visible, but a locked scope can only
 * discard the old draft and reload current values.
 */
function HeldSubmissionUnderLock({
  scope,
  editor,
}: {
  scope: SettingsScope;
  editor: SettingsEditorHandle;
}) {
  const submission = settingsSubmissionOf(editor.state);
  if (submission === null || submission.scope !== scope) return null;
  return (
    <div className="flex flex-wrap items-center justify-between gap-2" data-settings-save={editor.state.status}>
      <span className="text-body text-text-muted">
        This {scopeNoun(scope)} save is {saveStatusWord(editor.state.status)}. This setting is now
        read-only here; reload current values before editing it elsewhere.
      </span>
      <button
        type="button"
        className={secondarySettingsButtonClass}
        disabled={editor.state.status === 'submitting'}
        onClick={editor.reload}
      >
        Load current values
      </button>
    </div>
  );
}

/* ------------------------------------------------------------ writable --*/

function WritableBody({
  row,
  binding,
  target,
  editor,
  workerStatus,
  query,
}: {
  row: EffectiveRow;
  binding: SettingsBinding;
  target: string;
  editor: SettingsEditorHandle;
  workerStatus: CodeIndexWorkerStatusV1 | null;
  query: string;
}) {
  const { state } = editor;
  if (state.status === 'editor_unavailable') {
    return (
      <p className="text-xs text-state-error" data-settings-gate="editor_unavailable">
        Settings cannot be edited until the current configuration and its revision are available.
        Reload settings and try again.
      </p>
    );
  }
  const scope = binding.scope;
  const submission = settingsSubmissionOf(state);
  const scopeSubmission = submission?.scope === scope ? submission : null;
  const plan = settingsScopePlan(state, scope);
  const rejection = settingsRejection(state);
  const scopeErrors = rejection?.scope === scope ? rejection.errors : [];
  // The input wears whichever refusal names its field: a verdict the editor is
  // resting on first, else the live plan's, so `aria-invalid` is true the
  // moment the value would be refused rather than only after Save.
  // (An edit clears a resting refusal in the machine, so the two never
  // describe different drafts.)
  const liveErrors = plan?.outcome === 'invalid' ? plan.errors : [];
  const fieldError = [...scopeErrors, ...liveErrors].find(
    (error) => error.field === binding.field,
  )?.message;
  const applied = settingsApplied(state);
  const proposed = draftValue(state.draft, binding);
  const edited = fieldEdited(state.draft, state.authority, binding);
  const requirement = applyRequirement(binding);
  const savePending = state.status !== 'editing';
  const otherSubmission = submission !== null && scopeSubmission === null ? submission.scope : null;

  return (
    <>
      <Readouts>
        <Readout label="effective value">
          <ValueCell row={row.row} query={query} />
          <span className="mt-0.5 block text-sm text-text-muted">authoritative</span>
        </Readout>
        <Readout label="value">
          <SettingsRowEditor
            binding={binding}
            value={proposed}
            label={`Edit ${row.key}`}
            error={fieldError}
            disabled={savePending}
            workerStatus={workerStatus}
            onChange={(value) => editor.edit(binding, value)}
          />
        </Readout>
        <Readout label="validation">
          <Validation plan={plan} errors={scopeErrors} edited={edited} rejection={rejection?.scope === scope ? rejection : null} />
        </Readout>
        <Readout label="target layer">
          <span className="td-value text-sm text-text-primary">{scopeNoun(scope)}</span>
          <span className="mt-0.5 block text-sm text-text-muted" data-settings-gate="writable">
            applies to {target}
          </span>
        </Readout>
        <Readout label="apply requirement">
          <ApplyRequirementText requirement={requirement} />
        </Readout>
      </Readouts>

      {otherSubmission ? (
        <p className="text-body text-text-muted" data-settings-other-save={state.status}>
          A {scopeNoun(otherSubmission)} save is {saveStatusWord(state.status)}. This draft is
          unchanged.
        </p>
      ) : null}

      {/* The receipt describes the last write; once a new proposal exists for
        * this scope it describes something other than what is on screen, so
        * it yields to the proposal rather than sitting above it. */}
      {applied?.scope === scope && plan?.outcome === 'unchanged' ? (
        <p
          role="status"
          data-settings-receipt={scope}
          className="flex flex-wrap items-center gap-x-3 gap-y-1 border border-state-ready/40 bg-surface-0 px-3 py-2 text-xs text-text-secondary"
        >
          <StateChip kind="ready" detail="applied" />
          <strong className="font-semibold text-text-primary">{applied.message}</strong>
          <span className="td-value text-sm text-text-muted">
            revision now {applied.revisionId}
          </span>
          {applied.resyncRecommended ? <span>Resync recommended</span> : null}
          {applied.restartRecommended ? <span>Restart recommended</span> : null}
          {applied.effectReconciliation === 'pending' ? <span>Receipt reconciliation pending</span> : null}
          {applied.effectReconciliation === 'failed' ? <span>Receipt reconciliation failed</span> : null}
        </p>
      ) : null}

      {rejection?.origin === 'server' && rejection.scope === scope ? (
        <p role="status" className="text-xs text-state-error">
          The daemon rejected this {scopeNoun(scope)} settings change: {rejection.detail}
        </p>
      ) : null}

      {scopeSubmission ? (
        <SubmissionStatus state={state} scope={scope} editor={editor} />
      ) : (
        <div className="flex flex-wrap justify-end gap-2">
          <button
            type="button"
            className={secondarySettingsButtonClass}
            disabled={!edited || savePending}
            onClick={() => editor.revert(binding)}
          >
            Discard changes
          </button>
          <button
            type="button"
            className={settingsButtonClass}
            disabled={plan?.outcome !== 'ready' || savePending}
            onClick={() => editor.save(scope)}
          >
            Save
          </button>
        </div>
      )}
    </>
  );
}

/** Keep the submitted intent visible until it has a truthful outcome. */
function SubmissionStatus({
  state,
  scope,
  editor,
}: {
  state: SettingsEditorState;
  scope: SettingsScope;
  editor: SettingsEditorHandle;
}) {
  const requiresReload =
    state.status === 'conflicted' ||
    (state.status === 'submit_failed' &&
      (state.failure.kind === 'protocol_error' || state.failure.kind === 'unsettled'));
  const retryable =
    state.status === 'authority_withdrawn' ||
    (state.status === 'submit_failed' &&
      (state.failure.kind === 'offline' || state.failure.kind === 'error'));
  return (
    <div className="grid gap-2" data-settings-save={state.status}>
      <Verdict state={state} />
      <div className="flex flex-wrap justify-end gap-2">
        {requiresReload ? (
          <button type="button" className={settingsButtonClass} onClick={editor.reload}>
            Load current values
          </button>
        ) : retryable ? (
          <button
            type="button"
            className={settingsButtonClass}
            onClick={editor.retry}
          >
            Retry {scopeLabel(scope)}
          </button>
        ) : null}
      </div>
    </div>
  );
}

/**
 * What came back, said as what it is. A revision the authority refused, a
 * revision that moved before anything was sent, an authority that is not
 * mounted, and a write that never reached a verdict are four different
 * statements.
 */
function Verdict({ state }: { state: SettingsEditorState }) {
  switch (state.status) {
    case 'conflicted':
      return (
        <VerdictLine kind="conflicting">
          Settings changed since you loaded them. Your save was not applied. Load the current
          values, review your changes, and save again.
        </VerdictLine>
      );
    case 'authority_withdrawn':
      return <VerdictLine kind="unavailable">{state.detail}</VerdictLine>;
    case 'submit_failed':
      return (
        <VerdictLine kind={state.failure.kind === 'offline' ? 'offline' : 'error'}>
          {state.failure.kind === 'unsettled'
            ? state.failure.detail
            : state.failure.kind === 'protocol_error'
              ? state.failure.authority.startsWith('PATCH ')
                ? 'The save response could not be verified, so the change may have been applied. Load current values before trying again.'
                : 'Current settings could not be verified, so nothing was saved. Load current values before trying again.'
            : state.failure.detail}
        </VerdictLine>
      );
    case 'submitting':
      return <VerdictLine kind="loading">Saving settings…</VerdictLine>;
    case 'editor_unavailable':
    case 'editing':
      return null;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

function VerdictLine({ kind, children }: { kind: DomainStateKind; children: ReactNode }) {
  return (
    <div role="alert" className="flex flex-wrap items-start gap-2 text-xs text-text-secondary">
      <StateChip kind={kind} />
      <span className="min-w-0 flex-1 leading-relaxed">{children}</span>
    </div>
  );
}

/* ------------------------------------------------------------- readouts --*/

function Validation({
  plan,
  errors,
  edited,
  rejection,
}: {
  plan: ReturnType<typeof settingsScopePlan>;
  errors: readonly SettingsValidationError[];
  edited: boolean;
  rejection: ReturnType<typeof settingsRejection>;
}) {
  if (rejection && errors.length > 0) {
    return (
      <ul className="grid gap-0.5" data-settings-validation="rejected">
        {errors.map((error, index) => (
          <li key={`${index}:${error.field}`} className="text-body text-state-error">
            <span className="td-value">{error.field}</span> · {error.message}
          </li>
        ))}
      </ul>
    );
  }
  if (!plan) return <span className="text-body text-text-muted">unavailable</span>;
  switch (plan.outcome) {
    case 'unchanged':
      return (
        <span className="text-body text-text-muted" data-settings-validation="unchanged">
          {edited ? 'no change' : 'no proposal · effective value stands'}
        </span>
      );
    case 'invalid':
      return (
        <ul className="grid gap-0.5" data-settings-validation="invalid">
          {plan.errors.map((error, index) => (
            <li key={`${index}:${error.field}`} className="text-body text-state-error">
              <span className="td-value">{error.field}</span> · {error.message}
            </li>
          ))}
        </ul>
      );
    case 'ready': {
      const changed = Object.keys(plan.patch).length;
      return (
        <span className="text-body text-state-ready" data-settings-validation="ready">
          valid · {changed} {changed === 1 ? 'field' : 'fields'} changed
        </span>
      );
    }
    default: {
      const exhaustive: never = plan;
      return exhaustive;
    }
  }
}

function OriginText({ row }: { row: EffectiveRow }) {
  const { section } = row;
  return (
    <span className="td-value break-all text-sm text-text-secondary">
      {ORIGIN_WORD[section.origin]}
      {section.location ? ` · ${section.location}` : ''}
    </span>
  );
}

function Readouts({ children }: { children: ReactNode }) {
  // Container variants: the panel sits inside the table, whose width, not the
  // viewport's, decides how many readouts fit on a line.
  return <dl className="grid gap-x-4 gap-y-3 @md:grid-cols-2 @2xl:grid-cols-3">{children}</dl>;
}

function Readout({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <dt className="td-legend">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}

function scopeLabel(scope: SettingsScope): string {
  switch (scope) {
    case 'project':
      return 'project settings';
    case 'user':
      return 'user settings';
    case 'code_index_workers':
      return 'code-index worker selection';
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}
