/**
 * The review state for one selected key, drawn inline under its row.
 *
 * It renders one state of the editor machine and reports intent back. It never
 * decides whether a change may be applied — the machine does — so a stage that
 * cannot submit simply offers no way to try. What it always shows is the
 * ladder the brief names: the effective value stays authoritative; a proposal
 * is a proposal until the write authority validates it, persists it against
 * the held revision, reports its apply requirement, and is read back.
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
  settingsConfirmationHeld,
  settingsRejection,
  settingsReviewOf,
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
  reviewStatusWord,
} from './SettingsValues.tsx';

export function SettingsReviewPanel({
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
      aria-label={`Review ${row.key}`}
      data-settings-review={row.key}
      className="relative m-2 border border-edge-strong bg-surface-1 td-raised"
    >
      <Corners tone={capability.kind === 'writable' ? 'signal' : 'edge'} />
      <header className="flex min-h-9 items-center gap-2.5 border-b border-edge-subtle px-3">
        <h3 className="td-title truncate">
          {capability.kind === 'writable' ? 'Review' : 'Inspect'}
          <span className="ml-2 font-mono normal-case tracking-normal text-text-primary">
            {row.key}
          </span>
        </h3>
        <span aria-hidden className="td-rule" />
        <button
          type="button"
          onClick={onClose}
          disabled={!closable}
          aria-label="Close review"
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
              <span className="td-value text-2xs text-text-muted">no write path</span>
            </Readout>
          </Readouts>
          <p className="text-2xs leading-relaxed text-text-muted" data-settings-gate="no_write_path">
            <span className="text-text-secondary">Read-only.</span> GET /api/settings reports this
            key as effective configuration, and no settings PATCH route addresses it. Nothing on
            this surface can propose a change to it.
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
              <span className="td-value text-2xs text-text-secondary">
                {scopeNoun(capability.binding.scope)}
              </span>
            </Readout>
            <Readout label="write">
              <span className="inline-flex items-center gap-1 text-2xs text-state-locked">
                <Lock aria-hidden size={11} />
                locked · {capability.gate.replace('_', ' ')}
              </span>
            </Readout>
          </Readouts>
          <p
            className="text-2xs leading-relaxed text-text-muted"
            data-settings-gate={capability.gate}
          >
            <span className="text-text-secondary">Read-only · </span>
            {capability.reason}
          </p>
          <HeldReviewUnderLock scope={capability.binding.scope} editor={editor} />
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
 * A frozen review that outlived its gate: the scope moved, or the authority
 * withdrew, after a change was staged. The locked branch cannot apply it, but
 * it must still be able to let go of it — otherwise `review_dismissed` is
 * unreachable and the machine sits in a verdict no control can leave.
 */
function HeldReviewUnderLock({
  scope,
  editor,
}: {
  scope: SettingsScope;
  editor: SettingsEditorHandle;
}) {
  const review = settingsReviewOf(editor.state);
  if (review === null || review.scope !== scope) return null;
  return (
    <div className="flex flex-wrap items-center justify-between gap-2" data-settings-stage={editor.state.status}>
      <span className="text-2xs text-text-muted">
        A {scopeNoun(scope)} review is {reviewStatusWord(editor.state.status)} against revision{' '}
        <span className="td-value">{review.expectedRevisionId}</span>; this scope can no longer
        apply it.
      </span>
      <button
        type="button"
        className={secondarySettingsButtonClass}
        disabled={editor.state.status === 'submitting'}
        onClick={editor.dismiss}
      >
        Cancel review
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
  const review = settingsReviewOf(state);
  const scopeReview = review?.scope === scope ? review : null;
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
  const staged = scopeReview !== null;
  const applying = state.status === 'submitting';
  const otherReview = review !== null && scopeReview === null ? review.scope : null;

  return (
    <>
      <Readouts>
        <Readout label="effective value">
          <ValueCell row={row.row} query={query} />
          <span className="mt-0.5 block text-3xs text-text-muted">authoritative</span>
        </Readout>
        <Readout label="proposed value">
          <SettingsRowEditor
            binding={binding}
            value={proposed}
            label={`Proposed value for ${row.key}`}
            error={fieldError}
            disabled={staged || applying}
            workerStatus={workerStatus}
            onChange={(value) => editor.edit(binding, value)}
          />
        </Readout>
        <Readout label="validation">
          <Validation plan={plan} errors={scopeErrors} edited={edited} rejection={rejection?.scope === scope ? rejection : null} />
        </Readout>
        <Readout label="current revision (cas)">
          <span className="td-value break-all text-2xs text-text-primary">{revision}</span>
          <span className="mt-0.5 block text-3xs text-text-muted">
            {scopeReview
              ? scopeReview.expectedRevisionId === revision
                ? 'held by this review'
                : `review held ${scopeReview.expectedRevisionId}`
              : 'checked again immediately before apply'}
          </span>
        </Readout>
        <Readout label="target layer">
          <span className="td-value text-2xs text-text-primary">{scopeNoun(scope)}</span>
          <span className="mt-0.5 block text-3xs text-text-muted" data-settings-gate="writable">
            applies to {target}
          </span>
        </Readout>
        <Readout label="apply requirement">
          <ApplyRequirementText requirement={requirement} />
        </Readout>
      </Readouts>

      {otherReview ? (
        <p className="text-2xs text-text-muted" data-settings-other-review={state.status}>
          A {scopeNoun(otherReview)} review is {reviewStatusWord(state.status)}. Editing this
          value withdraws it.
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
          <span className="td-value text-2xs text-text-muted">
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

      {scopeReview ? (
        <FrozenReview
          state={state}
          patch={scopeReview.patch}
          expectedRevisionId={scopeReview.expectedRevisionId}
          scope={scope}
          editor={editor}
        />
      ) : (
        <div className="flex flex-wrap justify-end gap-2">
          <button
            type="button"
            className={secondarySettingsButtonClass}
            disabled={!edited || applying}
            onClick={() => editor.revert(binding)}
          >
            Discard proposal
          </button>
          <button
            type="button"
            className={settingsButtonClass}
            disabled={plan?.outcome !== 'ready' || applying}
            onClick={() => editor.review(scope)}
          >
            Review {scopeNoun(scope)} change
          </button>
        </div>
      )}
    </>
  );
}

/**
 * Once the change is frozen against a revision the panel shows exactly what
 * will be sent, asks for confirmation against that revision, and then renders
 * whatever verdict came back — each as its own state, none as "the save failed".
 */
function FrozenReview({
  state,
  patch,
  expectedRevisionId,
  scope,
  editor,
}: {
  state: SettingsEditorState;
  patch: unknown;
  expectedRevisionId: string;
  scope: SettingsScope;
  editor: SettingsEditorHandle;
}) {
  const confirmed = settingsConfirmationHeld(state);
  const applying = state.status === 'submitting';
  const resolvable = state.status === 'conflicted' || state.status === 'review_superseded';
  const retryable = state.status === 'authority_withdrawn' || state.status === 'submit_failed';
  return (
    <div className="grid gap-2" data-settings-stage={state.status}>
      <div className="grid gap-1">
        <span className="td-legend">validated patch</span>
        <pre className="max-h-40 overflow-auto border border-edge-subtle bg-surface-0 p-2 text-2xs text-text-secondary">
          {JSON.stringify(patch, null, 2)}
        </pre>
        <span className="td-value text-3xs text-text-muted">
          only the validated changed fields above are sent · expected revision {expectedRevisionId}
        </span>
      </div>
      <label className="flex cursor-pointer items-center gap-1 border border-edge-subtle py-1 pr-3 text-xs text-text-secondary">
        <input
          type="checkbox"
          className="td-check"
          checked={confirmed}
          disabled={applying || resolvable}
          onChange={(event) => editor.setConfirmed(event.target.checked)}
        />
        <span className="min-w-0">
          I confirm this change against configuration revision {expectedRevisionId}.
        </span>
      </label>
      <Verdict state={state} />
      <div className="flex flex-wrap justify-end gap-2">
        <button
          type="button"
          className={secondarySettingsButtonClass}
          disabled={applying}
          onClick={editor.dismiss}
        >
          Cancel review
        </button>
        {resolvable ? (
          <button type="button" className={settingsButtonClass} onClick={editor.reload}>
            Load current values
          </button>
        ) : (
          <button
            type="button"
            className={settingsButtonClass}
            disabled={!confirmed || applying}
            onClick={editor.apply}
          >
            {applying
              ? `Applying ${scopeLabel(scope)}`
              : retryable
                ? `Retry ${scopeLabel(scope)}`
                : `Apply ${scopeLabel(scope)}`}
          </button>
        )}
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
          Another writer saved {state.review.scope} settings after this form loaded. Your draft
          was based on {state.conflict.expectedRevisionId}; the current authority is{' '}
          {state.conflict.actualRevisionId ?? 'unknown'}. Nothing was applied.
        </VerdictLine>
      );
    case 'review_superseded':
      return (
        <VerdictLine kind="conflicting">
          The {state.review.scope} settings authority moved from {state.review.expectedRevisionId}{' '}
          to {state.currentRevisionId} while this change was under review, so this change no
          longer describes it. Nothing was sent.
        </VerdictLine>
      );
    case 'authority_withdrawn':
      return <VerdictLine kind="unavailable">{state.detail}</VerdictLine>;
    case 'submit_failed':
      return (
        <VerdictLine kind={state.failure.kind === 'offline' ? 'offline' : 'error'}>
          {state.failure.detail}
        </VerdictLine>
      );
    case 'submitting':
      return <VerdictLine kind="loading">Applying against the held revision…</VerdictLine>;
    case 'editor_unavailable':
    case 'editing':
    case 'reviewing':
    case 'confirmed':
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
          <li key={`${index}:${error.field}`} className="text-2xs text-state-error">
            <span className="td-value">{error.field}</span> · {error.message}
          </li>
        ))}
      </ul>
    );
  }
  if (!plan) return <span className="text-2xs text-text-muted">unavailable</span>;
  switch (plan.outcome) {
    case 'unchanged':
      return (
        <span className="text-2xs text-text-muted" data-settings-validation="unchanged">
          {edited ? 'no change' : 'no proposal · effective value stands'}
        </span>
      );
    case 'invalid':
      return (
        <ul className="grid gap-0.5" data-settings-validation="invalid">
          {plan.errors.map((error, index) => (
            <li key={`${index}:${error.field}`} className="text-2xs text-state-error">
              <span className="td-value">{error.field}</span> · {error.message}
            </li>
          ))}
        </ul>
      );
    case 'ready': {
      const changed = Object.keys(plan.patch).length;
      return (
        <span className="text-2xs text-state-ready" data-settings-validation="ready">
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
    <span className="td-value break-all text-2xs text-text-secondary">
      {ORIGIN_WORD[section.origin]}
      {section.location ? ` · ${section.location}` : ''}
    </span>
  );
}

function Readouts({ children }: { children: ReactNode }) {
  // Container variants: the panel sits inside the table, whose width — not the
  // viewport's — decides how many readouts fit on a line.
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
