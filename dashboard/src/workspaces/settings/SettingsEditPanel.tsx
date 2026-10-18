/**
 * The editor for one selected key, drawn inline under its row.
 *
 * It renders one state of the editor machine and reports intent back. It never
 * decides whether a change may be saved, the machine does, so a state that
 * cannot save simply offers no way to try. What it always shows: the effective
 * value stays authoritative; an edit is a proposal until the write authority
 * validates it, persists it against the held revision, reports its apply
 * requirement, and is read back.
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
  settingsAttemptOf,
  settingsRejection,
  settingsScopePlan,
  type SettingsEditorState,
} from './settingsEditorMachine.ts';
import {
  settingsRevisionId,
  type SettingsScope,
  type SettingsValidationError,
} from './settingsModel.ts';
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
  attemptStatusWord,
} from './SettingsValues.tsx';

export function SettingsEditPanel({
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
  /** False while a write is in flight: the panel stays until the verdict lands. */
  closable: boolean;
  onClose: () => void;
}) {
  return (
    <section
      aria-label={`Edit ${row.key}`}
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
            <span className="text-text-secondary">Read-only.</span> This key reports effective
            configuration only. It cannot be changed from this dashboard.
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
          <HeldAttemptUnderLock scope={capability.binding.scope} editor={editor} />
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
 * A save attempt that outlived its gate: the scope went read-only, or the
 * authority withdrew, after the save was started. The locked branch cannot
 * send it, but it must still be able to let go of it, otherwise the machine
 * sits in a verdict no control can leave.
 */
function HeldAttemptUnderLock({
  scope,
  editor,
}: {
  scope: SettingsScope;
  editor: SettingsEditorHandle;
}) {
  const attempt = settingsAttemptOf(editor.state);
  if (attempt === null || attempt.scope !== scope) return null;
  return (
    <div className="flex flex-wrap items-center justify-between gap-2" data-settings-stage={editor.state.status}>
      <span className="text-body text-text-muted">
        A {scopeNoun(scope)} save is {attemptStatusWord(editor.state.status)} against revision{' '}
        <span className="td-value">{attempt.expectedRevisionId}</span>; this scope can no longer
        send it.
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
        Settings editing requires project configuration values and configuration_revision_id
        from GET /api/settings, plus user settings and configuration_revision_id from the same
        authority. The response omitted at least one required field.
      </p>
    );
  }
  const scope = binding.scope;
  const attempt = settingsAttemptOf(state);
  const scopeAttempt = attempt?.scope === scope ? attempt : null;
  const plan = settingsScopePlan(state, scope);
  const rejection = settingsRejection(state);
  const scopeErrors = rejection?.scope === scope ? rejection.errors : [];
  // The input wears whichever refusal names its field: a verdict the editor is
  // resting on first, else the live plan's, so `aria-invalid` is true the
  // moment the value would be refused rather than only after a review attempt.
  // (An edit clears a resting refusal in the machine, so the two never
  // describe different drafts.)
  const liveErrors = plan?.outcome === 'invalid' ? plan.errors : [];
  const fieldError = [...scopeErrors, ...liveErrors].find(
    (error) => error.field === binding.field,
  )?.message;
  const applied = settingsApplied(state);
  const revision = settingsRevisionId(state.authority, scope);
  const proposed = draftValue(state.draft, binding);
  const edited = fieldEdited(state.draft, state.authority, binding);
  const requirement = applyRequirement(binding);
  const applying = state.status === 'submitting';
  // A conflicted draft was planned from a revision that is gone; it is resolved
  // by loading the current values, not by editing on top of it.
  const lockedByConflict = state.status === 'conflicted' && scopeAttempt !== null;
  const failedHere = state.status === 'failed' && scopeAttempt !== null;
  const otherAttempt = attempt !== null && scopeAttempt === null ? attempt.scope : null;

  return (
    <>
      <Readouts>
        <Readout label="effective value">
          <ValueCell row={row.row} query={query} />
          <span className="mt-0.5 block text-sm text-text-muted">authoritative</span>
        </Readout>
        <Readout label="proposed value">
          <SettingsRowEditor
            binding={binding}
            value={proposed}
            label={`Proposed value for ${row.key}`}
            error={fieldError}
            disabled={lockedByConflict || applying}
            workerStatus={workerStatus}
            onChange={(value) => editor.edit(binding, value)}
          />
        </Readout>
        <Readout label="validation">
          <Validation plan={plan} errors={scopeErrors} edited={edited} rejection={rejection?.scope === scope ? rejection : null} />
        </Readout>
        <Readout label="current revision (cas)">
          <span className="td-value break-all text-sm text-text-primary">{revision}</span>
          <span className="mt-0.5 block text-sm text-text-muted">
            {scopeAttempt
              ? scopeAttempt.expectedRevisionId === revision
                ? 'held by this save'
                : `save held ${scopeAttempt.expectedRevisionId}`
              : 'checked again immediately before save'}
          </span>
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

      {otherAttempt ? (
        <p className="text-body text-text-muted" data-settings-other-attempt={state.status}>
          A {scopeNoun(otherAttempt)} save is {attemptStatusWord(state.status)}. Editing this
          value drops it.
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
        </p>
      ) : null}

      {rejection?.origin === 'server' && rejection.scope === scope ? (
        <p role="status" className="text-xs text-state-error">
          The daemon rejected this {scopeNoun(scope)} settings change: {rejection.detail}
        </p>
      ) : null}

      {scopeAttempt ? <Verdict state={state} /> : null}

      <div className="flex flex-wrap justify-end gap-2">
        {lockedByConflict ? null : (
          <button
            type="button"
            className={secondarySettingsButtonClass}
            disabled={!edited || applying}
            onClick={() => editor.revert(binding)}
          >
            Discard proposal
          </button>
        )}
        {/* One primary button for every state, so focus stays on it as Save
          * becomes Saving, Retry, or Load current values. */}
        <button
          type="button"
          className={settingsButtonClass}
          disabled={applying || (!lockedByConflict && !failedHere && plan?.outcome !== 'ready')}
          onClick={
            lockedByConflict ? editor.reload : failedHere ? editor.retry : () => editor.save(scope)
          }
        >
          {lockedByConflict
            ? 'Load current values'
            : applying
              ? `Saving ${scopeLabel(scope)}`
              : failedHere
                ? `Retry ${scopeLabel(scope)}`
                : `Save ${scopeLabel(scope)}`}
        </button>
      </div>
    </>
  );
}

/**
 * What came back, said as what it is. A revision that moved, a write that was
 * never sent, and a write whose outcome the editor could not see are different
 * statements, and only the last one tells the reader it may have been applied.
 */
function Verdict({ state }: { state: SettingsEditorState }) {
  switch (state.status) {
    case 'conflicted':
      return (
        <div data-settings-stage={state.status}>
          <VerdictLine kind="conflicting">
            The {state.attempt.scope} settings changed after this form loaded. Your change was
            based on {state.conflict.expectedRevisionId}; the current revision is{' '}
            {state.conflict.actualRevisionId ?? 'unknown'}. Nothing further was sent, and your
            change was not written over the newer settings. Load the current values, then make
            the change again if it is still needed.
          </VerdictLine>
        </div>
      );
    case 'failed':
      return (
        <div data-settings-stage={state.status}>
          {state.failure.kind === 'not_sent' ? (
            <VerdictLine kind="unavailable">{state.failure.detail}</VerdictLine>
          ) : (
            <VerdictLine kind={state.failure.kind === 'offline' ? 'offline' : 'error'}>
              {state.failure.detail} The save may or may not have taken effect. Retrying sends the
              same request.
            </VerdictLine>
          )}
        </div>
      );
    case 'submitting':
      return (
        <div data-settings-stage={state.status}>
          <VerdictLine kind="loading">Saving against the held revision…</VerdictLine>
        </div>
      );
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
