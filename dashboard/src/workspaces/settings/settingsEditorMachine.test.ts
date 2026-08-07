/**
 * What the editor machine forbids.
 *
 * Each case below names a combination the six mirrored `useState` slices could
 * hold at once — a confirmation beside a review it was not given for, a saved
 * notice above a change still under review, a submit built from a revision the
 * resource had already moved past — and shows either that the state cannot be
 * built or that the machine refuses it and says why.
 *
 * No DOM, no transport: the machine is a pure function and is exercised as one.
 */

import { describe, expect, it } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import {
  initialSettingsEditorState,
  reduceSettingsEditor,
  settingsApplied,
  settingsConfirmationHeld,
  settingsFieldErrors,
  settingsRejection,
  settingsReviewOf,
  settingsScopeDirty,
  settingsSubmission,
  type SettingsEditorAction,
  type SettingsEditorState,
  type SettingsRoutes,
  type SubmittingSettingsState,
} from './settingsEditorMachine.ts';
import {
  buildSettingsEditor,
  readSettingsEnvelope,
  type ProjectSettingsValues,
  type SettingsEditor,
} from './settingsModel.ts';
import type { SettingsMutationResult } from './settingsMutation.ts';

const ROUTES: SettingsRoutes = {
  readUrl: '/api/settings',
  projectPatchUrl: '/api/settings/project',
  userPatchUrl: '/api/settings/user',
};

const AUTHORITY = fixtureAuthority();
const IDEMPOTENCY_KEY = 'idempotency.dashboard-settings.fixture';

describe('settings editor: reaching a confirmed change', () => {
  it('takes a validated change from editing through review to confirmation', () => {
    const confirmed = run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftMaxFileSize('2097152') },
      {
        type: 'review_requested',
        scope: 'project',
        idempotencyKey: IDEMPOTENCY_KEY,
      },
      { type: 'confirmation_set', confirmed: true },
    );

    expect(confirmed.status).toBe('confirmed');
    expect(settingsConfirmationHeld(confirmed)).toBe(true);
    expect(settingsReviewOf(confirmed)).toMatchObject({
      scope: 'project',
      expectedRevisionId: 'rev-42',
      idempotencyKey: IDEMPOTENCY_KEY,
      patch: { max_file_size: 2_097_152 },
    });
    expect(settingsScopeDirty(confirmed, 'project')).toBe(true);
    expect(settingsScopeDirty(confirmed, 'user')).toBe(false);
  });

  it('issues a submit that names the held revision and the scope authority', () => {
    const submitting = expectSubmitting(
      run(confirmedProjectChange(), { type: 'submit_started' }),
    );

    expect(settingsSubmission(submitting, ROUTES)).toEqual({
      scope: 'project',
      expectedRevisionId: 'rev-42',
      idempotencyKey: IDEMPOTENCY_KEY,
      readUrl: '/api/settings',
      patchUrl: '/api/settings/project',
      patch: { max_file_size: 2_097_152 },
    });
  });
});

describe('settings editor: a confirmation cannot outlive its review', () => {
  // Previously: `confirmed` was a boolean beside `review`. A refetch replaced
  // the draft through an effect and left both untouched, so the next apply
  // sent a patch the user had confirmed against a revision that was gone.
  it('supersedes a confirmation when the resource moves under it', () => {
    const moved = run(confirmedProjectChange(), {
      type: 'authority_observed',
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    });

    expect(moved.status).toBe('review_superseded');
    expect(moved).toMatchObject({ currentRevisionId: 'rev-43' });
    expect(settingsConfirmationHeld(moved)).toBe(false);
    // And it stays superseded: there is no path from here to a write.
    expect(run(moved, { type: 'submit_started' }).status).toBe('review_superseded');
  });

  // Previously: the review dialog held a patch computed from a draft the
  // editor had already replaced. Editing behind an open review left the two
  // describing different changes.
  it('drops the review entirely when the draft it described is edited', () => {
    const edited = run(confirmedProjectChange(), {
      type: 'project_drafted',
      values: draftMaxFileSize('4096'),
    });

    expect(edited.status).toBe('editing');
    expect(settingsReviewOf(edited)).toBeNull();
    expect(settingsConfirmationHeld(edited)).toBe(false);
  });

  // The revision alone is not the binding: the confirmation is for one exact
  // change. A resource whose values moved without moving its revision must not
  // inherit a confirmation given for the values it used to hold.
  it('refuses a submit when replanning no longer produces the confirmed change', () => {
    const restated = run(confirmedProjectChange(), {
      type: 'authority_observed',
      // Same revision, but the authority now already holds the confirmed value.
      authority: withProjectValues(AUTHORITY, { max_file_size: '2097152' }),
    });
    expect(restated.status).toBe('confirmed');

    const attempted = run(restated, { type: 'submit_started' });

    expect(attempted.status).toBe('review_superseded');
    expect(attempted).toMatchObject({ currentRevisionId: 'rev-42' });
  });

  it('cannot hold a confirmation without a review to hold it for', () => {
    const editing = initialSettingsEditorState(AUTHORITY);

    const attempted = run(editing, { type: 'confirmation_set', confirmed: true });

    expect(attempted).toBe(editing);
    expect(settingsConfirmationHeld(attempted)).toBe(false);
    expect(settingsReviewOf(attempted)).toBeNull();
  });

  it('returns to an unconfirmed review when the confirmation is withdrawn', () => {
    const withdrawn = run(confirmedProjectChange(), {
      type: 'confirmation_set',
      confirmed: false,
    });

    expect(withdrawn.status).toBe('reviewing');
    expect(settingsConfirmationHeld(withdrawn)).toBe(false);
    expect(run(withdrawn, { type: 'submit_started' }).status).toBe('reviewing');
  });
});

describe('settings editor: a stale revision cannot reach the wire', () => {
  // Even hand-built — the shape below is the one the mirrored slices produced
  // routinely — a state whose review names a revision the authority has moved
  // past yields no request.
  it('supersedes a submit issued from a review that holds a stale revision', () => {
    const stale: SettingsEditorState = {
      ...confirmedProjectChange(),
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    };

    const attempted = run(stale, { type: 'submit_started' });

    expect(attempted.status).not.toBe('submitting');
    expect(attempted.status).toBe('review_superseded');
    expect(attempted).toMatchObject({
      currentRevisionId: 'rev-43',
      review: { expectedRevisionId: 'rev-42' },
    });
  });

  it('offers the current values instead of a retry once a review is superseded', () => {
    const superseded = run(
      { ...confirmedProjectChange(), authority: withProjectRevision(AUTHORITY, 'rev-43') },
      { type: 'submit_started' },
      { type: 'reloaded_from_authority' },
    );

    expect(superseded.status).toBe('editing');
    expect(settingsReviewOf(superseded)).toBeNull();
    expect(settingsScopeDirty(superseded, 'project')).toBe(false);
  });

  it('keeps a write in flight when a fresh read lands underneath it', () => {
    const inFlight = run(
      confirmedProjectChange(),
      { type: 'submit_started' },
      { type: 'authority_observed', authority: withProjectRevision(AUTHORITY, 'rev-43') },
    );

    expect(inFlight.status).toBe('submitting');
    // The request already named rev-42; the pre-flight check owns the verdict.
    expect(settingsSubmission(expectSubmitting(inFlight), ROUTES).expectedRevisionId).toBe(
      'rev-42',
    );
  });

  it('cannot be dismissed out of a write that is already in flight', () => {
    const submitting = run(confirmedProjectChange(), { type: 'submit_started' });

    expect(run(submitting, { type: 'review_dismissed' })).toBe(submitting);
  });
});

describe('settings editor: each verdict is its own state', () => {
  const submitting = () => run(confirmedProjectChange(), { type: 'submit_started' });

  // A withdrawn authority means nothing was attempted; a failed write means
  // something was and did not land. The old surface rendered both as red text
  // from the same mutation result.
  it('lands a withdrawn authority somewhere a failed write cannot reach', () => {
    const withdrawn = settle(submitting(), {
      outcome: 'unavailable',
      detail: 'Nothing was applied: configuration authority is unavailable.',
    });
    const failed = settle(submitting(), {
      outcome: 'error',
      detail: 'Settings update failed (HTTP 500).',
    });

    expect(withdrawn.status).toBe('authority_withdrawn');
    expect(withdrawn).toMatchObject({
      detail: 'Nothing was applied: configuration authority is unavailable.',
    });
    expect(failed.status).toBe('submit_failed');
    expect(failed).toMatchObject({ failure: { kind: 'error' } });
    expect(withdrawn.status).not.toBe(failed.status);
    // Neither is a save.
    expect(settingsApplied(withdrawn)).toBeNull();
    expect(settingsApplied(failed)).toBeNull();
  });

  it('reuses the held caller key when a response-loss retry resubmits the same intent', () => {
    const first = submitting();
    const failed = settle(first, {
      outcome: 'offline',
      detail: 'The response was lost after dispatch.',
    });
    const retried = expectSubmitting(run(failed, { type: 'submit_started' }));

    expect(settingsSubmission(first, ROUTES).idempotencyKey).toBe(IDEMPOTENCY_KEY);
    expect(settingsSubmission(retried, ROUTES).idempotencyKey).toBe(IDEMPOTENCY_KEY);
  });

  it('keeps server validation as typed field state rather than a generic failure', () => {
    const rejected = settle(submitting(), {
      outcome: 'validation',
      detail: 'settings validation failed',
      errors: [
        { field: 'max_file_size', message: 'max_file_size is denied by the active policy' },
      ],
    });

    expect(rejected.status).toBe('editing');
    expect(rejected.status).not.toBe('submit_failed');
    expect(settingsRejection(rejected)).toEqual({
      origin: 'server',
      scope: 'project',
      detail: 'settings validation failed',
      errors: [
        { field: 'max_file_size', message: 'max_file_size is denied by the active policy' },
      ],
    });
    expect(settingsFieldErrors(rejected)).toHaveLength(1);
    expect(settingsApplied(rejected)).toBeNull();
    expect(settingsReviewOf(rejected)).toBeNull();
  });

  it('distinguishes a refused revision from a superseded one', () => {
    const conflicted = settle(submitting(), {
      outcome: 'conflict',
      expectedRevisionId: 'rev-42',
      actualRevisionId: 'rev-43',
    });

    expect(conflicted.status).toBe('conflicted');
    expect(conflicted).toMatchObject({
      conflict: { expectedRevisionId: 'rev-42', actualRevisionId: 'rev-43' },
    });
    expect(settingsApplied(conflicted)).toBeNull();
  });

  it('records a protocol violation against the authority that committed it', () => {
    const failed = settle(submitting(), {
      outcome: 'protocol_error',
      authority: 'PATCH /api/settings/project',
      detail: 'PATCH /api/settings/project violated the settings contract: expected JSON.',
    });

    expect(failed).toMatchObject({
      status: 'submit_failed',
      failure: { kind: 'protocol_error', authority: 'PATCH /api/settings/project' },
    });
  });

  it('ignores a verdict for a write this state did not issue', () => {
    const editing = initialSettingsEditorState(AUTHORITY);

    const settled = settle(editing, {
      outcome: 'success',
      scope: 'project',
      payload: fixturePayload(),
      revisionId: 'rev-43',
      resyncRecommended: false,
      restartRecommended: false,
      effectReceipt: null,
    });

    expect(settled).toBe(editing);
    expect(settingsApplied(settled)).toBeNull();
  });
});

describe('settings editor: a save and a pending change cannot be shown at once', () => {
  const applied = () =>
    settle(run(confirmedProjectChange(), { type: 'submit_started' }), {
      outcome: 'success',
      scope: 'project',
      payload: fixturePayload(),
      revisionId: 'rev-43',
      resyncRecommended: true,
      restartRecommended: false,
      effectReceipt: null,
    });

  it('records the save the authority reported, and nothing it did not', () => {
    const state = applied();

    expect(state.status).toBe('editing');
    expect(settingsApplied(state)).toEqual({
      scope: 'project',
      message: 'Project settings saved',
      resyncRecommended: true,
      restartRecommended: false,
    });
    expect(settingsRejection(state)).toBeNull();
    expect(settingsReviewOf(state)).toBeNull();
  });

  // Previously: `notice` survived every subsequent review, so the editor could
  // say "Project settings saved" directly above an unsent change.
  it('drops the save notice as soon as another change goes under review', () => {
    const reviewing = run(
      applied(),
      { type: 'project_drafted', values: draftMaxFileSize('4096') },
      {
        type: 'review_requested',
        scope: 'project',
        idempotencyKey: IDEMPOTENCY_KEY,
      },
    );

    expect(reviewing.status).toBe('reviewing');
    expect(settingsApplied(reviewing)).toBeNull();
  });

  it('keeps the save across the refetch it triggered', () => {
    const refetched = run(applied(), {
      type: 'authority_observed',
      authority: withProjectRevision(
        withProjectValues(AUTHORITY, { max_file_size: '2097152' }),
        'rev-43',
      ),
    });

    expect(settingsApplied(refetched)).toMatchObject({ message: 'Project settings saved' });
    expect(settingsScopeDirty(refetched, 'project')).toBe(false);
  });

  it('does not carry a refusal across a snapshot that replaced the values it judged', () => {
    const rejected = run(initialSettingsEditorState(AUTHORITY), {
      type: 'review_requested',
      scope: 'project',
      idempotencyKey: IDEMPOTENCY_KEY,
    });
    expect(settingsRejection(rejected)).toMatchObject({ origin: 'client' });

    const refetched = run(rejected, {
      type: 'authority_observed',
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    });

    expect(settingsRejection(refetched)).toBeNull();
  });

  it('closing the review does not undo a verdict the editor is resting on', () => {
    const state = applied();

    expect(run(state, { type: 'review_dismissed' })).toBe(state);
    expect(settingsApplied(run(state, { type: 'review_dismissed' }))).not.toBeNull();
  });
});

describe('settings editor: refusing to review what cannot be sent', () => {
  it('refuses an unchanged draft as a stated rejection rather than an empty review', () => {
    const state = run(initialSettingsEditorState(AUTHORITY), {
      type: 'review_requested',
      scope: 'user',
      idempotencyKey: IDEMPOTENCY_KEY,
    });

    expect(settingsReviewOf(state)).toBeNull();
    expect(settingsRejection(state)).toEqual({
      origin: 'client',
      scope: 'user',
      errors: [{ field: 'user', message: 'No user settings have changed.' }],
    });
  });

  it('refuses values the daemon would reject, without building a review', () => {
    const state = run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftPollSecs('59') },
      {
        type: 'review_requested',
        scope: 'project',
        idempotencyKey: IDEMPOTENCY_KEY,
      },
    );

    expect(settingsReviewOf(state)).toBeNull();
    expect(settingsFieldErrors(state)).toEqual([
      {
        field: 'auto_track_pr_poll_secs',
        message: 'auto_track_pr_poll_secs must be at least 60 seconds',
      },
    ]);
  });

  it('has no draft, review, or verdict at all while the read names no revision', () => {
    const unavailable = initialSettingsEditorState(null);

    expect(unavailable).toEqual({ status: 'editor_unavailable' });
    expect(settingsReviewOf(unavailable)).toBeNull();
    expect(settingsApplied(unavailable)).toBeNull();
    expect(settingsFieldErrors(unavailable)).toEqual([]);
    expect(settingsScopeDirty(unavailable, 'project')).toBe(false);
    for (const action of [
      {
        type: 'review_requested',
        scope: 'project',
        idempotencyKey: IDEMPOTENCY_KEY,
      },
      { type: 'confirmation_set', confirmed: true },
      { type: 'submit_started' },
    ] satisfies SettingsEditorAction[]) {
      expect(run(unavailable, action)).toBe(unavailable);
    }
  });

  it('withdraws the whole editor when a later read names no revision', () => {
    const withdrawn = run(confirmedProjectChange(), {
      type: 'authority_observed',
      authority: null,
    });

    expect(withdrawn).toEqual({ status: 'editor_unavailable' });
  });

  it('leaves the draft alone when a read reports the snapshot it already holds', () => {
    const drafted = run(initialSettingsEditorState(AUTHORITY), {
      type: 'project_drafted',
      values: draftMaxFileSize('2097152'),
    });

    const observed = run(drafted, {
      type: 'authority_observed',
      authority: fixtureAuthority(),
    });

    expect(observed).toBe(drafted);
    expect(settingsScopeDirty(observed, 'project')).toBe(true);
  });
});

/* ----------------------------------------------------------------- setup --*/

function run(
  state: SettingsEditorState,
  ...actions: readonly SettingsEditorAction[]
): SettingsEditorState {
  return actions.reduce(reduceSettingsEditor, state);
}

function settle(
  state: SettingsEditorState,
  result: SettingsMutationResult,
): SettingsEditorState {
  return reduceSettingsEditor(state, { type: 'submit_settled', result });
}

function expectSubmitting(state: SettingsEditorState): SubmittingSettingsState {
  expect(state.status).toBe('submitting');
  if (state.status !== 'submitting') throw new Error(`expected submitting, got ${state.status}`);
  return state;
}

/** Narrowed, so a test can restate one field of it and still hold a real state. */
function confirmedProjectChange(): Extract<SettingsEditorState, { status: 'confirmed' }> {
  const state = run(
    initialSettingsEditorState(AUTHORITY),
    { type: 'project_drafted', values: draftMaxFileSize('2097152') },
    {
      type: 'review_requested',
      scope: 'project',
      idempotencyKey: IDEMPOTENCY_KEY,
    },
    { type: 'confirmation_set', confirmed: true },
  );
  if (state.status !== 'confirmed') throw new Error(`expected confirmed, got ${state.status}`);
  return state;
}

function draftMaxFileSize(max_file_size: string): ProjectSettingsValues {
  return { ...AUTHORITY.project, max_file_size };
}

function draftPollSecs(auto_track_pr_poll_secs: string): ProjectSettingsValues {
  return { ...AUTHORITY.project, auto_track_pr_poll_secs };
}

function withProjectRevision(
  authority: SettingsEditor,
  projectExpectedRevisionId: string,
): SettingsEditor {
  return { ...authority, projectExpectedRevisionId };
}

function withProjectValues(
  authority: SettingsEditor,
  values: Partial<ProjectSettingsValues>,
): SettingsEditor {
  return { ...authority, project: { ...authority.project, ...values } };
}

/** The authority as the generated contract yields it from the served fixture. */
function fixtureAuthority(): SettingsEditor {
  const authority = buildSettingsEditor(fixturePayload());
  if (!authority) throw new Error('the /api/settings fixture carries no editable authority');
  return authority;
}

function fixturePayload() {
  const read = readSettingsEnvelope(FIXTURES['/api/settings']);
  if (read.outcome !== 'settings') {
    throw new Error(`the /api/settings fixture does not satisfy SettingsPayloadV1: ${read.outcome}`);
  }
  return read.payload;
}
