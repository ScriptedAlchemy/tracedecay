import { describe, expect, it } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import {
  initialSettingsEditorState,
  reduceSettingsEditor,
  settingsApplied,
  settingsRejection,
  settingsScopeDirty,
  settingsScopePlan,
  settingsSubmission,
  settingsSubmissionOf,
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

describe('settings editor direct Save', () => {
  it('captures the valid patch, revision, and request identity in one Save', () => {
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
    expect(settingsSubmissionOf(submitting)).toMatchObject({
      scope: 'project',
      expectedRevisionId: 'rev-42',
      patch: { max_file_size: 2_097_152 },
      idempotencyKey: IDEMPOTENCY_KEY,
    });
  });

  it('uses the profile revision and route for worker settings', () => {
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
        {
          type: 'save_requested',
          scope: 'code_index_workers',
          idempotencyKey: IDEMPOTENCY_KEY,
        },
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

  it('keeps an edited draft on its original revision when a newer snapshot arrives', () => {
    const drafted = run(initialSettingsEditorState(AUTHORITY), {
      type: 'project_drafted',
      values: draftMaxFileSize('2097152'),
    });
    const observed = run(drafted, {
      type: 'authority_observed',
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    });

    expect(observed).toBe(drafted);
    expect(settingsScopePlan(observed, 'project')).toMatchObject({
      outcome: 'ready',
      expectedRevisionId: 'rev-42',
      patch: { max_file_size: 2_097_152 },
    });
  });

  it('does not submit invalid or unchanged values', () => {
    const untouched = initialSettingsEditorState(AUTHORITY);
    expect(
      run(untouched, {
        type: 'save_requested',
        scope: 'project',
        idempotencyKey: IDEMPOTENCY_KEY,
      }),
    ).toBe(untouched);

    const invalid = run(
      untouched,
      { type: 'project_drafted', values: draftMaxFileSize('0') },
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
    );
    expect(invalid.status).toBe('editing');
    expect(settingsRejection(invalid)?.errors).toEqual([
      { field: 'max_file_size', message: 'max_file_size must be at least 1 byte' },
    ]);
    expect(settingsSubmissionOf(invalid)).toBeNull();
  });

  it('settles each saved scope without discarding the other drafts or their revisions', () => {
    let state = run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftMaxFileSize('2097152') },
      { type: 'user_drafted', values: { ...AUTHORITY.user, watcher_debounce: '15s' } },
      {
        type: 'code_index_workers_drafted',
        values: { ...AUTHORITY.codeIndexWorkers, code_index_workers: { mode: 'exact', workers: 3 } },
      },
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
    );
    const payload = structuredClone(fixturePayload());
    payload.project.configuration_revision_id = 'rev-43';
    payload.project.config.max_file_size = 2_097_152;
    // A concurrent writer changed both other resources. Their drafts still
    // belong to the original revisions, even when PATCH/readback sees this.
    payload.user.configuration_revision_id = 'user-rev-8';
    payload.user.watcher_debounce = '20s';
    payload.user.code_index_worker_configuration_revision_id = 'profile-worker-rev-8';
    payload.user.code_index_workers = { mode: 'exact', workers: 2 };
    state = settle(state, {
      outcome: 'success', scope: 'project', payload, revisionId: 'rev-43',
      resyncRecommended: false, restartRecommended: false,
      effectReceipt: null, effectReconciliation: null,
    });
    state = run(state, { type: 'authority_observed', authority: buildSettingsEditor(payload) });

    expect(settingsScopeDirty(state, 'project')).toBe(false);
    expect(settingsScopePlan(state, 'user')).toMatchObject({
      outcome: 'ready', expectedRevisionId: 'user-rev-7', patch: { watcher_debounce: '15s' },
    });
    expect(settingsScopePlan(state, 'code_index_workers')).toMatchObject({
      outcome: 'ready', expectedRevisionId: 'profile-worker-rev-7',
      patch: { code_index_workers: { mode: 'exact', workers: 3 } },
    });
  });

  it('refreshes clean resources independently while an edited resource keeps its base', () => {
    const drafted = run(initialSettingsEditorState(AUTHORITY), {
      type: 'project_drafted', values: draftMaxFileSize('2097152'),
    });
    const fresh: SettingsEditor = {
      ...withProjectRevision(AUTHORITY, 'rev-43'),
      project: draftMaxFileSize('4096'),
      userExpectedRevisionId: 'user-rev-8',
      user: { ...AUTHORITY.user, watcher_debounce: '20s' },
      codeIndexWorkerExpectedRevisionId: 'profile-worker-rev-8',
      codeIndexWorkers: {
        ...AUTHORITY.codeIndexWorkers, code_index_workers: { mode: 'exact', workers: 2 },
      },
    };
    const observed = run(drafted, { type: 'authority_observed', authority: fresh });

    expect(settingsScopePlan(observed, 'project')).toMatchObject({
      outcome: 'ready', expectedRevisionId: 'rev-42', patch: { max_file_size: 2_097_152 },
    });
    expect(observed).toMatchObject({
      authority: {
        userExpectedRevisionId: 'user-rev-8',
        codeIndexWorkerExpectedRevisionId: 'profile-worker-rev-8',
      },
      draft: { user: fresh.user, codeIndexWorkers: fresh.codeIndexWorkers },
    });
    expect(settingsScopeDirty(observed, 'user')).toBe(false);
    expect(settingsScopeDirty(observed, 'code_index_workers')).toBe(false);

    const submitted = expectSubmitting(run(observed, {
      type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY,
    }));
    const refreshed = expectSubmitting(run(submitted, {
      type: 'authority_observed',
      authority: { ...fresh, userExpectedRevisionId: 'user-rev-9' },
    }));
    expect(refreshed.submission).toBe(submitted.submission);
    expect(refreshed.authority.userExpectedRevisionId).toBe('user-rev-9');
    expect(settingsScopePlan(refreshed, 'project')).toMatchObject({ expectedRevisionId: 'rev-42' });
  });

  it('preserves the exact request identity across an uncertain transport retry', () => {
    const first = expectSubmitting(saveProjectChange());
    const original = settingsSubmission(first, ROUTES);
    const failed = settle(first, {
      outcome: 'offline',
      detail: 'The response was lost after dispatch.',
    });
    const retried = expectSubmitting(run(failed, { type: 'retry_requested' }));

    expect(settingsSubmission(retried, ROUTES)).toEqual(original);
  });

  it('does not retry an uncertain effect and requires current values instead', () => {
    const submitted = expectSubmitting(saveProjectChange());
    const uncertain = settle(submitted, {
      outcome: 'unsettled',
      detail: 'The write may have been applied.',
    });

    expect(uncertain.status).toBe('submit_failed');
    expect(run(uncertain, { type: 'retry_requested' })).toBe(uncertain);
    expect(settingsSubmissionOf(uncertain)).toMatchObject({
      idempotencyKey: IDEMPOTENCY_KEY,
      patch: { max_file_size: 2_097_152 },
    });
    const authority = withProjectRevision(AUTHORITY, 'rev-43');
    const reloaded = run(uncertain, { type: 'reloaded_from_authority', authority });
    expect(reloaded.status).toBe('editing');
    expect(settingsScopeDirty(reloaded, 'project')).toBe(false);
    expect(reloaded).toMatchObject({ authority: { projectExpectedRevisionId: 'rev-43' } });
  });

  it('keeps a stale-revision refusal actionable without overwriting the draft', () => {
    const conflicted = settle(expectSubmitting(saveProjectChange()), {
      outcome: 'conflict',
      expectedRevisionId: 'rev-42',
      actualRevisionId: 'rev-43',
    });

    expect(conflicted.status).toBe('conflicted');
    expect(conflicted).toMatchObject({
      conflict: { expectedRevisionId: 'rev-42', actualRevisionId: 'rev-43' },
      draft: { project: { max_file_size: '2097152' } },
    });
    expect(settingsApplied(conflicted)).toBeNull();
    expect(run(conflicted, { type: 'retry_requested' })).toBe(conflicted);
    expect(settingsScopeDirty(conflicted, 'project')).toBe(true);
  });

  it('keeps authorization refusals distinct and retries the same submission', () => {
    const denied = settle(expectSubmitting(saveProjectChange()), {
      outcome: 'not_dispatched',
      detail: 'Nothing was sent. This scope is read-only.',
    });
    expect(denied).toMatchObject({
      status: 'authority_withdrawn',
      detail: 'Nothing was sent. This scope is read-only.',
    });

    const retried = expectSubmitting(run(denied, { type: 'retry_requested' }));
    expect(settingsSubmission(retried, ROUTES)).toMatchObject({
      idempotencyKey: IDEMPOTENCY_KEY,
      expectedRevisionId: 'rev-42',
      patch: { max_file_size: 2_097_152 },
    });
  });

  it('keeps server validation attached to the field until the user edits again', () => {
    const rejected = settle(expectSubmitting(saveProjectChange()), {
      outcome: 'validation',
      detail: 'settings validation failed',
      errors: [
        { field: 'max_file_size', message: 'max_file_size is denied by the active policy' },
      ],
    });
    expect(settingsRejection(rejected)).toEqual({
      origin: 'server',
      scope: 'project',
      detail: 'settings validation failed',
      errors: [
        { field: 'max_file_size', message: 'max_file_size is denied by the active policy' },
      ],
    });
    expect(settingsSubmissionOf(rejected)).toBeNull();

    const corrected = run(rejected, {
      type: 'project_drafted',
      values: draftMaxFileSize('4096'),
    });
    expect(settingsRejection(corrected)).toBeNull();
    expect(settingsScopeDirty(corrected, 'project')).toBe(true);
  });

  it('records successful saves and their server-reported follow-up requirements', () => {
    const saved = settle(expectSubmitting(saveProjectChange()), {
      outcome: 'success',
      scope: 'project',
      payload: fixturePayload(),
      revisionId: 'rev-43',
      resyncRecommended: true,
      restartRecommended: false,
      effectReceipt: null,
      effectReconciliation: null,
    });

    expect(settingsApplied(saved)).toMatchObject({
      message: 'Project settings saved',
      revisionId: 'rev-43',
      resyncRecommended: true,
      restartRecommended: false,
    });
    expect(settingsRejection(saved)).toBeNull();
    expect(settingsSubmissionOf(saved)).toBeNull();
  });

  it('holds the submitted request and verdict while refreshes arrive', () => {
    const submitted = expectSubmitting(saveProjectChange());
    const observed = run(submitted, {
      type: 'authority_observed',
      authority: withProjectRevision(AUTHORITY, 'rev-43'),
    });

    expect(observed).toBe(submitted);
    expect(settingsSubmission(expectSubmitting(observed), ROUTES).expectedRevisionId).toBe(
      'rev-42',
    );
    expect(run(observed, {
      type: 'project_drafted',
      values: draftMaxFileSize('4096'),
    })).toBe(observed);
  });

  it('does not dispatch a write without an editable authority', () => {
    const unavailable = initialSettingsEditorState(null);
    expect(
      run(unavailable, {
        type: 'save_requested',
        scope: 'project',
        idempotencyKey: IDEMPOTENCY_KEY,
      }),
    ).toBe(unavailable);
    expect(settingsSubmissionOf(unavailable)).toBeNull();
  });
});

function saveProjectChange(): SubmittingSettingsState {
  return expectSubmitting(
    run(
      initialSettingsEditorState(AUTHORITY),
      { type: 'project_drafted', values: draftMaxFileSize('2097152') },
      { type: 'save_requested', scope: 'project', idempotencyKey: IDEMPOTENCY_KEY },
    ),
  );
}

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

function draftMaxFileSize(max_file_size: string): ProjectSettingsValues {
  return { ...AUTHORITY.project, max_file_size };
}

function withProjectRevision(
  authority: SettingsEditor,
  projectExpectedRevisionId: string,
): SettingsEditor {
  return { ...authority, projectExpectedRevisionId };
}

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
