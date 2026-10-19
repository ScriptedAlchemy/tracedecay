/**
 * The settings editor as one state machine.
 *
 * One editable draft leads directly to a saved, rejected, or pending write.
 * The exact patch, revision, and idempotency key are captured together only
 * when Save is requested; retries reuse that submission unchanged.
 *
 * Everything here is pure. No DOM, no transport, no clock.
 */

import {
  planCodeIndexWorkerChangeAgainst,
  planProjectChangeAgainst,
  planUserChangeAgainst,
  buildSettingsEditor,
  sameCodeIndexWorkerSelection,
  sameStrings,
  type CodeIndexWorkerSettingsChangeSet,
  type CodeIndexWorkerSettingsValues,
  type ProjectSettingsChangeSet,
  type ProjectSettingsValues,
  type SettingsChangePlan,
  type SettingsEditor,
  type SettingsRevisionConflict,
  type SettingsScope,
  type SettingsValidationError,
  type UserSettingsChangeSet,
  type UserSettingsValues,
} from './settingsModel.ts';
import type { ReconciliationState } from '../../contracts/generated.ts';
import type { SettingsMutationRequest, SettingsMutationResult } from './settingsMutation.ts';

/** The two drafts the editor holds, always for the same authority snapshot. */
export interface SettingsDraft {
  readonly project: ProjectSettingsValues;
  readonly user: UserSettingsValues;
  readonly codeIndexWorkers: CodeIndexWorkerSettingsValues;
}

/**
 * One exact change held for submission and any idempotent retry.
 */
export interface SettingsSubmission {
  readonly scope: SettingsScope;
  readonly expectedRevisionId: string;
  readonly patch:
    | ProjectSettingsChangeSet
    | UserSettingsChangeSet
    | CodeIndexWorkerSettingsChangeSet;
  readonly idempotencyKey: string;
}

/** A write the authority applied, as the authority reported it. */
interface SettingsAppliedRecord {
  readonly scope: SettingsScope;
  readonly message: string;
  /** The revision the PATCH response named, the authority's own statement of
   * where the resource now stands, ahead of the read that follows. */
  readonly revisionId: string;
  readonly resyncRecommended: boolean;
  readonly restartRecommended: boolean;
  readonly effectReconciliation: ReconciliationState | null;
}

/** Field-level refusals, kept with the authority that issued them. */
type SettingsRejection =
  | {
      readonly origin: 'client';
      readonly scope: SettingsScope;
      readonly errors: readonly SettingsValidationError[];
    }
  | {
      readonly origin: 'server';
      readonly scope: SettingsScope;
      readonly detail: string;
      readonly errors: readonly SettingsValidationError[];
    };

/**
 * What the editor is resting on between writes: nothing, one completed write,
 * or one refused write. Never a completed write and a refusal at once, the
 * old pair of slices could hold "Project settings saved" above a field still
 * marked invalid by an earlier attempt.
 */
type SettingsResting =
  | { readonly rest: 'clean' }
  | { readonly rest: 'applied'; readonly applied: SettingsAppliedRecord }
  | { readonly rest: 'rejected'; readonly rejection: SettingsRejection };

/** A write that did not reach a verdict. Distinct from a refusal, which did. */
type SettingsSubmitFailure =
  | { readonly kind: 'offline'; readonly detail: string }
  | { readonly kind: 'error' | 'unsettled'; readonly detail: string }
  | { readonly kind: 'protocol_error'; readonly authority: string; readonly detail: string };

interface SettingsEditable {
  readonly authority: SettingsEditor;
  readonly draft: SettingsDraft;
}

interface SettingsWithSubmission extends SettingsEditable {
  readonly submission: SettingsSubmission;
}

export type SettingsEditorState =
  /** The read parsed, but it named no revision the editor could hold a write against. */
  | { readonly status: 'editor_unavailable' }
  | (SettingsEditable & { readonly status: 'editing'; readonly resting: SettingsResting })
  | (SettingsWithSubmission & { readonly status: 'submitting' })
  /** The write authority refused: it holds a different revision. Nothing was applied. */
  | (SettingsWithSubmission & {
      readonly status: 'conflicted';
      readonly conflict: SettingsRevisionConflict;
    })
  /** The authority for this write is not mounted. Nothing was attempted. */
  | (SettingsWithSubmission & { readonly status: 'authority_withdrawn'; readonly detail: string })
  /** The write was attempted and did not reach a verdict. */
  | (SettingsWithSubmission & {
      readonly status: 'submit_failed';
      readonly failure: SettingsSubmitFailure;
    });

export type SettingsEditorAction =
  /** A fresh snapshot of the resource; `null` when it carries no usable revision. */
  | { readonly type: 'authority_observed'; readonly authority: SettingsEditor | null }
  | { readonly type: 'project_drafted'; readonly values: ProjectSettingsValues }
  | { readonly type: 'user_drafted'; readonly values: UserSettingsValues }
  | { readonly type: 'code_index_workers_drafted'; readonly values: CodeIndexWorkerSettingsValues }
  | {
      readonly type: 'save_requested';
      readonly scope: SettingsScope;
      readonly idempotencyKey: string;
    }
  | { readonly type: 'retry_requested' }
  | { readonly type: 'submit_settled'; readonly result: SettingsMutationResult }
  /** Discard the draft and take the authority's current values. */
  | { readonly type: 'reloaded_from_authority' };

const EDITOR_UNAVAILABLE: SettingsEditorState = { status: 'editor_unavailable' };

export function initialSettingsEditorState(
  authority: SettingsEditor | null,
): SettingsEditorState {
  return authority ? editing(authority, draftOf(authority), CLEAN) : EDITOR_UNAVAILABLE;
}

export function reduceSettingsEditor(
  state: SettingsEditorState,
  action: SettingsEditorAction,
): SettingsEditorState {
  switch (action.type) {
    case 'authority_observed':
      return observeAuthority(state, action.authority);
    case 'project_drafted':
      return redraft(state, (draft) => ({ ...draft, project: action.values }));
    case 'user_drafted':
      return redraft(state, (draft) => ({ ...draft, user: action.values }));
    case 'code_index_workers_drafted':
      return redraft(state, (draft) => ({ ...draft, codeIndexWorkers: action.values }));
    case 'save_requested':
      return requestSave(state, action.scope, action.idempotencyKey);
    case 'retry_requested':
      return retrySubmit(state);
    case 'submit_settled':
      return settleSubmit(state, action.result);
    case 'reloaded_from_authority':
      return state.status === 'editor_unavailable' || state.status === 'submitting'
        ? state
        : editing(state.authority, draftOf(state.authority), CLEAN);
    default: {
      const exhaustive: never = action;
      return exhaustive;
    }
  }
}

/**
 * A fresh read of the resource.
 *
 * A clean draft follows fresh values. An edited draft stays on the revision it
 * came from so Save's preflight can report a conflict instead of silently
 * replacing the user's work or re-aiming it at unseen values.
 */
function observeAuthority(
  state: SettingsEditorState,
  authority: SettingsEditor | null,
): SettingsEditorState {
  if (!authority) {
    return state.status === 'editor_unavailable' || state.status !== 'editing'
      ? state
      : EDITOR_UNAVAILABLE;
  }
  if (state.status === 'editor_unavailable') {
    return editing(authority, draftOf(authority), CLEAN);
  }
  if (sameAuthority(state.authority, authority)) return state;
  switch (state.status) {
    case 'editing': {
      if ((['project', 'user', 'code_index_workers'] as const).some((scope) =>
        settingsScopeDirty(state, scope),
      )) {
        return state;
      }
      return editing(
        authority,
        draftOf(authority),
        state.resting.rest === 'applied' ? state.resting : CLEAN,
      );
    }
    case 'conflicted':
    case 'authority_withdrawn':
    case 'submit_failed':
    case 'submitting':
      return state;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/**
 * A write owns its immutable patch until it reaches a verdict. Editing becomes
 * available again only after that verdict or an explicit reload.
 */
function redraft(
  state: SettingsEditorState,
  next: (draft: SettingsDraft) => SettingsDraft,
): SettingsEditorState {
  if (state.status !== 'editing') return state;
  return editing(
    state.authority,
    next(state.draft),
    state.resting.rest === 'rejected' ? CLEAN : state.resting,
  );
}

function requestSave(
  state: SettingsEditorState,
  scope: SettingsScope,
  idempotencyKey: string,
): SettingsEditorState {
  if (state.status !== 'editing') return state;
  const plan = planFor(state.authority, state.draft, scope);
  switch (plan.outcome) {
    case 'invalid':
      return editing(state.authority, state.draft, {
        rest: 'rejected',
        rejection: { origin: 'client', scope, errors: plan.errors },
      });
    case 'unchanged':
      return state;
    case 'ready':
      return {
        status: 'submitting',
        authority: state.authority,
        draft: state.draft,
        submission: {
          scope,
          expectedRevisionId: plan.expectedRevisionId,
          patch: plan.patch,
          idempotencyKey,
        },
      };
    default: {
      const exhaustive: never = plan;
      return exhaustive;
    }
  }
}

function retrySubmit(state: SettingsEditorState): SettingsEditorState {
  if (
    state.status === 'authority_withdrawn' ||
    (state.status === 'submit_failed' &&
      (state.failure.kind === 'offline' || state.failure.kind === 'error'))
  ) {
    return {
      status: 'submitting',
      authority: state.authority,
      draft: state.draft,
      submission: state.submission,
    };
  }
  return state;
}

function settleSubmit(
  state: SettingsEditorState,
  result: SettingsMutationResult,
): SettingsEditorState {
  if (state.status !== 'submitting') return state;
  const { authority, draft, submission } = state;
  switch (result.outcome) {
    case 'success':
      const appliedAuthority = buildSettingsEditor(result.payload);
      if (!appliedAuthority) {
        return failed(state, {
          kind: 'protocol_error',
          authority: 'settings mutation',
          detail: 'The settings response no longer carries editable values and revisions.',
        });
      }
      return editing(appliedAuthority, draftOf(appliedAuthority), {
        rest: 'applied',
        applied: {
          scope: result.scope,
          message: savedMessage(result.scope),
          revisionId: result.revisionId,
          resyncRecommended: result.resyncRecommended,
          restartRecommended: result.restartRecommended,
          effectReconciliation: result.effectReconciliation,
        },
      });
    case 'validation':
      return editing(authority, draft, {
        rest: 'rejected',
        rejection: {
          origin: 'server',
          scope: submission.scope,
          detail: result.detail,
          errors: result.errors,
        },
      });
    case 'conflict':
      return {
        status: 'conflicted',
        authority,
        draft,
        submission,
        conflict: {
          expectedRevisionId: result.expectedRevisionId,
          actualRevisionId: result.actualRevisionId,
        },
      };
    // Three ways for nothing to have been applied, and they share a state
    // because they share the statement the reader needs: the draft is intact,
    // the revision is untouched, and the detail says why. They differ only in
    // the reason, which each one carries.
    case 'unavailable':
    case 'not_dispatched':
    case 'read_only_scope':
      return {
        status: 'authority_withdrawn',
        authority,
        draft,
        submission,
        detail: result.detail,
      };
    case 'offline':
      return failed(state, { kind: 'offline', detail: result.detail });
    case 'error':
      return failed(state, { kind: 'error', detail: result.detail });
    case 'unsettled':
      return failed(state, { kind: 'unsettled', detail: result.detail });
    case 'protocol_error':
      return failed(state, {
        kind: 'protocol_error',
        authority: result.authority,
        detail: result.detail,
      });
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

/* ------------------------------------------------------------- selectors --*/

export interface SettingsRoutes {
  readonly readUrl: string;
  /** Profile-global authority read used only by the worker resource. */
  readonly codeIndexWorkerReadUrl: string;
  readonly projectPatchUrl: string;
  readonly userPatchUrl: string;
  readonly codeIndexWorkerPatchUrl: string;
}

export type SubmittingSettingsState = Extract<SettingsEditorState, { status: 'submitting' }>;

/**
 * The request a submitting state stands for.
 *
 * Total, and reachable from nowhere else: a request exists only after Save has
 * captured the exact patch and revision it will ask the authority to compare.
 *
 * Scope writability is deliberately absent. The controller supplies its current
 * value at dispatch so a changed permission cannot be bypassed by a stale form.
 */
export function settingsSubmission(
  state: SubmittingSettingsState,
  routes: SettingsRoutes,
): Omit<SettingsMutationRequest, 'writability'> {
  return {
    scope: state.submission.scope,
    expectedRevisionId: state.submission.expectedRevisionId,
    idempotencyKey: state.submission.idempotencyKey,
    readUrl: readUrlFor(state.submission.scope, routes),
    patchUrl: patchUrlFor(state.submission.scope, routes),
    patch: state.submission.patch,
  };
}

/** The immutable write awaiting a verdict or an explicit resolution. */
export function settingsSubmissionOf(state: SettingsEditorState): SettingsSubmission | null {
  return state.status === 'editor_unavailable' || state.status === 'editing'
    ? null
    : state.submission;
}

export function settingsRejection(state: SettingsEditorState): SettingsRejection | null {
  return state.status === 'editing' && state.resting.rest === 'rejected'
    ? state.resting.rejection
    : null;
}

export function settingsApplied(state: SettingsEditorState): SettingsAppliedRecord | null {
  return state.status === 'editing' && state.resting.rest === 'applied'
    ? state.resting.applied
    : null;
}

/** Whether this scope's draft differs from the authority it was taken from. */
export function settingsScopeDirty(
  state: SettingsEditorState,
  scope: SettingsScope,
): boolean {
  const plan = settingsScopePlan(state, scope);
  return plan !== null && plan.outcome !== 'unchanged';
}

/**
 * The change this scope's draft currently amounts to, replanned live so the
 * editor can state validity while the value is still being typed.
 */
export function settingsScopePlan(
  state: SettingsEditorState,
  scope: SettingsScope,
): SettingsPlan | null {
  if (state.status === 'editor_unavailable') return null;
  return planFor(state.authority, state.draft, scope);
}

/* --------------------------------------------------------------- helpers --*/

const CLEAN: SettingsResting = { rest: 'clean' };

function editing(
  authority: SettingsEditor,
  draft: SettingsDraft,
  resting: SettingsResting,
): SettingsEditorState {
  return { status: 'editing', authority, draft, resting };
}

function failed(
  state: SubmittingSettingsState,
  failure: SettingsSubmitFailure,
): SettingsEditorState {
  return {
    status: 'submit_failed',
    authority: state.authority,
    draft: state.draft,
    submission: state.submission,
    failure,
  };
}

function draftOf(authority: SettingsEditor): SettingsDraft {
  return {
    project: authority.project,
    user: authority.user,
    codeIndexWorkers: authority.codeIndexWorkers,
  };
}

type SettingsPlan = SettingsChangePlan<
  ProjectSettingsChangeSet | UserSettingsChangeSet | CodeIndexWorkerSettingsChangeSet
>;

function planFor(
  authority: SettingsEditor,
  draft: SettingsDraft,
  scope: SettingsScope,
): SettingsPlan {
  switch (scope) {
    case 'project':
      return planProjectChangeAgainst(authority, draft.project);
    case 'user':
      return planUserChangeAgainst(authority, draft.user);
    case 'code_index_workers':
      return planCodeIndexWorkerChangeAgainst(authority, draft.codeIndexWorkers);
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

function patchUrlFor(scope: SettingsScope, routes: SettingsRoutes): string {
  switch (scope) {
    case 'project':
      return routes.projectPatchUrl;
    case 'user':
      return routes.userPatchUrl;
    case 'code_index_workers':
      return routes.codeIndexWorkerPatchUrl;
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

function readUrlFor(scope: SettingsScope, routes: SettingsRoutes): string {
  return scope === 'code_index_workers' ? routes.codeIndexWorkerReadUrl : routes.readUrl;
}

function savedMessage(scope: SettingsScope): string {
  switch (scope) {
    case 'project':
      return 'Project settings saved';
    case 'user':
      return 'User settings saved';
    case 'code_index_workers':
      return 'Code-index worker selection saved';
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

function sameAuthority(left: SettingsEditor, right: SettingsEditor): boolean {
  return (
    left.projectExpectedRevisionId === right.projectExpectedRevisionId &&
    left.userExpectedRevisionId === right.userExpectedRevisionId &&
    left.codeIndexWorkerExpectedRevisionId === right.codeIndexWorkerExpectedRevisionId &&
    sameProjectValues(left.project, right.project) &&
    sameUserValues(left.user, right.user) &&
    sameCodeIndexWorkerValues(left.codeIndexWorkers, right.codeIndexWorkers)
  );
}

function sameProjectValues(
  left: ProjectSettingsValues,
  right: ProjectSettingsValues,
): boolean {
  return (
    sameStrings(left.include, right.include) &&
    sameStrings(left.exclude, right.exclude) &&
    left.max_file_size === right.max_file_size &&
    left.extract_docstrings === right.extract_docstrings &&
    left.track_call_sites === right.track_call_sites &&
    left.context_scout === right.context_scout &&
    left.telemetry_timings === right.telemetry_timings &&
    left.auto_track_pr_branches === right.auto_track_pr_branches &&
    left.auto_track_pr_poll_secs === right.auto_track_pr_poll_secs
  );
}

function sameUserValues(left: UserSettingsValues, right: UserSettingsValues): boolean {
  return (
    left.upload_enabled === right.upload_enabled &&
    left.watcher_debounce === right.watcher_debounce &&
    left.extraction_timeout_secs === right.extraction_timeout_secs
  );
}

function sameCodeIndexWorkerValues(
  left: CodeIndexWorkerSettingsValues,
  right: CodeIndexWorkerSettingsValues,
): boolean {
  return (
    sameCodeIndexWorkerSelection(left.code_index_workers, right.code_index_workers) &&
    sameCodeIndexWorkerStatus(left.code_index_worker_status, right.code_index_worker_status)
  );
}

function sameCodeIndexWorkerStatus(
  left: CodeIndexWorkerSettingsValues['code_index_worker_status'],
  right: CodeIndexWorkerSettingsValues['code_index_worker_status'],
): boolean {
  if (left === null || right === null) return left === right;
  return (
    sameCodeIndexWorkerSelection(left.configured, right.configured) &&
    left.environment_override_workers === right.environment_override_workers &&
    left.effective_workers === right.effective_workers &&
    left.available_logical_cpus === right.available_logical_cpus &&
    left.memory_safe_workers === right.memory_safe_workers &&
    left.limiting_reason === right.limiting_reason
  );
}
