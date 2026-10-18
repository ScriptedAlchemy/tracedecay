/**
 * What the editor machine forbids.
 *
 * Each case below names a combination the old mirrored `useState` slices could
 * hold at once, a save built from a revision the resource had already moved
 * past, a retry under a new identity, a saved notice above a change still
 * unsent, and shows either that the state cannot be built or that the machine
 * refuses it and says why.
 *
 * No DOM, no transport: the machine is a pure function and is exercised as one.
 */

import { describe, expect, it } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import {
  initialSettingsEditorState,
  reduceSettingsEditor,
  settingsApplied,
  settingsAttemptOf,
  settingsRejection,
  settingsScopeDirty,
  settingsScopePlan,
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
  codeIndexWorkerReadUrl: '/api/settings',
  projectPatchUrl: '/api/settings/project',
  userPatchUrl: '/api/settings/user',
  codeIndexWorkerPatchUrl: '/api/settings/user/code-index-workers',
};

const AUTHORITY = fixtureAuthority();
const IDEMPOTENCY_KEY = 'idempotency.dashboard-settings.fixture';

describe('settings editor: saving directly', () => {
  it('takes a valid change from editing straight to a write, with no review step', () => {
    const submitting = expectSubmitting(
      run(
        initialSettingsEditorState(AUTHORITY),
        { type: 'project_drafted', values: draftMaxFileSize('2097152') },
        { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
      ),
    );

    expect(settingsSubmission(submitting, ROUTES)).toEqual({
      scope: 'project',
      expectedRevisionId: 'rev-42',
      idempotencyKey: IDEMPOTENCY_KEY,
      readUrl: '/api/settings',
      patchUrl: '/api/settings/project',
      patch: { max_file_size: 2_097_152 },
    });
    expect(settingsScopeDirty(submitting, 'project')).toBe(true);
    expect(settingsScopeDirty(submitting, 'user')).toBe(false);
  });

  it('holds a worker-only change against the independent ProfileSessions revision', () => {
    const submitting = expectSubmitting(
      run(
        initialSettingsEditorState(AUTHORITY),
        {
          type: 'code_index_workers_drafted',
          values: {
            ...AUTHORITY.codeIndexWorkers,
            code_index_workers: { mode: 'exact', workers: 4 },
          },
        },
        { type: 'save_requested', scope: 'code_index_workers', idempotencyKey: IDEMPOTENCY_KEY },
      ),
    );

    expect(settingsSubmission(submitting, ROUTES)).toEqual({
      scope: 'code_index_workers',
      expectedRevisionId: 'profile-worker-rev-7',
      idempotencyKey: IDEMPOTENCY_KEY,
      readUrl: '/api/settings',
      patchUrl: '/api/settings/user/code-index-workers',
      patch: { code_index_workers: { mode: 'exact', workers: 4 } },
    });
  });

  /** The inline editor states validity while the value is still being typed,
   * from the same plan `save_requested` sends, never a second one. */
  it('replans a scope live without starting a write', () => {
    const untouched = initialSettingsEditorState(AUTHORITY);
    expect(settingsScopePlan(untouched, 'project')).toMatchObject({ outcome: 'unchanged' });
    expect(settingsScopePlan(initialSettingsEditorState(null), 'project')).toBeNull();

    const edited = run(untouched, {
      type: 'project_drafted',
      values: draftMaxFileSize('2097152'),
    });
    expect(edited.status).toBe('editing');
    expect(settingsScopePlan(edited, 'project')).toMatchObject({
      outcome: 'ready',
      expectedRevisionId: 'rev-42',
      patch: { max_file_size: 2_097_152 },
    });
    expect(settingsAttemptOf(edited)).toBeNull();

    const invalid = run(untouched, { type: 'project_drafted', values: draftMaxFileSize('0') });
    expect(settingsScopePlan(invalid, 'project')).toMatchObject({
      outcome: 'invalid',
      errors: [{ field: 'max_file_size', message: 'max_file_size must be at least 1 byte' }],
    });
    expect(settingsRejection(invalid)).toBeNull();
  });

  it('refuses values the daemon would reject, without sending anything', () => {
    const state = run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftPollSecs('59') },
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
    );

    expect(state.status).toBe('editing');
    expect(settingsAttemptOf(state)).toBeNull();
    expect(settingsRejection(state)?.errors).toEqual([
      {
        field: 'auto_track_pr_poll_secs',
        message: 'auto_track_pr_poll_secs must be at least 60 seconds',
      },
    ]);
  });

  it('sends nothing for a draft that matches the authority', () => {
    const untouched = initialSettingsEditorState(AUTHORITY);

    expect(
      run(untouched, { type: 'save_requested', scope: 'user', idempotencyKey: IDEMPOTENCY_KEY }),
    ).toBe(untouched);
  });

  it('has no draft, attempt, or verdict at all while the read names no revision', () => {
    const unavailable = initialSettingsEditorState(null);

    expect(unavailable).toEqual({ status: 'editor_unavailable' });
    expect(settingsAttemptOf(unavailable)).toBeNull();
    expect(settingsApplied(unavailable)).toBeNull();
    expect(settingsRejection(unavailable)).toBeNull();
    expect(settingsScopeDirty(unavailable, 'project')).toBe(false);
    for (const action of [
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
      { type: 'retry_requested' },
    ] satisfies SettingsEditorAction[]) {
      expect(run(unavailable, action)).toBe(unavailable);
    }
  });

  it('withdraws the whole editor when a later read names no revision', () => {
    const withdrawn = run(failedProjectSave('offline'), {
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

describe('settings editor: a stale revision never reaches the wire', () => {
  it('ends a failed save in conflict when the resource moves under it', () => {
    const moved = run(failedProjectSave('offline'), {
      type: 'authority_observed',
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    });

    expect(moved).toMatchObject({
      status: 'conflicted',
      conflict: { expectedRevisionId: 'rev-42', actualRevisionId: 'rev-43' },
    });
    // And it stays there: neither a retry nor a new save leaves a conflict.
    expect(run(moved, { type: 'retry_requested' })).toBe(moved);
    expect(
      run(moved, { type: 'save_requested', scope: 'project', idempotencyKey: 'another-key' }),
    ).toBe(moved);
  });

  it('refuses a retry when replanning no longer produces the attempted change', () => {
    const restated = run(failedProjectSave('offline'), {
      type: 'authority_observed',
      // Same revision, but the authority now already holds the attempted value.
      authority: withProjectValues(AUTHORITY, { max_file_size: '2097152' }),
    });
    expect(restated.status).toBe('failed');

    const retried = run(restated, { type: 'retry_requested' });

    expect(retried.status).toBe('conflicted');
    expect(retried).toMatchObject({ conflict: { actualRevisionId: 'rev-42' } });
  });

  it('offers the current values instead of a retry once a save is in conflict', () => {
    const reloaded = run(
      conflictedProjectSave(),
      { type: 'authority_observed', authority: withProjectRevision(AUTHORITY, 'rev-43') },
      { type: 'reloaded_from_authority' },
    );

    expect(reloaded.status).toBe('editing');
    expect(settingsAttemptOf(reloaded)).toBeNull();
    expect(settingsScopeDirty(reloaded, 'project')).toBe(false);
  });

  it('keeps a write in flight when a fresh read lands underneath it', () => {
    const inFlight = run(submittingProjectSave(), {
      type: 'authority_observed',
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    });

    expect(inFlight.status).toBe('submitting');
    // The request already named rev-42; the pre-flight check owns the verdict.
    expect(settingsSubmission(expectSubmitting(inFlight), ROUTES).expectedRevisionId).toBe(
      'rev-42',
    );
  });

  /** Leaving `submitting` would make `settleSave` drop the verdict when it
   * lands: a conflict or refusal for scope A would vanish because the reader
   * touched a scope-B field meanwhile. Edits and new saves wait instead. */
  it('cannot be redrafted or re-saved out of a write that is already in flight', () => {
    const submitting = submittingProjectSave();

    const redrafted = run(submitting, {
      type: 'user_drafted',
      values: { ...AUTHORITY.user, watcher_debounce: '15s' },
    });
    expect(redrafted).toBe(submitting);

    const resaved = run(submitting, {
      type: 'save_requested',
      scope: 'user',
      idempotencyKey: 'another-key',
    });
    expect(resaved).toBe(submitting);
  });

  it('refuses a verdict for a write the state did not issue', () => {
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

describe('settings editor: each outcome is its own state', () => {
  // A scope or authority that declined the write means nothing was attempted;
  // a failure with no verdict means something was and the outcome is unknown.
  it('keeps a write that was never sent apart from one whose outcome is unknown', () => {
    const notSent = settle(submittingProjectSave(), {
      outcome: 'unavailable',
      detail: 'Nothing was applied: configuration authority is unavailable.',
    });
    const readOnly = settle(submittingProjectSave(), {
      outcome: 'read_only_scope',
      detail: 'Nothing was applied: this project is read-only.',
    });
    const unknown = settle(submittingProjectSave(), {
      outcome: 'error',
      detail: 'Settings update failed (HTTP 500).',
    });

    expect(notSent).toMatchObject({
      status: 'failed',
      failure: {
        kind: 'not_sent',
        detail: 'Nothing was applied: configuration authority is unavailable.',
      },
    });
    expect(readOnly).toMatchObject({ status: 'failed', failure: { kind: 'not_sent' } });
    expect(unknown).toMatchObject({ status: 'failed', failure: { kind: 'error' } });
    // None of them is a save.
    expect(settingsApplied(notSent)).toBeNull();
    expect(settingsApplied(unknown)).toBeNull();
  });

  it('retries a failed save under the identity it first went out with', () => {
    const first = submittingProjectSave();
    const failed = settle(first, {
      outcome: 'offline',
      detail: 'The response was lost after dispatch.',
    });
    const retried = expectSubmitting(run(failed, { type: 'retry_requested' }));

    expect(settingsSubmission(first, ROUTES).idempotencyKey).toBe(IDEMPOTENCY_KEY);
    expect(settingsSubmission(retried, ROUTES)).toEqual(settingsSubmission(first, ROUTES));
  });

  it('starts a new request, not a retry, once the draft is edited after a failure', () => {
    const failed = failedProjectSave('offline');

    const edited = run(failed, { type: 'project_drafted', values: draftMaxFileSize('4096') });

    expect(edited.status).toBe('editing');
    expect(settingsAttemptOf(edited)).toBeNull();
    expect(run(edited, { type: 'retry_requested' })).toBe(edited);
    const resent = expectSubmitting(
      run(edited, { type: 'save_requested', scope: 'project', idempotencyKey: 'second-key' }),
    );
    expect(settingsSubmission(resent, ROUTES)).toMatchObject({
      idempotencyKey: 'second-key',
      patch: { max_file_size: 4096 },
    });
  });

  it('keeps server validation as typed field state rather than a generic failure', () => {
    const rejected = settle(submittingProjectSave(), {
      outcome: 'validation',
      detail: 'settings validation failed',
      errors: [
        { field: 'max_file_size', message: 'max_file_size is denied by the active policy' },
      ],
    });

    expect(rejected.status).toBe('editing');
    expect(settingsRejection(rejected)).toEqual({
      origin: 'server',
      scope: 'project',
      detail: 'settings validation failed',
      errors: [
        { field: 'max_file_size', message: 'max_file_size is denied by the active policy' },
      ],
    });
    expect(settingsApplied(rejected)).toBeNull();
    expect(settingsAttemptOf(rejected)).toBeNull();
  });

  it('records a refused revision as a conflict that nothing was written for', () => {
    const conflicted = conflictedProjectSave();

    expect(conflicted).toMatchObject({
      status: 'conflicted',
      conflict: { expectedRevisionId: 'rev-42', actualRevisionId: 'rev-43' },
    });
    expect(settingsApplied(conflicted)).toBeNull();
  });

  it('records a protocol violation against the authority that committed it', () => {
    const failed = settle(submittingProjectSave(), {
      outcome: 'protocol_error',
      authority: 'PATCH /api/settings/project',
      detail: 'PATCH /api/settings/project violated the settings contract: expected JSON.',
    });

    expect(failed).toMatchObject({
      status: 'failed',
      failure: { kind: 'protocol_error', authority: 'PATCH /api/settings/project' },
    });
  });
});

describe('settings editor: a save and a pending change cannot be shown at once', () => {
  const applied = () =>
    settle(submittingProjectSave(), {
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
      revisionId: 'rev-43',
      resyncRecommended: true,
      restartRecommended: false,
    });
    expect(settingsRejection(state)).toBeNull();
    expect(settingsAttemptOf(state)).toBeNull();
  });

  it('drops the save notice as soon as another change is saved', () => {
    const saving = run(
      applied(),
      { type: 'project_drafted', values: draftMaxFileSize('4096') },
      { type: 'save_requested', scope: 'project', idempotencyKey: 'second-key' },
    );

    expect(saving.status).toBe('submitting');
    expect(settingsApplied(saving)).toBeNull();
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

  /** A refusal judged the draft as it was; once the draft moves the refusal
   * describes nothing, and the live plan takes over. A completed write, by
   * contrast, stays true across an edit. */
  it('drops a resting refusal on the next edit but keeps a completed write', () => {
    const rejected = run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftMaxFileSize('0') },
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
    );
    expect(settingsRejection(rejected)).not.toBeNull();
    const retyped = run(rejected, { type: 'project_drafted', values: draftMaxFileSize('4096') });
    expect(settingsRejection(retyped)).toBeNull();
    expect(settingsScopeDirty(retyped, 'project')).toBe(true);

    const edited = run(applied(), { type: 'project_drafted', values: draftMaxFileSize('4096') });
    expect(settingsApplied(edited)).not.toBeNull();
  });

  it('does not carry a refusal across a snapshot that replaced the values it judged', () => {
    const rejected = run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftMaxFileSize('0') },
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
    );
    expect(settingsRejection(rejected)).toMatchObject({ origin: 'client' });

    const refetched = run(rejected, {
      type: 'authority_observed',
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    });

    expect(settingsRejection(refetched)).toBeNull();
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
  return reduceSettingsEditor(state, { type: 'save_settled', result });
}

function expectSubmitting(state: SettingsEditorState): SubmittingSettingsState {
  expect(state.status).toBe('submitting');
  if (state.status !== 'submitting') throw new Error(`expected submitting, got ${state.status}`);
  return state;
}

/** A project change saved straight from the draft, awaiting its verdict. */
function submittingProjectSave(): SubmittingSettingsState {
  return expectSubmitting(
    run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftMaxFileSize('2097152') },
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
    ),
  );
}

function failedProjectSave(kind: 'offline' | 'error'): SettingsEditorState {
  const failed = settle(submittingProjectSave(), { outcome: kind, detail: `${kind} detail` });
  expect(failed.status).toBe('failed');
  return failed;
}

function conflictedProjectSave(): SettingsEditorState {
  const conflicted = settle(submittingProjectSave(), {
    outcome: 'conflict',
    expectedRevisionId: 'rev-42',
    actualRevisionId: 'rev-43',
  });
  expect(conflicted.status).toBe('conflicted');
  return conflicted;
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
