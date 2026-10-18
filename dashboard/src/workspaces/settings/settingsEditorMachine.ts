/**
 * The settings editor as one state machine.
 *
 * The editor used to be six independent `useState` slices that only added up to
 * a correct screen when they happened to agree. Here every state names one
 * situation and carries exactly what that situation has: only `editing` has
 * rest state, and only the states that follow a save request carry the
 * `attempt` that request stands for.
 *
 * An ordinary valid change saves directly: `save_requested` plans the draft
 * against the revision the editor holds and enters `submitting` in one step.
 * The planned patch, the revision it was planned against, and the idempotency
 * key it is sent under are fixed together as the attempt, so a retry after an
 * outcome the editor could not see is the same request, never a new one, and a
 * resource that moved in the meantime ends in `conflicted` instead of being
 * written over.
 *
 * Everything here is pure. No DOM, no transport, no clock.
 */

import {
  planCodeIndexWorkerChangeAgainst,
  planProjectChangeAgainst,
  planUserChangeAgainst,
  sameCodeIndexWorkerSelection,
  sameStrings,
  type CodeIndexWorkerSettingsChangeSet,
  type CodeIndexWorkerSettingsValues,
  settingsRevisionId,
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
import type { SettingsMutationRequest, SettingsMutationResult } from './settingsMutation.ts';

/** The drafts the editor holds, always for the same authority snapshot. */
export interface SettingsDraft {
  readonly project: ProjectSettingsValues;
  readonly user: UserSettingsValues;
  readonly codeIndexWorkers: CodeIndexWorkerSettingsValues;
}

/**
 * One save request: a validated patch against the revision it was planned
 * from, and the idempotency key it is sent under. Fixed when the save starts
 * and reused unchanged by every retry.
 */
interface SettingsAttempt {
  readonly scope: SettingsScope;
  readonly expectedRevisionId: string;
  readonly patch:
    | ProjectSettingsChangeSet
    | UserSettingsChangeSet
    | CodeIndexWorkerSettingsChangeSet;
  readonly idempotencyKey: string;
}

/** A write the authority confirmed, as the authority reported it. */
interface SettingsAppliedRecord {
  readonly scope: SettingsScope;
  readonly message: string;
  /** The revision the PATCH response named, the authority's own statement of
   * where the resource now stands, ahead of the read that follows. */
  readonly revisionId: string;
  readonly resyncRecommended: boolean;
  readonly restartRecommended: boolean;
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
 * or one refused write. Never a completed write and a refusal at once.
 */
type SettingsResting =
  | { readonly rest: 'clean' }
  | { readonly rest: 'applied'; readonly applied: SettingsAppliedRecord }
  | { readonly rest: 'rejected'; readonly rejection: SettingsRejection };

/**
 * A save that did not land and was not refused on its merits.
 *
 * `not_sent` is the absence of a write: the scope or the authority declined it
 * and nothing changed. The other three did not reach a verdict, so whether the
 * write took effect is not known; retrying them reuses the attempt's identity.
 */
type SettingsSaveFailure =
  | { readonly kind: 'not_sent'; readonly detail: string }
  | { readonly kind: 'offline'; readonly detail: string }
  | { readonly kind: 'error'; readonly detail: string }
  | { readonly kind: 'protocol_error'; readonly authority: string; readonly detail: string };

interface SettingsEditable {
  readonly authority: SettingsEditor;
  readonly draft: SettingsDraft;
}

interface SettingsAttempting extends SettingsEditable {
  readonly attempt: SettingsAttempt;
}

export type SettingsEditorState =
  /** The read parsed, but it named no revision the editor could hold a write against. */
  | { readonly status: 'editor_unavailable' }
  | (SettingsEditable & { readonly status: 'editing'; readonly resting: SettingsResting })
  | (SettingsAttempting & { readonly status: 'submitting' })
  /** The resource holds a different revision than the one this save was based on. Nothing was applied. */
  | (SettingsAttempting & {
      readonly status: 'conflicted';
      readonly conflict: SettingsRevisionConflict;
    })
  /** The save did not land. `failure.kind` says whether anything was attempted. */
  | (SettingsAttempting & { readonly status: 'failed'; readonly failure: SettingsSaveFailure });

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
  /** Resend the attempt a failed save held, under the same identity. */
  | { readonly type: 'retry_requested' }
  | { readonly type: 'save_settled'; readonly result: SettingsMutationResult }
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
      return retrySave(state);
    case 'save_settled':
      return settleSave(state, action.result);
    case 'reloaded_from_authority':
      return state.status === 'editor_unavailable'
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
 * The draft always follows the authority, the editor shows what is there, not
 * what was there. A held attempt does not: it is a statement about a specific
 * revision, so when the authority moves past that revision the attempt ends in
 * a conflict rather than being quietly re-aimed at the new one. The one
 * exception is a write already in flight, whose own pre-flight check owns that
 * decision.
 */
function observeAuthority(
  state: SettingsEditorState,
  authority: SettingsEditor | null,
): SettingsEditorState {
  if (!authority) {
    return state.status === 'editor_unavailable' ? state : EDITOR_UNAVAILABLE;
  }
  if (state.status === 'editor_unavailable') {
    return editing(authority, draftOf(authority), CLEAN);
  }
  if (sameAuthority(state.authority, authority)) return state;
  const draft = draftOf(authority);
  switch (state.status) {
    case 'editing':
      // A completed write stays true across the refetch it triggered; a
      // refusal judged values the new snapshot has just replaced.
      return editing(
        authority,
        draft,
        state.resting.rest === 'applied' ? state.resting : CLEAN,
      );
    case 'submitting':
      return { ...state, authority, draft };
    case 'conflicted':
    case 'failed': {
      const current = settingsRevisionId(authority, state.attempt.scope);
      return current === state.attempt.expectedRevisionId
        ? { ...state, authority, draft }
        : conflicted(authority, draft, state.attempt, {
            expectedRevisionId: state.attempt.expectedRevisionId,
            actualRevisionId: current,
          });
    }
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/**
 * An edit. An attempt describes a draft; once the draft moves, the attempt
 * describes nothing, so it goes, and the next save is a new request.
 *
 * A write in flight is the exception: leaving `submitting` would make
 * `settleSave` discard the verdict when it lands, so the edit waits. A refusal
 * the editor is resting on judged the draft as it was; once the draft moves it
 * is no longer about anything and goes too, while a completed write stays true
 * and stays.
 */
function redraft(
  state: SettingsEditorState,
  next: (draft: SettingsDraft) => SettingsDraft,
): SettingsEditorState {
  if (state.status === 'editor_unavailable' || state.status === 'submitting') return state;
  if (state.status === 'editing') {
    return editing(
      state.authority,
      next(state.draft),
      state.resting.rest === 'rejected' ? CLEAN : state.resting,
    );
  }
  return editing(state.authority, next(state.draft), CLEAN);
}

/**
 * Save a scope's draft: plan it against the revision held and send it. Only an
 * editing state may start a save, so a failed or conflicted attempt is resolved
 * through `retry_requested` or a reload and can never be replaced by a request
 * under a new identity by accident.
 */
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
        attempt: {
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

/**
 * Resending a failed save. The attempt is replanned against the authority the
 * editor now holds, and only an identical patch against the identical revision
 * keeps its identity; anything else means the attempt no longer describes the
 * resource, and it ends in a conflict instead of being sent.
 */
function retrySave(state: SettingsEditorState): SettingsEditorState {
  if (state.status !== 'failed') return state;
  const { authority, draft, attempt } = state;
  const replanned = planFor(authority, draft, attempt.scope);
  if (
    replanned.outcome !== 'ready' ||
    replanned.expectedRevisionId !== attempt.expectedRevisionId ||
    stableJson(replanned.patch) !== stableJson(attempt.patch)
  ) {
    return conflicted(authority, draft, attempt, {
      expectedRevisionId: attempt.expectedRevisionId,
      actualRevisionId: settingsRevisionId(authority, attempt.scope),
    });
  }
  return { status: 'submitting', authority, draft, attempt };
}

function settleSave(
  state: SettingsEditorState,
  result: SettingsMutationResult,
): SettingsEditorState {
  if (state.status !== 'submitting') return state;
  const { authority, draft, attempt } = state;
  switch (result.outcome) {
    case 'success':
      return editing(authority, draft, {
        rest: 'applied',
        applied: {
          scope: result.scope,
          message: savedMessage(result.scope),
          revisionId: result.revisionId,
          resyncRecommended: result.resyncRecommended,
          restartRecommended: result.restartRecommended,
        },
      });
    case 'validation':
      return editing(authority, draft, {
        rest: 'rejected',
        rejection: {
          origin: 'server',
          scope: attempt.scope,
          detail: result.detail,
          errors: result.errors,
        },
      });
    case 'conflict':
      return conflicted(authority, draft, attempt, {
        expectedRevisionId: result.expectedRevisionId,
        actualRevisionId: result.actualRevisionId,
      });
    // Three ways for nothing to have been applied: the draft is intact, the
    // revision is untouched, and the detail says why.
    case 'unavailable':
    case 'not_dispatched':
    case 'read_only_scope':
      return failed(state, { kind: 'not_sent', detail: result.detail });
    case 'offline':
      return failed(state, { kind: 'offline', detail: result.detail });
    case 'error':
      return failed(state, { kind: 'error', detail: result.detail });
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
 * Total, and reachable from nowhere else: a request exists only where the
 * machine has already planned the draft against the held revision, so there is
 * no path that sends a patch built from anything but the attempt.
 *
 * Scope writability is deliberately absent. It is a property of the dashboard
 * the routes point at, and it can change while an attempt is held. The
 * controller supplies it at dispatch so the write refuses on the current
 * reading rather than one captured when the draft was planned.
 */
export function settingsSubmission(
  state: SubmittingSettingsState,
  routes: SettingsRoutes,
): Omit<SettingsMutationRequest, 'writability'> {
  return {
    scope: state.attempt.scope,
    expectedRevisionId: state.attempt.expectedRevisionId,
    idempotencyKey: state.attempt.idempotencyKey,
    readUrl: readUrlFor(state.attempt.scope, routes),
    patchUrl: patchUrlFor(state.attempt.scope, routes),
    patch: state.attempt.patch,
  };
}

/** The save the editor is holding, if it is holding one. */
export function settingsAttemptOf(state: SettingsEditorState): SettingsAttempt | null {
  return state.status === 'editor_unavailable' || state.status === 'editing'
    ? null
    : state.attempt;
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
 * The change this scope's draft currently amounts to, replanned live.
 *
 * This is the same plan `save_requested` sends, computed without sending it,
 * so a surface can state validity, `ready`, `unchanged`, or `invalid` with the
 * daemon-shaped field errors, while the value is still being typed. `null`
 * while the read names no revision to plan against.
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

function conflicted(
  authority: SettingsEditor,
  draft: SettingsDraft,
  attempt: SettingsAttempt,
  conflict: SettingsRevisionConflict,
): SettingsEditorState {
  return { status: 'conflicted', authority, draft, attempt, conflict };
}

function failed(
  state: SubmittingSettingsState,
  failure: SettingsSaveFailure,
): SettingsEditorState {
  return {
    status: 'failed',
    authority: state.authority,
    draft: state.draft,
    attempt: state.attempt,
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

/** Key order never distinguishes two patches, so identity does not depend on it. */
function stableJson(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map((item) => stableJson(item)).join(',')}]`;
  }
  if (typeof value === 'object' && value !== null) {
    const entries = Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
      .map(([key, item]) => `${JSON.stringify(key)}:${stableJson(item)}`);
    return `{${entries.join(',')}}`;
  }
  return JSON.stringify(value) ?? 'null';
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
