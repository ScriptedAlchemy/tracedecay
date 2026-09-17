import { describe, expect, it } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import type { SettingsDraft } from './settingsEditorMachine.ts';
import { writableScopes, type WritableScopes } from './settingsGates.ts';
import { buildSettingsEditor, buildSettingsModel, readSettingsEnvelope } from './settingsModel.ts';
import {
  applyRequirement,
  bindingFor,
  boundKeys,
  effectiveRows,
  fieldEdited,
  filterEffectiveRows,
  withFieldValue,
  writeCapability,
} from './settingsRows.ts';

const read = readSettingsEnvelope(FIXTURES['/api/settings']);
if (read.outcome !== 'settings') throw new Error(`fixture is not settings: ${read.outcome}`);
const payload = read.payload;
const model = buildSettingsModel(payload);
const authority = buildSettingsEditor(payload);
if (!authority) throw new Error('the fixture must produce an editable snapshot');
const draft: SettingsDraft = {
  project: authority.project,
  user: authority.user,
  codeIndexWorkers: authority.codeIndexWorkers,
};

const ALL_WRITABLE: WritableScopes = writableScopes(
  [
    { kind: 'request_apply', operation: 'configuration_batch' },
    { kind: 'request_apply', operation: 'profile_code_index_worker_selection' },
  ],
  { state: 'writable', target: 'the active project' },
);

describe('effective rows', () => {
  it('flattens every served scalar under its full dotted key, in section order', () => {
    const rows = effectiveRows(model);
    expect(rows.every((row) => row.row.kind !== 'group')).toBe(true);
    expect(rows.length).toBe(model.settingCount);
    const keys = rows.map((row) => row.key);
    expect(keys).toContain('project.config.sync.auto_track_pr_poll_secs');
    expect(keys).toContain('environment.variables.TRACEDECAY_DATA_DIR');
    expect(keys).toContain('user.code_index_worker_status.effective_workers');
    expect(new Set(keys).size).toBe(keys.length);
    // Files first, then the environment overlay, then daemon-resolved state.
    const firstOf = (id: string) => keys.findIndex((key) => key.startsWith(`${id}.`));
    expect(firstOf('project')).toBeLessThan(firstOf('environment'));
    expect(firstOf('environment')).toBeLessThan(firstOf('storage'));
  });

  it('filters over key, value text, and served description', () => {
    const rows = effectiveRows(model);
    expect(filterEffectiveRows(rows, 'poll_secs').map((row) => row.key)).toEqual([
      'project.config.sync.auto_track_pr_poll_secs',
    ]);
    expect(filterEffectiveRows(rows, 'node_modules').map((row) => row.key)).toEqual([
      'project.config.exclude',
    ]);
    expect(filterEffectiveRows(rows, 'data directory').map((row) => row.key)).toEqual([
      'environment.variables.TRACEDECAY_DATA_DIR',
    ]);
    expect(filterEffectiveRows(rows, '')).toHaveLength(rows.length);
    expect(filterEffectiveRows(rows, 'zzz-nothing')).toEqual([]);
  });
});

describe('write bindings', () => {
  /** A binding names a key the daemon serves; a renamed contract field must
   * fail here rather than leave a phantom `editable` row nobody can reach. */
  it('binds only keys the fixture payload actually serves', () => {
    const served = new Set(effectiveRows(model).map((row) => row.key));
    for (const key of boundKeys()) {
      expect(served.has(key), `${key} is bound but not served`).toBe(true);
    }
  });

  it('answers no write path for keys no PATCH route addresses', () => {
    expect(bindingFor('storage.store_root')).toBeNull();
    expect(writeCapability('storage.store_root', ALL_WRITABLE)).toEqual({ kind: 'no_write_path' });
    expect(bindingFor('automation.enabled')).toBeNull();
    expect(bindingFor('project.configuration_revision_id')).toBeNull();
  });

  it('answers writable with the gate target, and locked with the gate reason', () => {
    expect(writeCapability('project.config.max_file_size', ALL_WRITABLE)).toMatchObject({
      kind: 'writable',
      target: 'the active project',
      binding: { scope: 'project', field: 'max_file_size', input: 'integer' },
    });
    expect(writeCapability('user.code_index_workers', ALL_WRITABLE)).toMatchObject({
      kind: 'writable',
      target: 'your TraceDecay profile',
    });

    const unauthorized = writableScopes([], { state: 'writable', target: 'x' });
    expect(writeCapability('project.config.include', unauthorized)).toMatchObject({
      kind: 'locked',
      gate: 'unauthorized',
      reason: 'this dashboard is not authorized to apply project settings',
    });
    expect(writeCapability('user.code_index_workers', unauthorized)).toMatchObject({
      kind: 'locked',
      gate: 'unauthorized',
      reason: 'this dashboard is not authorized to apply code-index worker settings',
    });

    const readOnly = writableScopes(
      [
        { kind: 'request_apply', operation: 'configuration_batch' },
        { kind: 'request_apply', operation: 'profile_code_index_worker_selection' },
      ],
      { state: 'read_only', reason: 'Other is not the active project.' },
    );
    expect(writeCapability('user.watcher_debounce', readOnly)).toMatchObject({
      kind: 'locked',
      gate: 'read_only',
      reason: 'Other is not the active project.',
    });
    // The profile worker resource never inherits the project gateway's refusal.
    expect(writeCapability('user.code_index_workers', readOnly)).toMatchObject({ kind: 'writable' });
  });

  it('states the apply requirement only where the product documents it', () => {
    expect(applyRequirement(bindingFor('user.code_index_workers')!)).toMatchObject({ kind: 'restart' });
    expect(applyRequirement(bindingFor('project.config.git_ignore')!)).toEqual({
      kind: 'reported_on_apply',
    });
    expect(applyRequirement(bindingFor('user.upload_enabled')!)).toEqual({
      kind: 'reported_on_apply',
    });
  });
});

describe('draft field access', () => {
  it('edits exactly one bound field and reports it edited against the authority', () => {
    const binding = bindingFor('project.config.max_file_size')!;
    expect(fieldEdited(draft, authority, binding)).toBe(false);
    const next = withFieldValue(draft, binding, '2097152');
    expect(next.project.max_file_size).toBe('2097152');
    expect(next.project.include).toEqual(authority.project.include);
    expect(next.user).toBe(draft.user);
    expect(fieldEdited(next, authority, binding)).toBe(true);
    expect(fieldEdited(next, authority, bindingFor('project.config.include')!)).toBe(false);
  });

  it('compares lists and worker selections by value', () => {
    const include = bindingFor('project.config.include')!;
    const same = withFieldValue(draft, include, [...authority.project.include]);
    expect(fieldEdited(same, authority, include)).toBe(false);
    const reordered = withFieldValue(draft, include, [...authority.project.include].reverse());
    expect(fieldEdited(reordered, authority, include)).toBe(true);

    const workers = bindingFor('user.code_index_workers')!;
    expect(fieldEdited(withFieldValue(draft, workers, { mode: 'automatic' }), authority, workers)).toBe(false);
    const exact = withFieldValue(draft, workers, { mode: 'exact', workers: 4 });
    expect(exact.codeIndexWorkers.code_index_workers).toEqual({ mode: 'exact', workers: 4 });
    expect(fieldEdited(exact, authority, workers)).toBe(true);
  });

  it('ignores a value of the wrong shape rather than corrupting the draft', () => {
    const boolean = bindingFor('project.config.git_ignore')!;
    expect(withFieldValue(draft, boolean, 'yes')).toEqual(draft);
    const workers = bindingFor('user.code_index_workers')!;
    expect(withFieldValue(draft, workers, { mode: 'exact' })).toEqual(draft);
    const globs = bindingFor('project.config.exclude')!;
    expect(withFieldValue(draft, globs, 'target/**')).toEqual(draft);
  });
});
