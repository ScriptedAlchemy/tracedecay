/**
 * The effective-configuration table's read model: one flat row per served
 * scalar, addressed by its full dotted key, plus what this dashboard can say
 * about writing it.
 *
 * Two authorities meet here and stay distinguishable. The PROVENANCE of a row
 * is whatever `/api/settings` stated (`settingsModel.ts`); the WRITE CAPABILITY
 * of a row is whether a real PATCH route exists for exactly that key and
 * whether the current gates admit it. A key with no binding has no write path
 * — not a locked one, not a denied one — and says so.
 */

import type { CodeIndexWorkerSelectionV1 } from '../../contracts/generated.ts';
import type { SettingsWriteGate, WritableScopes } from './settingsGates.ts';
import type { SettingsDraft } from './settingsEditorMachine.ts';
import type {
  CodeIndexWorkerSettingsValues,
  ConfigRow,
  ConfigSection,
  ProjectSettingsValues,
  SettingsEditor,
  SettingsModel,
  SettingsScope,
  UserSettingsValues,
} from './settingsModel.ts';

export interface EffectiveRow {
  /** `${section.id}.${row.id}` — unique across the whole payload. */
  readonly key: string;
  readonly section: ConfigSection;
  readonly row: ConfigRow;
}

/** Every scalar the payload serves, in payload order, with its full key. */
export function effectiveRows(model: SettingsModel): EffectiveRow[] {
  const rows: EffectiveRow[] = [];
  for (const section of model.sections) {
    for (const row of section.rows) {
      if (row.kind === 'group') continue;
      rows.push({ key: `${section.id}.${row.id}`, section, row });
    }
  }
  return rows;
}

/** Rows matching `query` over key, value text, and any served description. */
export function filterEffectiveRows(rows: readonly EffectiveRow[], query: string): EffectiveRow[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') return [...rows];
  return rows.filter(
    ({ key, row }) =>
      key.toLowerCase().includes(needle) ||
      row.text.toLowerCase().includes(needle) ||
      (row.description ?? '').toLowerCase().includes(needle),
  );
}

/* ------------------------------------------------------------- bindings --*/

/** How a bound key is edited: the control the value's contract calls for. */
export type SettingsInputKind = 'globs' | 'integer' | 'boolean' | 'duration' | 'workers';

/**
 * A key the daemon accepts a write for, and the draft field it lands in.
 *
 * The field names are the editor's (`settingsModel.ts`), which are held to the
 * generated patch contracts by type assertion there; a key bound here reaches
 * exactly one field of exactly one CAS-guarded resource.
 */
export type SettingsBinding =
  | {
      readonly scope: 'project';
      readonly field: keyof ProjectSettingsValues;
      readonly input: SettingsInputKind;
    }
  | {
      readonly scope: 'user';
      readonly field: keyof UserSettingsValues;
      readonly input: SettingsInputKind;
    }
  | {
      readonly scope: 'code_index_workers';
      readonly field: 'code_index_workers';
      readonly input: 'workers';
    };

const BINDINGS: Readonly<Record<string, SettingsBinding>> = {
  'project.config.include': { scope: 'project', field: 'include', input: 'globs' },
  'project.config.exclude': { scope: 'project', field: 'exclude', input: 'globs' },
  'project.config.max_file_size': { scope: 'project', field: 'max_file_size', input: 'integer' },
  'project.config.extract_docstrings': {
    scope: 'project',
    field: 'extract_docstrings',
    input: 'boolean',
  },
  'project.config.track_call_sites': {
    scope: 'project',
    field: 'track_call_sites',
    input: 'boolean',
  },
  'project.config.git_ignore': { scope: 'project', field: 'git_ignore', input: 'boolean' },
  'project.config.context_scout': { scope: 'project', field: 'context_scout', input: 'boolean' },
  'project.config.telemetry.timings': {
    scope: 'project',
    field: 'telemetry_timings',
    input: 'boolean',
  },
  'project.config.sync.auto_track_pr_branches': {
    scope: 'project',
    field: 'auto_track_pr_branches',
    input: 'boolean',
  },
  'project.config.sync.auto_track_pr_poll_secs': {
    scope: 'project',
    field: 'auto_track_pr_poll_secs',
    input: 'integer',
  },
  'user.upload_enabled': { scope: 'user', field: 'upload_enabled', input: 'boolean' },
  'user.watcher_debounce': { scope: 'user', field: 'watcher_debounce', input: 'duration' },
  'user.extraction_timeout_secs': {
    scope: 'user',
    field: 'extraction_timeout_secs',
    input: 'integer',
  },
  'user.code_index_workers': {
    scope: 'code_index_workers',
    field: 'code_index_workers',
    input: 'workers',
  },
};

/** The PATCH binding for a full key, or `null` when no write route addresses it. */
export function bindingFor(key: string): SettingsBinding | null {
  return BINDINGS[key] ?? null;
}

/** The full keys this dashboard can write, so a test can hold them to the served payload. */
export function boundKeys(): readonly string[] {
  return Object.keys(BINDINGS);
}

/* ------------------------------------------------------ write capability --*/

/**
 * What a row's write column states. Three answers, none of which is a
 * boolean: a key may be writable (and to what), locked by a gate (and by
 * which, with the authority's reason), or simply have no write path at all.
 *
 * `locked` carries a fourth gate beside the three scope gates: the read itself
 * named no revision to hold a write against, so the editor is unavailable for
 * every bound key at once. Folded in here so the WRITE column and the review
 * panel cannot disagree about whether a key can be edited.
 */
export type WriteCapability =
  | { readonly kind: 'writable'; readonly target: string; readonly binding: SettingsBinding }
  | {
      readonly kind: 'locked';
      readonly gate: Exclude<SettingsWriteGate['state'], 'writable'> | 'editor_unavailable';
      readonly reason: string;
      readonly binding: SettingsBinding;
    }
  | { readonly kind: 'no_write_path' };

export const EDITOR_UNAVAILABLE_REASON =
  'GET /api/settings named no configuration revision to hold a write against, so nothing here can be edited until it does.';

export function writeCapability(
  key: string,
  gates: WritableScopes,
  editorAvailable: boolean,
): WriteCapability {
  const binding = bindingFor(key);
  if (!binding) return { kind: 'no_write_path' };
  if (!editorAvailable) {
    return { kind: 'locked', gate: 'editor_unavailable', reason: EDITOR_UNAVAILABLE_REASON, binding };
  }
  const gate = gateFor(binding.scope, gates);
  switch (gate.state) {
    case 'writable':
      return { kind: 'writable', target: gate.target, binding };
    case 'unauthorized':
      return {
        kind: 'locked',
        gate: gate.state,
        reason: `this dashboard is not authorized to apply ${scopeNoun(binding.scope)} settings`,
        binding,
      };
    case 'read_only':
    case 'unknown':
      return { kind: 'locked', gate: gate.state, reason: gate.reason, binding };
    default: {
      const exhaustive: never = gate;
      return exhaustive;
    }
  }
}

export function gateFor(scope: SettingsScope, gates: WritableScopes): SettingsWriteGate {
  switch (scope) {
    case 'project':
      return gates.project;
    case 'user':
      return gates.user;
    case 'code_index_workers':
      return gates.codeIndexWorkers;
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

/** The scope as a noun a sentence can carry. */
export function scopeNoun(scope: SettingsScope): string {
  switch (scope) {
    case 'project':
      return 'project';
    case 'user':
      return 'user';
    case 'code_index_workers':
      return 'code-index worker';
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

/* ----------------------------------------------------- apply requirement --*/

/**
 * When a persisted change takes effect, as far as the product states it.
 *
 * The worker selection is the one resource whose adoption the daemon documents
 * ahead of time: it is read at startup, so a saved change waits for a restart.
 * Project and user writes report `resync_recommended` / `restart_recommended`
 * in the PATCH response, so before apply their requirement is genuinely not
 * yet known — and is labelled as such rather than guessed.
 */
export type ApplyRequirement =
  | { readonly kind: 'restart'; readonly detail: string }
  | { readonly kind: 'reported_on_apply' };

export function applyRequirement(binding: SettingsBinding): ApplyRequirement {
  switch (binding.scope) {
    case 'code_index_workers':
      return {
        kind: 'restart',
        detail:
          'The profile persists this selection; the daemon adopts it when it restarts. The running worker plan stays in force until then.',
      };
    case 'project':
    case 'user':
      return { kind: 'reported_on_apply' };
    default: {
      const exhaustive: never = binding;
      return exhaustive;
    }
  }
}

/* ------------------------------------------------------- draft accessors --*/

/** The draft's value for a bound field. */
export function draftValue(draft: SettingsDraft, binding: SettingsBinding): unknown {
  switch (binding.scope) {
    case 'project':
      return draft.project[binding.field];
    case 'user':
      return draft.user[binding.field];
    case 'code_index_workers':
      return draft.codeIndexWorkers.code_index_workers;
    default: {
      const exhaustive: never = binding;
      return exhaustive;
    }
  }
}

/** The authority's value for a bound field — what the daemon last reported. */
export function authorityValue(authority: SettingsEditor, binding: SettingsBinding): unknown {
  switch (binding.scope) {
    case 'project':
      return authority.project[binding.field];
    case 'user':
      return authority.user[binding.field];
    case 'code_index_workers':
      return authority.codeIndexWorkers.code_index_workers;
    default: {
      const exhaustive: never = binding;
      return exhaustive;
    }
  }
}

/** Whether the draft departs from the authority on exactly this field. */
export function fieldEdited(
  draft: SettingsDraft,
  authority: SettingsEditor,
  binding: SettingsBinding,
): boolean {
  return !sameValue(draftValue(draft, binding), authorityValue(authority, binding));
}

/**
 * The draft with one bound field replaced. Values are typed at the boundary:
 * a project or user field takes the string/boolean/list its `Values` type
 * holds, and the worker field takes a whole selection, because that is the
 * unit the daemon accepts.
 */
export function withFieldValue(
  draft: SettingsDraft,
  binding: SettingsBinding,
  value: unknown,
): SettingsDraft {
  switch (binding.scope) {
    case 'project':
      return { ...draft, project: withProjectField(draft.project, binding.field, value) };
    case 'user':
      return { ...draft, user: withUserField(draft.user, binding.field, value) };
    case 'code_index_workers':
      return {
        ...draft,
        codeIndexWorkers: withWorkerSelection(draft.codeIndexWorkers, value),
      };
    default: {
      const exhaustive: never = binding;
      return exhaustive;
    }
  }
}

function withProjectField(
  values: ProjectSettingsValues,
  field: keyof ProjectSettingsValues,
  value: unknown,
): ProjectSettingsValues {
  switch (field) {
    case 'include':
    case 'exclude':
      return isStringList(value) ? { ...values, [field]: value } : values;
    case 'max_file_size':
    case 'auto_track_pr_poll_secs':
      return typeof value === 'string' ? { ...values, [field]: value } : values;
    case 'extract_docstrings':
    case 'track_call_sites':
    case 'git_ignore':
    case 'context_scout':
    case 'telemetry_timings':
    case 'auto_track_pr_branches':
      return typeof value === 'boolean' ? { ...values, [field]: value } : values;
    default: {
      const exhaustive: never = field;
      return exhaustive;
    }
  }
}

function withUserField(
  values: UserSettingsValues,
  field: keyof UserSettingsValues,
  value: unknown,
): UserSettingsValues {
  switch (field) {
    case 'upload_enabled':
      return typeof value === 'boolean' ? { ...values, [field]: value } : values;
    case 'watcher_debounce':
    case 'extraction_timeout_secs':
      return typeof value === 'string' ? { ...values, [field]: value } : values;
    default: {
      const exhaustive: never = field;
      return exhaustive;
    }
  }
}

function withWorkerSelection(
  values: CodeIndexWorkerSettingsValues,
  value: unknown,
): CodeIndexWorkerSettingsValues {
  return isWorkerSelection(value) ? { ...values, code_index_workers: value } : values;
}

/** The one runtime check for a worker selection, shared by every reader of one. */
export function isWorkerSelection(value: unknown): value is CodeIndexWorkerSelectionV1 {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as { mode?: unknown; workers?: unknown };
  return (
    candidate.mode === 'automatic' ||
    (candidate.mode === 'exact' && typeof candidate.workers === 'number')
  );
}

function isStringList(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === 'string');
}

function sameValue(left: unknown, right: unknown): boolean {
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((item, index) => item === right[index]);
  }
  if (isWorkerSelection(left) && isWorkerSelection(right)) {
    return (
      left.mode === right.mode &&
      (left.mode !== 'exact' || (right.mode === 'exact' && left.workers === right.workers))
    );
  }
  return left === right;
}
