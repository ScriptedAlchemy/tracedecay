/**
 * Settings read model: turns the untyped `/api/settings` payload
 * (settings_api.rs::get_settings) into a navigable, origin-aware shape.
 *
 * WHAT THE WIRE ACTUALLY CARRIES — this file exists to keep the surface honest
 * about it:
 *
 *   - `project.config` is `project_configuration.config`, a resolved config
 *     struct serialized wholesale. Every key in it is already effective. The
 *     payload never says which file or default supplied a given key.
 *   - `automation` is `automation_config::effective_config(global, project)`.
 *     The daemon really does merge two layers there — and then ships only the
 *     result. The winning layer is not on the wire.
 *   - `user` and `project.config` do not even address the same keys
 *     (`upload_enabled`/`watcher_debounce` vs `include`/`exclude`/…), so they
 *     are not competing layers over a shared namespace.
 *
 * Therefore this model does NOT rank groups into a resolution stack and does
 * not claim that one group overrides another: nothing in the payload supports
 * it. What it does model is ORIGIN — the source each group is read from, which
 * the payload states directly via `config_path` / `config_endpoint` — and the
 * one place the payload carries genuine per-value provenance:
 * `environment.variables[]`, where `active` distinguishes an override that is
 * actually in force from one that is unset so a default applies.
 *
 * Nothing here invents a value. Every row is a literal from the payload and
 * every note is a restatement of a key the payload actually carries.
 */

import { z } from 'zod';
import {
  SettingsPayloadV1Schema,
  type ProjectSettingsPatch,
  type SettingsPayloadV1,
  type UserSettingsPatch,
} from '../../contracts/generated.ts';

export type ConfigRowKind =
  | 'group'
  | 'boolean'
  | 'number'
  | 'path'
  | 'string'
  | 'null'
  | 'list';

export interface ConfigRow {
  /** Dotted path within the section — unique, and the row's React key. */
  readonly id: string;
  /** Leaf label as it appears in the payload. */
  readonly label: string;
  /** Nesting depth inside the section (0 = the section's own top level). */
  readonly depth: number;
  readonly kind: ConfigRowKind;
  /** The literal payload value (scalars and lists only; groups carry null). */
  readonly value: unknown;
  /** Scalar rendering + search text. Empty for groups. */
  readonly text: string;
  /** For groups: how many scalar settings live underneath. */
  count: number;
}

/**
 * Where a group's values come from, as the payload states it.
 *
 * `file`        — read from a configuration file whose path the payload gives.
 * `environment` — the process environment overlay.
 * `resolved`    — state the daemon computed and reported; no source stated.
 */
export type OriginKind = 'file' | 'environment' | 'resolved';

export interface ConfigSection {
  /** Top-level payload key — also the DOM id used for in-page navigation. */
  readonly id: string;
  readonly title: string;
  /** What this group is, in one clause. */
  readonly blurb: string;
  readonly origin: OriginKind;
  /** Path or endpoint the payload names for this group, when it names one. */
  readonly location: string | null;
  /** How to read `location`: a filesystem path or an API endpoint. */
  readonly locationKind: 'path' | 'endpoint' | null;
  /** Facts restated from keys the payload carries. Never inferred. */
  readonly notes: readonly string[];
  readonly rows: readonly ConfigRow[];
  /** Scalar settings in this section. */
  readonly settingCount: number;
}

/**
 * A process-environment variable the daemon reports on. This is the only
 * per-value provenance `/api/settings` carries: `active` states whether the
 * variable is set in the daemon's environment (an override in force) or unset
 * (so whatever default applies, applies).
 */
export interface EnvOverride {
  readonly name: string;
  /** Payload `active`: true when the variable is set in the process env. */
  readonly active: boolean;
  /** Payload `value`. Non-null only when active. */
  readonly value: string | null;
  readonly description: string;
}

export interface ConfigStamp {
  readonly label: string;
  readonly value: string;
}

export interface SettingsModel {
  readonly sections: readonly ConfigSection[];
  readonly settingCount: number;
  /** Identity of the configuration snapshot being displayed, when present. */
  readonly stamps: readonly ConfigStamp[];
  /** Environment overrides, reported verbatim. Empty when absent. */
  readonly overrides: readonly EnvOverride[];
  /** How many of `overrides` are actually in force. */
  readonly activeOverrides: number;
}

/** The two settings resources, each with its own authority and revision. */
export type SettingsScope = 'project' | 'user';

export interface ProjectSettingsValues {
  readonly include: readonly string[];
  readonly exclude: readonly string[];
  readonly max_file_size: string;
  readonly extract_docstrings: boolean;
  readonly track_call_sites: boolean;
  readonly git_ignore: boolean;
  readonly telemetry_timings: boolean;
  readonly auto_track_pr_branches: boolean;
  readonly auto_track_pr_poll_secs: string;
}

export interface UserSettingsValues {
  readonly upload_enabled: boolean;
  readonly watcher_debounce: string;
  readonly extraction_timeout_secs: string;
}

export interface SettingsEditor {
  readonly projectExpectedRevisionId: string;
  readonly userExpectedRevisionId: string;
  readonly project: ProjectSettingsValues;
  readonly user: UserSettingsValues;
}

export interface SettingsValidationError {
  readonly field: string;
  readonly message: string;
}

/**
 * The fields the editor found changed — not the request body.
 *
 * The contracted body is `ProjectSettingsPatch`, which also carries
 * `expected_revision_id`; the plan holds that separately so a change set can be
 * reviewed on its own, and `settingsMutation.ts` merges the two at the PATCH.
 * Omission here means "not edited", which the daemon reads as no change through
 * `#[serde(default)]`. Do not rename this back onto the generated name: they
 * are different values and the shadow gate will reject it.
 */
export interface ProjectSettingsChangeSet {
  include?: string[];
  exclude?: string[];
  max_file_size?: number;
  extract_docstrings?: boolean;
  track_call_sites?: boolean;
  git_ignore?: boolean;
  telemetry?: { timings?: boolean };
  sync?: {
    auto_track_pr_branches?: boolean;
    auto_track_pr_poll_secs?: number;
  };
}

/** The user-scope counterpart of `ProjectSettingsChangeSet`. */
export interface UserSettingsChangeSet {
  upload_enabled?: boolean;
  watcher_debounce?: string;
  extraction_timeout_secs?: number;
}

/**
 * The change sets carry omission semantics the generated patch types cannot
 * express — schemars renders every `Option<T>` field as required-and-nullable,
 * so `ProjectSettingsPatch` has no way to say "this field was not edited",
 * which is precisely what the daemon's `#[serde(default)]` reads. The field
 * NAMES are still the contract's, and these assertions fail the build if the
 * two ever drift apart, so a field renamed or added in Rust cannot silently
 * keep being sent under its old name.
 */
type Assert<T extends true> = T;
type WritableFieldsOf<TPatch> = Exclude<keyof TPatch, 'expected_revision_id'>;
type SameKeys<TLeft, TRight> = [keyof TLeft] extends [TRight]
  ? [TRight] extends [keyof TLeft]
    ? true
    : false
  : false;

type _ProjectChangeSetNamesTheContractsFields = Assert<
  SameKeys<ProjectSettingsChangeSet, WritableFieldsOf<ProjectSettingsPatch>>
>;
type _UserChangeSetNamesTheContractsFields = Assert<
  SameKeys<UserSettingsChangeSet, WritableFieldsOf<UserSettingsPatch>>
>;

export type SettingsChangePlan<T> =
  | {
      readonly outcome: 'ready';
      readonly expectedRevisionId: string;
      readonly patch: T;
    }
  | {
      readonly outcome: 'unchanged';
      readonly expectedRevisionId: string;
    }
  | {
      readonly outcome: 'invalid';
      readonly errors: readonly SettingsValidationError[];
    };

export interface SettingsRevisionConflict {
  readonly expectedRevisionId: string;
  readonly actualRevisionId: string | null;
}

export const MIN_AUTO_TRACK_PR_POLL_SECS = 60;

/**
 * Recognized top-level groups and how to describe them. `origin` records where
 * the group is read from — a fact the payload supports — and deliberately
 * carries no precedence, because the payload states none.
 */
const GROUP_META: Readonly<
  Record<string, { title: string; blurb: string; origin: OriginKind }>
> = {
  project: {
    title: 'Project',
    blurb: 'Repository-local configuration file',
    origin: 'file',
  },
  user: {
    title: 'User',
    blurb: 'Profile-level configuration file',
    origin: 'file',
  },
  environment: {
    title: 'Environment',
    blurb: 'Process environment overlay',
    origin: 'environment',
  },
  automation: {
    title: 'Automation',
    blurb: 'Effective automation config, merged daemon-side',
    origin: 'resolved',
  },
  storage: { title: 'Storage', blurb: 'Resolved store locations', origin: 'resolved' },
  version: { title: 'Version', blurb: 'Build identity', origin: 'resolved' },
  capabilities: {
    title: 'Capabilities',
    blurb: 'Reported feature surface',
    origin: 'resolved',
  },
};

/** Files first, then the environment overlay, then daemon-resolved state. */
const ORIGIN_ORDER: Readonly<Record<OriginKind, number>> = {
  file: 0,
  environment: 1,
  resolved: 2,
};

/**
 * Keys consumed by a dedicated renderer, so the generic row flattener does not
 * also emit them and show the same facts twice.
 */
const SPECIALIZED: Readonly<Record<string, ReadonlySet<string>>> = {
  environment: new Set(['variables']),
};

/**
 * What reading a settings body produced. The two failures are different
 * accusations against different authorities, so they stay apart: a body that
 * is not an envelope at all, and an envelope whose payload the generated
 * contract refuses.
 */
export type SettingsPayloadRead =
  | { readonly outcome: 'settings'; readonly payload: SettingsPayloadV1 }
  | { readonly outcome: 'not_an_envelope' }
  | { readonly outcome: 'unsupported_payload' };

/** An envelope with *some* object under `payload`; the contract judges the rest. */
const SettingsEnvelopeMemberSchema = z.object({ payload: z.record(z.string(), z.unknown()) });

/**
 * The settings payload inside a `DashboardEnvelopeV1`, parsed by the generated
 * schema.
 *
 * `/api/settings` and both PATCH routes answer with the envelope, so the
 * groups this file models live under `payload` and never at the top level.
 * Reading the envelope itself as settings would render `schema_revision`,
 * `coverage`, and `freshness` as configuration and, worse, find no revision
 * identity — which the editor would report as an omitted required field.
 */
export function readSettingsEnvelope(body: unknown): SettingsPayloadRead {
  const envelope = SettingsEnvelopeMemberSchema.safeParse(body);
  if (!envelope.success) return { outcome: 'not_an_envelope' };
  const payload = SettingsPayloadV1Schema.safeParse(envelope.data.payload);
  return payload.success
    ? { outcome: 'settings', payload: payload.data }
    : { outcome: 'unsupported_payload' };
}

/**
 * The read model walks whatever groups the payload carries, so its parameter
 * stays `unknown` on purpose: a group the daemon starts reporting before the
 * contract is regenerated must still appear here rather than vanish, and every
 * row it emits is a literal it found — it never claims a named field. The
 * editable slice, which does name fields, goes through `buildSettingsEditor`
 * against a contract-parsed payload instead.
 */
export function buildSettingsModel(payload: unknown): SettingsModel {
  if (!isRecord(payload)) {
    return { sections: [], settingCount: 0, stamps: [], overrides: [], activeOverrides: 0 };
  }
  const sections: ConfigSection[] = [];
  for (const [key, value] of Object.entries(payload)) {
    sections.push(buildSection(key, value));
  }
  const ordered = sections
    .map((section, index) => ({ section, index }))
    .sort(
      (a, b) =>
        ORIGIN_ORDER[a.section.origin] - ORIGIN_ORDER[b.section.origin] ||
        a.index - b.index,
    )
    .map((entry) => entry.section);
  const overrides = readOverrides(payload['environment']);
  return {
    sections: ordered,
    settingCount: ordered.reduce((total, s) => total + s.settingCount, 0),
    stamps: readStamps(payload),
    overrides,
    activeOverrides: overrides.filter((item) => item.active).length,
  };
}

/**
 * The editable slice of a payload the generated contract already vouched for.
 *
 * Only two things remain to check, and neither is a shape: a revision id the
 * contract admits as a string but that names no revision, and a count the
 * contract admits as a signed integer but that the editor's unsigned fields
 * cannot round-trip. Both make the resource uneditable rather than partially
 * editable, so they answer `null` and the surface says so.
 */
export function buildSettingsEditor(payload: SettingsPayloadV1): SettingsEditor | null {
  const { config, configuration_revision_id: projectRevision } = payload.project;
  const { user_settings_revision_id: userRevision } = payload.user;
  const maxFileSize = unsignedIntegerString(config.max_file_size);
  const pollSecs = unsignedIntegerString(config.sync.auto_track_pr_poll_secs);
  const extractionTimeout = unsignedIntegerString(payload.user.extraction_timeout_secs);
  if (
    projectRevision.length === 0 ||
    userRevision.length === 0 ||
    maxFileSize == null ||
    pollSecs == null ||
    extractionTimeout == null
  ) {
    return null;
  }
  return {
    projectExpectedRevisionId: projectRevision,
    userExpectedRevisionId: userRevision,
    project: {
      include: [...config.include],
      exclude: [...config.exclude],
      max_file_size: maxFileSize,
      extract_docstrings: config.extract_docstrings,
      track_call_sites: config.track_call_sites,
      git_ignore: config.git_ignore,
      telemetry_timings: config.telemetry.timings,
      auto_track_pr_branches: config.sync.auto_track_pr_branches,
      auto_track_pr_poll_secs: pollSecs,
    },
    user: {
      upload_enabled: payload.user.upload_enabled,
      watcher_debounce: payload.user.watcher_debounce,
      extraction_timeout_secs: extractionTimeout,
    },
  };
}

/** The revision a write to `scope` must be held against. */
export function settingsRevisionId(current: SettingsEditor, scope: SettingsScope): string {
  switch (scope) {
    case 'project':
      return current.projectExpectedRevisionId;
    case 'user':
      return current.userExpectedRevisionId;
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

export function planProjectSettingsChange(
  payload: SettingsPayloadV1,
  values: ProjectSettingsValues,
): SettingsChangePlan<ProjectSettingsChangeSet> {
  return planProjectChangeAgainst(buildSettingsEditor(payload), values);
}

/**
 * The project change a draft represents against a known-current snapshot.
 *
 * Split from the payload-level entry point so the state machine can replan
 * without re-parsing: replanning is how a confirmation is checked against the
 * review it was given for.
 */
export function planProjectChangeAgainst(
  current: SettingsEditor | null,
  values: ProjectSettingsValues,
): SettingsChangePlan<ProjectSettingsChangeSet> {
  if (!current) {
    return {
      outcome: 'invalid',
      errors: [
        {
          field: 'configuration_revision_id',
          message: 'current editable settings and revision are unavailable',
        },
      ],
    };
  }
  const errors = validateProjectValues(values);
  if (errors.length > 0) return { outcome: 'invalid', errors };

  const patch: ProjectSettingsChangeSet = {};
  if (!sameStrings(values.include, current.project.include)) {
    patch.include = [...values.include];
  }
  if (!sameStrings(values.exclude, current.project.exclude)) {
    patch.exclude = [...values.exclude];
  }
  const maxFileSize = Number(values.max_file_size);
  if (maxFileSize !== Number(current.project.max_file_size)) {
    patch.max_file_size = maxFileSize;
  }
  for (const field of ['extract_docstrings', 'track_call_sites', 'git_ignore'] as const) {
    if (values[field] !== current.project[field]) patch[field] = values[field];
  }
  if (values.telemetry_timings !== current.project.telemetry_timings) {
    patch.telemetry = { timings: values.telemetry_timings };
  }
  const sync: NonNullable<ProjectSettingsChangeSet['sync']> = {};
  if (values.auto_track_pr_branches !== current.project.auto_track_pr_branches) {
    sync.auto_track_pr_branches = values.auto_track_pr_branches;
  }
  const pollSecs = Number(values.auto_track_pr_poll_secs);
  if (pollSecs !== Number(current.project.auto_track_pr_poll_secs)) {
    sync.auto_track_pr_poll_secs = pollSecs;
  }
  if (Object.keys(sync).length > 0) patch.sync = sync;

  return Object.keys(patch).length === 0
    ? { outcome: 'unchanged', expectedRevisionId: current.projectExpectedRevisionId }
    : { outcome: 'ready', expectedRevisionId: current.projectExpectedRevisionId, patch };
}

export function planUserSettingsChange(
  payload: SettingsPayloadV1,
  values: UserSettingsValues,
): SettingsChangePlan<UserSettingsChangeSet> {
  return planUserChangeAgainst(buildSettingsEditor(payload), values);
}

/** The user-scope counterpart of `planProjectChangeAgainst`. */
export function planUserChangeAgainst(
  current: SettingsEditor | null,
  values: UserSettingsValues,
): SettingsChangePlan<UserSettingsChangeSet> {
  if (!current) {
    return {
      outcome: 'invalid',
      errors: [
        {
          field: 'user_settings_revision_id',
          message: 'current editable settings and revision are unavailable',
        },
      ],
    };
  }
  const errors = validateUserValues(values);
  if (errors.length > 0) return { outcome: 'invalid', errors };

  const patch: UserSettingsChangeSet = {};
  if (values.upload_enabled !== current.user.upload_enabled) {
    patch.upload_enabled = values.upload_enabled;
  }
  if (values.watcher_debounce !== current.user.watcher_debounce) {
    patch.watcher_debounce = values.watcher_debounce;
  }
  const timeout = Number(values.extraction_timeout_secs);
  if (timeout !== Number(current.user.extraction_timeout_secs)) {
    patch.extraction_timeout_secs = timeout;
  }
  return Object.keys(patch).length === 0
    ? { outcome: 'unchanged', expectedRevisionId: current.userExpectedRevisionId }
    : { outcome: 'ready', expectedRevisionId: current.userExpectedRevisionId, patch };
}

export function settingsRevisionConflict(
  scope: SettingsScope,
  expectedRevisionId: string,
  payload: SettingsPayloadV1,
): SettingsRevisionConflict | null {
  const editor = buildSettingsEditor(payload);
  const actualRevisionId = editor ? settingsRevisionId(editor, scope) : null;
  return actualRevisionId === expectedRevisionId
    ? null
    : { expectedRevisionId, actualRevisionId };
}

function validateProjectValues(values: ProjectSettingsValues): SettingsValidationError[] {
  const errors: SettingsValidationError[] = [];
  validateGlobValues('include', values.include, errors);
  validateGlobValues('exclude', values.exclude, errors);
  if (!isPositiveInteger(values.max_file_size)) {
    errors.push({
      field: 'max_file_size',
      message: 'max_file_size must be at least 1 byte',
    });
  }
  const pollSecs = unsignedInteger(values.auto_track_pr_poll_secs);
  if (pollSecs == null || pollSecs < MIN_AUTO_TRACK_PR_POLL_SECS) {
    errors.push({
      field: 'auto_track_pr_poll_secs',
      message: `auto_track_pr_poll_secs must be at least ${MIN_AUTO_TRACK_PR_POLL_SECS} seconds`,
    });
  }
  return errors;
}

function validateUserValues(values: UserSettingsValues): SettingsValidationError[] {
  const errors: SettingsValidationError[] = [];
  if (!validDuration(values.watcher_debounce)) {
    errors.push({
      field: 'watcher_debounce',
      message: 'watcher_debounce must be a duration like "2s", "15s", or "1m"',
    });
  }
  if (!isPositiveInteger(values.extraction_timeout_secs)) {
    errors.push({
      field: 'extraction_timeout_secs',
      message: 'extraction_timeout_secs must be at least 1 second',
    });
  }
  return errors;
}

function validateGlobValues(
  field: 'include' | 'exclude',
  patterns: readonly string[],
  errors: SettingsValidationError[],
): void {
  for (const pattern of patterns) {
    if (pattern.trim() === '') {
      errors.push({ field, message: `${field} patterns must not be empty` });
    } else if (!validGlob(pattern)) {
      errors.push({ field, message: `invalid glob pattern '${pattern}'` });
    }
  }
}

function validGlob(pattern: string): boolean {
  const chars = Array.from(pattern);
  let index = 0;
  while (index < chars.length) {
    if (chars[index] === '*') {
      const start = index;
      while (chars[index] === '*') index += 1;
      const count = index - start;
      if (count > 2) return false;
      if (
        count === 2 &&
        !(
          (start === 0 || chars[start - 1] === '/') &&
          (index === chars.length || chars[index] === '/')
        )
      ) {
        return false;
      }
      continue;
    }
    if (chars[index] === '[') {
      const contentStart = chars[index + 1] === '!' ? index + 2 : index + 1;
      let close = contentStart + 1;
      while (close < chars.length && chars[close] !== ']') close += 1;
      if (close >= chars.length) return false;
      index = close + 1;
      continue;
    }
    index += 1;
  }
  return true;
}

function validDuration(value: string): boolean {
  return /^\s*\d+\s*[sm]?\s*$/.test(value) && unsignedInteger(value.replace(/[sm]\s*$/, '')) != null;
}

function isPositiveInteger(value: string): boolean {
  const parsed = unsignedInteger(value);
  return parsed != null && parsed >= 1;
}

function unsignedInteger(value: string): number | null {
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const parsed = Number(trimmed);
  return Number.isSafeInteger(parsed) ? parsed : null;
}

/** The contract admits any `i64` here; the editor's field can only hold an
 * unsigned one, so a negative or unrepresentable count is not editable. */
function unsignedIntegerString(value: number): string | null {
  return Number.isSafeInteger(value) && value >= 0 ? String(value) : null;
}

function sameStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function buildSection(key: string, value: unknown): ConfigSection {
  const meta = GROUP_META[key];
  const skip = SPECIALIZED[key];
  const rows: ConfigRow[] = [];
  let settingCount = 0;
  if (isRecord(value)) {
    for (const [childKey, childValue] of Object.entries(value)) {
      if (skip?.has(childKey)) continue;
      settingCount += flatten(childKey, childValue, childKey, 0, rows);
    }
  } else {
    settingCount += flatten(key, value, key, 0, rows);
  }
  const location = readLocation(key, value);
  return {
    id: key,
    title: meta?.title ?? humanize(key),
    blurb: sectionBlurb(key, value, meta?.blurb ?? 'Reported by the daemon'),
    origin: meta?.origin ?? 'resolved',
    location: location?.value ?? null,
    locationKind: location?.kind ?? null,
    notes: readNotes(value),
    rows,
    settingCount,
  };
}

function sectionBlurb(key: string, value: unknown, fallback: string): string {
  if (key !== 'automation' || !isRecord(value)) return fallback;
  const availability = value['availability'];
  return isRecord(availability) && availability['available'] === false
    ? 'Automation configuration unavailable'
    : fallback;
}

/** Where a section's values live, when the payload says so. */
function readLocation(
  key: string,
  value: unknown,
): { value: string; kind: 'path' | 'endpoint' } | null {
  if (isRecord(value)) {
    const path = value['config_path'];
    if (typeof path === 'string' && path.length > 0) return { value: path, kind: 'path' };
    const endpoint = value['config_endpoint'];
    if (typeof endpoint === 'string' && endpoint.length > 0) {
      return { value: endpoint, kind: 'endpoint' };
    }
  }
  if (key === 'environment') return { value: 'process environment', kind: 'endpoint' };
  return null;
}

/**
 * Notes are restatements, not inferences: each one is emitted only when the
 * payload carries the exact key it reports on.
 */
function readNotes(value: unknown): string[] {
  if (!isRecord(value)) return [];
  const notes: string[] = [];
  if (value['legacy_config_read_only'] === true) {
    notes.push('legacy config path is read-only');
  }
  const configPath = value['config_path'];
  const legacyPath = value['legacy_config_path'];
  if (
    typeof configPath === 'string' &&
    typeof legacyPath === 'string' &&
    configPath === legacyPath
  ) {
    notes.push('config path and legacy path are the same file');
  }
  if (value['enabled'] === false) notes.push('disabled');
  return notes;
}

/** Identity of the displayed snapshot, if the payload carries one. */
function readStamps(payload: Record<string, unknown>): ConfigStamp[] {
  const stamps: ConfigStamp[] = [];
  const push = (label: string, value: unknown) => {
    if (typeof value === 'string' && value.length > 0) stamps.push({ label, value });
  };
  for (const group of Object.values(payload)) {
    if (!isRecord(group)) continue;
    push('snapshot', group['configuration_snapshot_id']);
    push('revision', group['configuration_revision_id']);
  }
  const version = payload['version'];
  if (isRecord(version)) {
    push('version', version['version']);
    push('channel', version['channel']);
  }
  return stamps;
}

/**
 * Reads `environment.variables[]` verbatim. Entries without a usable `name` are
 * dropped rather than guessed at; `active` is taken only from a literal `true`.
 */
function readOverrides(environment: unknown): EnvOverride[] {
  if (!isRecord(environment)) return [];
  const variables = environment['variables'];
  if (!Array.isArray(variables)) return [];
  const overrides: EnvOverride[] = [];
  for (const item of variables) {
    if (!isRecord(item)) continue;
    const name = item['name'];
    if (typeof name !== 'string' || name.length === 0) continue;
    const value = item['value'];
    const description = item['description'];
    overrides.push({
      name,
      active: item['active'] === true,
      value: typeof value === 'string' && value.length > 0 ? value : null,
      description: typeof description === 'string' ? description : '',
    });
  }
  return overrides;
}

/**
 * Depth-first flatten. Returns the number of scalar settings contributed, which
 * is what group rows report as their `count`.
 */
function flatten(
  label: string,
  value: unknown,
  id: string,
  depth: number,
  out: ConfigRow[],
): number {
  if (Array.isArray(value)) {
    if (value.length === 0) {
      out.push(scalarRow(label, id, depth, 'list', value, 'empty list'));
      return 1;
    }
    if (value.every(isRecord)) {
      const index = out.length;
      out.push(groupRow(label, id, depth));
      let count = 0;
      value.forEach((item, i) => {
        count += flatten(itemLabel(item, i), item, `${id}[${i}]`, depth + 1, out);
      });
      out[index]!.count = count;
      return count;
    }
    out.push(scalarRow(label, id, depth, 'list', value, value.map(String).join(', ')));
    return 1;
  }
  if (isRecord(value)) {
    const entries = Object.entries(value);
    if (entries.length === 0) {
      out.push(scalarRow(label, id, depth, 'list', value, 'empty'));
      return 1;
    }
    const index = out.length;
    out.push(groupRow(label, id, depth));
    let count = 0;
    for (const [childKey, childValue] of entries) {
      count += flatten(childKey, childValue, `${id}.${childKey}`, depth + 1, out);
    }
    out[index]!.count = count;
    return count;
  }
  out.push(scalarRow(label, id, depth, classify(value), value, scalarText(value)));
  return 1;
}

function itemLabel(item: Record<string, unknown>, index: number): string {
  for (const key of ['name', 'id', 'key', 'label', 'branch']) {
    const candidate = item[key];
    if (typeof candidate === 'string' && candidate.length > 0) return candidate;
  }
  return `#${index}`;
}

function groupRow(label: string, id: string, depth: number): ConfigRow {
  return { id, label, depth, kind: 'group', value: null, text: '', count: 0 };
}

function scalarRow(
  label: string,
  id: string,
  depth: number,
  kind: ConfigRowKind,
  value: unknown,
  text: string,
): ConfigRow {
  return { id, label, depth, kind, value, text, count: 1 };
}

function classify(value: unknown): ConfigRowKind {
  if (value === null || value === undefined) return 'null';
  if (typeof value === 'boolean') return 'boolean';
  if (typeof value === 'number') return 'number';
  if (typeof value === 'string') return isPathLike(value) ? 'path' : 'string';
  return 'string';
}

/** Absolute or home-relative filesystem paths, and the daemon's route paths. */
export function isPathLike(value: string): boolean {
  return /^~?\/[^\s]*$/.test(value) && value.length > 1;
}

function scalarText(value: unknown): string {
  if (value === null || value === undefined) return 'null';
  return String(value);
}

/**
 * Filter rows to those matching `query`, keeping the ancestors that give a
 * match its context and the full subtree of any group that matches by name.
 */
export function filterRows(rows: readonly ConfigRow[], query: string): ConfigRow[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') return [...rows];
  const keep = new Array<boolean>(rows.length).fill(false);
  rows.forEach((row, index) => {
    if (!matches(row, needle)) return;
    keep[index] = true;
    if (row.kind === 'group') {
      for (let i = index + 1; i < rows.length && rows[i]!.depth > row.depth; i += 1) {
        keep[i] = true;
      }
    }
  });
  // Ancestors of every kept row, so nesting still reads.
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    if (!keep[index]) continue;
    let depth = rows[index]!.depth;
    for (let i = index - 1; i >= 0 && depth > 0; i -= 1) {
      if (rows[i]!.depth < depth) {
        keep[i] = true;
        depth = rows[i]!.depth;
      }
    }
  }
  return rows.filter((_, index) => keep[index]!);
}

/** Environment overrides matching `query`, over name, value and description. */
export function filterOverrides(
  overrides: readonly EnvOverride[],
  query: string,
): EnvOverride[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') return [...overrides];
  return overrides.filter(
    (item) =>
      item.name.toLowerCase().includes(needle) ||
      item.description.toLowerCase().includes(needle) ||
      (item.value ?? '').toLowerCase().includes(needle),
  );
}

function matches(row: ConfigRow, needle: string): boolean {
  return (
    row.label.toLowerCase().includes(needle) ||
    row.id.toLowerCase().includes(needle) ||
    row.text.toLowerCase().includes(needle)
  );
}

/** Scalar settings among a filtered row slice. */
export function countSettings(rows: readonly ConfigRow[]): number {
  return rows.reduce((total, row) => total + (row.kind === 'group' ? 0 : 1), 0);
}

/** Split a path into its directory prefix and the segment worth reading. */
export function splitPath(value: string): { head: string; tail: string } {
  const cut = value.lastIndexOf('/');
  if (cut < 0) return { head: '', tail: value };
  return { head: value.slice(0, cut + 1), tail: value.slice(cut + 1) };
}

function humanize(key: string): string {
  const spaced = key.replace(/[_-]+/g, ' ').trim();
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
