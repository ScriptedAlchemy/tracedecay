/**
 * The settings editor as one state machine.
 *
 * The editor used to be six independent `useState` slices — the project draft,
 * the user draft, the pending review, a `confirmed` boolean, the validation
 * errors, and the saved notice — that only added up to a correct screen when
 * they happened to agree. Nothing stopped them disagreeing. A confirmation was
 * a bare boolean beside the review it was supposedly given for, so a refetch
 * could replace the draft underneath a confirmed review and the next apply
 * would send a patch nobody had confirmed against a revision nobody was
 * looking at. A saved notice sat beside a fresh pending change, telling the
 * reader the change was already applied.
 *
 * Here those combinations do not exist to be reached. Every state names one
 * situation and carries exactly what that situation has: only `editing` has
 * rest state, only the review-bearing states have a review, and a confirmation
 * is not a flag beside a review but a state that contains one. A review is
 * identified by the exact change against the exact revision it was planned
 * from (`reviewId`), so replanning is enough to tell whether the thing the
 * user confirmed is still the thing about to be sent.
 *
 * Everything here is pure. No DOM, no transport, no clock.
 */

import {
  planProjectChangeAgainst,
  planUserChangeAgainst,
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

/** The two drafts the editor holds, always for the same authority snapshot. */
export interface SettingsDraft {
  readonly project: ProjectSettingsValues;
  readonly user: UserSettingsValues;
}

/**
 * A validated change frozen against the revision it was planned from.
 *
 * `reviewId` is the identity of that pairing. Two reviews with the same id are
 * the same change against the same revision; anything else — an edited draft,
 * a moved authority — produces a different id, which is how a confirmation
 * given for one review is refused for another.
 */
export interface SettingsReview {
  readonly scope: SettingsScope;
  readonly expectedRevisionId: string;
  readonly patch: ProjectSettingsChangeSet | UserSettingsChangeSet;
  readonly reviewId: string;
}

/** A write the authority confirmed, as the authority reported it. */
export interface SettingsAppliedRecord {
  readonly scope: SettingsScope;
  readonly message: string;
  readonly resyncRecommended: boolean;
  readonly restartRecommended: boolean;
}

/** Field-level refusals, kept with the authority that issued them. */
export type SettingsRejection =
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
 * or one refused write. Never a completed write and a refusal at once — the
 * old pair of slices could hold "Project settings saved" above a field still
 * marked invalid by an earlier attempt.
 */
export type SettingsResting =
  | { readonly rest: 'clean' }
  | { readonly rest: 'applied'; readonly applied: SettingsAppliedRecord }
  | { readonly rest: 'rejected'; readonly rejection: SettingsRejection };

/** A write that did not reach a verdict. Distinct from a refusal, which did. */
export type SettingsSubmitFailure =
  | { readonly kind: 'offline'; readonly detail: string }
  | { readonly kind: 'error'; readonly detail: string }
  | { readonly kind: 'protocol_error'; readonly authority: string; readonly detail: string };

interface SettingsEditable {
  readonly authority: SettingsEditor;
  readonly draft: SettingsDraft;
}

interface SettingsUnderReview extends SettingsEditable {
  readonly review: SettingsReview;
}

export type SettingsEditorState =
  /** The read parsed, but it named no revision the editor could hold a write against. */
  | { readonly status: 'editor_unavailable' }
  | (SettingsEditable & { readonly status: 'editing'; readonly resting: SettingsResting })
  | (SettingsUnderReview & { readonly status: 'reviewing' })
  | (SettingsUnderReview & { readonly status: 'confirmed' })
  | (SettingsUnderReview & { readonly status: 'submitting' })
  /** The write authority refused: it holds a different revision. Nothing was applied. */
  | (SettingsUnderReview & {
      readonly status: 'conflicted';
      readonly conflict: SettingsRevisionConflict;
    })
  /** The authority for this write is not mounted. Nothing was attempted. */
  | (SettingsUnderReview & { readonly status: 'authority_withdrawn'; readonly detail: string })
  /** The write was attempted and did not reach a verdict. */
  | (SettingsUnderReview & {
      readonly status: 'submit_failed';
      readonly failure: SettingsSubmitFailure;
    })
  /** The resource moved while this change was held. Nothing was attempted. */
  | (SettingsUnderReview & {
      readonly status: 'review_superseded';
      readonly currentRevisionId: string;
    });

export type SettingsEditorAction =
  /** A fresh snapshot of the resource; `null` when it carries no usable revision. */
  | { readonly type: 'authority_observed'; readonly authority: SettingsEditor | null }
  | { readonly type: 'project_drafted'; readonly values: ProjectSettingsValues }
  | { readonly type: 'user_drafted'; readonly values: UserSettingsValues }
  | { readonly type: 'review_requested'; readonly scope: SettingsScope }
  | { readonly type: 'review_dismissed' }
  | { readonly type: 'confirmation_set'; readonly confirmed: boolean }
  | { readonly type: 'submit_started' }
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
    case 'review_requested':
      return requestReview(state, action.scope);
    case 'review_dismissed':
      // A dismissal cannot cancel a write already in flight, and it cannot
      // clear a verdict the editor is resting on — closing the dialog after a
      // save must not take the save with it.
      return state.status === 'editor_unavailable' ||
        state.status === 'editing' ||
        state.status === 'submitting'
        ? state
        : editing(state.authority, state.draft, CLEAN);
    case 'confirmation_set':
      return setConfirmation(state, action.confirmed);
    case 'submit_started':
      return startSubmit(state);
    case 'submit_settled':
      return settleSubmit(state, action.result);
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
 * The draft always follows the authority — the editor shows what is there, not
 * what was there. A review does not: it is a statement about a specific
 * revision, so when the authority moves past that revision the review is
 * superseded rather than quietly re-aimed at the new one. The one exception is
 * a write already in flight, whose own pre-flight check owns that decision.
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
    case 'reviewing':
    case 'confirmed':
    case 'conflicted':
    case 'authority_withdrawn':
    case 'submit_failed':
    case 'review_superseded': {
      const current = settingsRevisionId(authority, state.review.scope);
      return current === state.review.expectedRevisionId
        ? { ...state, authority, draft }
        : superseded(authority, draft, state.review, current);
    }
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/**
 * An edit. A review describes a draft; once the draft moves, the review
 * describes nothing, so it goes — along with any confirmation given for it.
 */
function redraft(
  state: SettingsEditorState,
  next: (draft: SettingsDraft) => SettingsDraft,
): SettingsEditorState {
  if (state.status === 'editor_unavailable') return state;
  if (state.status === 'editing') {
    return editing(state.authority, next(state.draft), state.resting);
  }
  return editing(state.authority, next(state.draft), CLEAN);
}

function requestReview(
  state: SettingsEditorState,
  scope: SettingsScope,
): SettingsEditorState {
  if (state.status === 'editor_unavailable') return state;
  const plan = planFor(state.authority, state.draft, scope);
  switch (plan.outcome) {
    case 'invalid':
      return editing(state.authority, state.draft, {
        rest: 'rejected',
        rejection: { origin: 'client', scope, errors: plan.errors },
      });
    case 'unchanged':
      return editing(state.authority, state.draft, {
        rest: 'rejected',
        rejection: {
          origin: 'client',
          scope,
          errors: [{ field: scope, message: `No ${scope} settings have changed.` }],
        },
      });
    case 'ready':
      return {
        status: 'reviewing',
        authority: state.authority,
        draft: state.draft,
        review: reviewOf(scope, plan),
      };
    default: {
      const exhaustive: never = plan;
      return exhaustive;
    }
  }
}

/**
 * A confirmation is not a flag: it is the state that holds the review. So it
 * can only be given while a review is open, and it can only be withdrawn from
 * a state whose review it was given for.
 */
function setConfirmation(
  state: SettingsEditorState,
  confirmed: boolean,
): SettingsEditorState {
  switch (state.status) {
    case 'reviewing':
      return confirmed
        ? {
            status: 'confirmed',
            authority: state.authority,
            draft: state.draft,
            review: state.review,
          }
        : state;
    case 'confirmed':
    case 'authority_withdrawn':
    case 'submit_failed':
      return confirmed
        ? state
        : {
            status: 'reviewing',
            authority: state.authority,
            draft: state.draft,
            review: state.review,
          };
    // A write in flight, a refused revision, and a superseded review are past
    // the point where a checkbox decides anything.
    case 'editor_unavailable':
    case 'editing':
    case 'submitting':
    case 'conflicted':
    case 'review_superseded':
      return state;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/**
 * Entering the write.
 *
 * The confirmation is re-derived rather than trusted: the change is replanned
 * against the authority the editor now holds, and only an identical review id
 * lets the write proceed. The id carries the revision as well as the change,
 * so one comparison catches both a resource that moved and a draft that no
 * longer produces what was confirmed — either way the review is superseded
 * instead of applied.
 */
function startSubmit(state: SettingsEditorState): SettingsEditorState {
  // Reachable only from a confirmation: `submit_failed` and
  // `authority_withdrawn` are themselves only reachable through one, so
  // retrying from them is retrying a change the user did confirm.
  if (
    state.status !== 'confirmed' &&
    state.status !== 'authority_withdrawn' &&
    state.status !== 'submit_failed'
  ) {
    return state;
  }
  const { authority, draft, review } = state;
  const replanned = planFor(authority, draft, review.scope);
  if (
    replanned.outcome !== 'ready' ||
    reviewOf(review.scope, replanned).reviewId !== review.reviewId
  ) {
    return superseded(authority, draft, review, settingsRevisionId(authority, review.scope));
  }
  return { status: 'submitting', authority, draft, review };
}

function settleSubmit(
  state: SettingsEditorState,
  result: SettingsMutationResult,
): SettingsEditorState {
  if (state.status !== 'submitting') return state;
  const { authority, draft, review } = state;
  switch (result.outcome) {
    case 'success':
      return editing(authority, draft, {
        rest: 'applied',
        applied: {
          scope: result.scope,
          message: savedMessage(result.scope),
          resyncRecommended: result.resyncRecommended,
          restartRecommended: result.restartRecommended,
        },
      });
    case 'validation':
      return editing(authority, draft, {
        rest: 'rejected',
        rejection: {
          origin: 'server',
          scope: review.scope,
          detail: result.detail,
          errors: result.errors,
        },
      });
    case 'conflict':
      return {
        status: 'conflicted',
        authority,
        draft,
        review,
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
      return { status: 'authority_withdrawn', authority, draft, review, detail: result.detail };
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
  readonly projectPatchUrl: string;
  readonly userPatchUrl: string;
}

export type SubmittingSettingsState = Extract<SettingsEditorState, { status: 'submitting' }>;

/**
 * The request a submitting state stands for.
 *
 * Total, and reachable from nowhere else: a request exists only where the
 * machine has already checked the held revision against the authority, so
 * there is no path that sends a patch from a stale review.
 *
 * Scope writability is deliberately absent. It is not a property of the review
 * this machine holds — it is a property of the dashboard the routes point at,
 * and it can change while a review sits open. The controller supplies it at
 * dispatch so the write refuses on the current reading rather than one captured
 * when the draft was confirmed.
 */
export function settingsSubmission(
  state: SubmittingSettingsState,
  routes: SettingsRoutes,
): Omit<SettingsMutationRequest, 'writability'> {
  return {
    scope: state.review.scope,
    expectedRevisionId: state.review.expectedRevisionId,
    readUrl: routes.readUrl,
    patchUrl: patchUrlFor(state.review.scope, routes),
    patch: state.review.patch,
  };
}

/** The review the dialog is about, if the machine is holding one. */
export function settingsReviewOf(state: SettingsEditorState): SettingsReview | null {
  return state.status === 'editor_unavailable' || state.status === 'editing'
    ? null
    : state.review;
}

/** Whether the review on screen is one the user has confirmed. */
export function settingsConfirmationHeld(state: SettingsEditorState): boolean {
  switch (state.status) {
    case 'confirmed':
    case 'submitting':
    case 'conflicted':
    case 'authority_withdrawn':
    case 'submit_failed':
      return true;
    case 'editor_unavailable':
    case 'editing':
    case 'reviewing':
    case 'review_superseded':
      return false;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/** Field refusals to mark the inputs with. Only a resting editor has any. */
export function settingsFieldErrors(
  state: SettingsEditorState,
): readonly SettingsValidationError[] {
  const rejection = settingsRejection(state);
  return rejection ? rejection.errors : [];
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
  if (state.status === 'editor_unavailable') return false;
  return planFor(state.authority, state.draft, scope).outcome !== 'unchanged';
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

function superseded(
  authority: SettingsEditor,
  draft: SettingsDraft,
  review: SettingsReview,
  currentRevisionId: string,
): SettingsEditorState {
  return { status: 'review_superseded', authority, draft, review, currentRevisionId };
}

function failed(
  state: SubmittingSettingsState,
  failure: SettingsSubmitFailure,
): SettingsEditorState {
  return {
    status: 'submit_failed',
    authority: state.authority,
    draft: state.draft,
    review: state.review,
    failure,
  };
}

function draftOf(authority: SettingsEditor): SettingsDraft {
  return { project: authority.project, user: authority.user };
}

type SettingsPlan = SettingsChangePlan<ProjectSettingsChangeSet | UserSettingsChangeSet>;

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
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

function reviewOf(
  scope: SettingsScope,
  plan: Extract<SettingsPlan, { outcome: 'ready' }>,
): SettingsReview {
  return {
    scope,
    expectedRevisionId: plan.expectedRevisionId,
    patch: plan.patch,
    reviewId: `${scope}@${plan.expectedRevisionId}#${stableJson(plan.patch)}`,
  };
}

function patchUrlFor(scope: SettingsScope, routes: SettingsRoutes): string {
  switch (scope) {
    case 'project':
      return routes.projectPatchUrl;
    case 'user':
      return routes.userPatchUrl;
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

function savedMessage(scope: SettingsScope): string {
  switch (scope) {
    case 'project':
      return 'Project settings saved';
    case 'user':
      return 'User settings saved';
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
    sameProjectValues(left.project, right.project) &&
    sameUserValues(left.user, right.user)
  );
}

function sameProjectValues(
  left: ProjectSettingsValues,
  right: ProjectSettingsValues,
): boolean {
  return (
    sameStringList(left.include, right.include) &&
    sameStringList(left.exclude, right.exclude) &&
    left.max_file_size === right.max_file_size &&
    left.extract_docstrings === right.extract_docstrings &&
    left.track_call_sites === right.track_call_sites &&
    left.git_ignore === right.git_ignore &&
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

function sameStringList(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}
