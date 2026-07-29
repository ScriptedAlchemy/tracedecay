/**
 * The editor: the two scopes' fields, and nothing about applying them.
 *
 * These components hold no state and reach for nothing. They render the draft
 * they are handed, mark the fields the controller says were refused, and
 * report edits and review requests back through callbacks.
 */

import { useId } from 'react';
import { cn } from '../../ui/cn';
import {
  settingsButtonClass,
  settingsCheckboxRowClass,
  settingsInputClass,
} from './settingsChrome.ts';
import type {
  ProjectSettingsValues,
  SettingsScope,
  SettingsValidationError,
  UserSettingsValues,
} from './settingsModel.ts';

export function ProjectSettingsFields({
  values,
  errors,
  dirty,
  writable,
  onChange,
  onReview,
}: {
  values: ProjectSettingsValues;
  errors: readonly SettingsValidationError[];
  dirty: boolean;
  writable: boolean;
  onChange: (values: ProjectSettingsValues) => void;
  onReview: () => void;
}) {
  return (
    <fieldset
      className="min-w-0 border border-edge-subtle bg-surface-1 p-3"
      disabled={!writable}
    >
      <legend className="px-1 text-xs font-semibold text-text-primary">Project settings</legend>
      {writable ? <EditState scope="project" dirty={dirty} /> : <ReadOnlyScope scope="project" />}
      <div className="grid gap-2 sm:grid-cols-2">
        <SettingsTextArea
          label="Include globs"
          value={values.include.join('\n')}
          error={errorFor(errors, 'include')}
          onChange={(value) => onChange({ ...values, include: globLines(value) })}
        />
        <SettingsTextArea
          label="Exclude globs"
          value={values.exclude.join('\n')}
          error={errorFor(errors, 'exclude')}
          onChange={(value) => onChange({ ...values, exclude: globLines(value) })}
        />
        <SettingsInput
          label="Maximum file size (bytes)"
          inputMode="numeric"
          value={values.max_file_size}
          error={errorFor(errors, 'max_file_size')}
          onChange={(value) => onChange({ ...values, max_file_size: value })}
        />
        <SettingsInput
          label="PR branch poll interval (seconds)"
          inputMode="numeric"
          value={values.auto_track_pr_poll_secs}
          error={errorFor(errors, 'auto_track_pr_poll_secs')}
          onChange={(value) => onChange({ ...values, auto_track_pr_poll_secs: value })}
        />
      </div>
      <div className="mt-3 grid gap-2 sm:grid-cols-2">
        <SettingsCheckbox
          label="Extract docstrings"
          checked={values.extract_docstrings}
          error={errorFor(errors, 'extract_docstrings')}
          onChange={(checked) => onChange({ ...values, extract_docstrings: checked })}
        />
        <SettingsCheckbox
          label="Track call sites"
          checked={values.track_call_sites}
          error={errorFor(errors, 'track_call_sites')}
          onChange={(checked) => onChange({ ...values, track_call_sites: checked })}
        />
        <SettingsCheckbox
          label="Honor git ignore"
          checked={values.git_ignore}
          error={errorFor(errors, 'git_ignore')}
          onChange={(checked) => onChange({ ...values, git_ignore: checked })}
        />
        <SettingsCheckbox
          label="Record telemetry timings"
          checked={values.telemetry_timings}
          error={errorFor(errors, 'telemetry_timings')}
          onChange={(checked) => onChange({ ...values, telemetry_timings: checked })}
        />
        <SettingsCheckbox
          label="Auto-track pull request branches"
          checked={values.auto_track_pr_branches}
          error={errorFor(errors, 'auto_track_pr_branches')}
          onChange={(checked) => onChange({ ...values, auto_track_pr_branches: checked })}
        />
      </div>
      {writable ? (
        <button type="button" className={`${settingsButtonClass} mt-3`} onClick={onReview}>
          Review project changes
        </button>
      ) : null}
      <FieldError error={errorFor(errors, 'project')} />
    </fieldset>
  );
}

export function UserSettingsFields({
  values,
  errors,
  dirty,
  writable,
  onChange,
  onReview,
}: {
  values: UserSettingsValues;
  errors: readonly SettingsValidationError[];
  dirty: boolean;
  writable: boolean;
  onChange: (values: UserSettingsValues) => void;
  onReview: () => void;
}) {
  return (
    <fieldset
      className="min-w-0 border border-edge-subtle bg-surface-1 p-3"
      disabled={!writable}
    >
      <legend className="px-1 text-xs font-semibold text-text-primary">User settings</legend>
      {writable ? <EditState scope="user" dirty={dirty} /> : <ReadOnlyScope scope="user" />}
      <div className="grid gap-2">
        <SettingsInput
          label="Watcher debounce"
          value={values.watcher_debounce}
          error={errorFor(errors, 'watcher_debounce')}
          onChange={(value) => onChange({ ...values, watcher_debounce: value })}
        />
        <SettingsInput
          label="Extraction timeout (seconds)"
          inputMode="numeric"
          value={values.extraction_timeout_secs}
          error={errorFor(errors, 'extraction_timeout_secs')}
          onChange={(value) => onChange({ ...values, extraction_timeout_secs: value })}
        />
        <SettingsCheckbox
          label="Upload enabled"
          checked={values.upload_enabled}
          error={errorFor(errors, 'upload_enabled')}
          onChange={(checked) => onChange({ ...values, upload_enabled: checked })}
        />
      </div>
      {writable ? (
        <button type="button" className={`${settingsButtonClass} mt-3`} onClick={onReview}>
          Review user changes
        </button>
      ) : null}
      <FieldError error={errorFor(errors, 'user')} />
    </fieldset>
  );
}

function SettingsInput({
  label,
  value,
  inputMode,
  error,
  onChange,
}: {
  label: string;
  value: string;
  inputMode?: 'numeric';
  error?: string;
  onChange: (value: string) => void;
}) {
  const errorId = useId();
  return (
    <label className="grid gap-1 text-2xs text-text-secondary">
      <span>{label}</span>
      <input
        value={value}
        inputMode={inputMode}
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? errorId : undefined}
        onChange={(event) => onChange(event.target.value)}
        className={settingsInputClass}
      />
      <FieldError id={errorId} error={error} />
    </label>
  );
}

function SettingsTextArea({
  label,
  value,
  error,
  onChange,
}: {
  label: string;
  value: string;
  error?: string;
  onChange: (value: string) => void;
}) {
  const errorId = useId();
  return (
    <label className="grid gap-1 text-2xs text-text-secondary">
      <span>{label}</span>
      <textarea
        rows={3}
        value={value}
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? errorId : undefined}
        onChange={(event) => onChange(event.target.value)}
        className={`${settingsInputClass} h-auto min-h-16 py-1.5 font-mono`}
      />
      <FieldError id={errorId} error={error} />
    </label>
  );
}

function SettingsCheckbox({
  label,
  checked,
  error,
  onChange,
}: {
  label: string;
  checked: boolean;
  error?: string;
  onChange: (checked: boolean) => void;
}) {
  const errorId = useId();
  return (
    <div>
      <label className={settingsCheckboxRowClass}>
        <input
          type="checkbox"
          className="td-check"
          checked={checked}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          onChange={(event) => onChange(event.target.checked)}
        />
        <span className="min-w-0 pr-2">{label}</span>
      </label>
      <FieldError id={errorId} error={error} />
    </div>
  );
}

export function errorFor(
  errors: readonly SettingsValidationError[],
  field: string,
): string | undefined {
  return errors.find((error) => error.field === field)?.message;
}

export function FieldError({ id, error }: { id?: string; error?: string }) {
  if (!error) return null;
  return (
    <p id={id} role="alert" className="mt-1 text-2xs text-state-error">
      {error}
    </p>
  );
}

function EditState({ scope, dirty }: { scope: SettingsScope; dirty: boolean }) {
  return (
    <p
      aria-live="polite"
      className={cn(
        'mb-2 text-3xs font-semibold uppercase tracking-[0.16em]',
        dirty ? 'text-accent' : 'text-text-muted',
      )}
    >
      {dirty ? `Unsaved ${scope} changes` : `Current ${scope} values`}
    </p>
  );
}

/** A scope the envelope carries no apply action for.
 *
 * This states only what the wire states — the server does not currently
 * authorize this write — rather than guessing at which authority is missing. */
function ReadOnlyScope({ scope }: { scope: SettingsScope }) {
  return (
    <p
      aria-live="polite"
      className="mb-2 text-3xs font-semibold uppercase tracking-[0.16em] text-state-unsupported-schema"
    >
      Read-only · this dashboard is not authorized to apply {scope} settings
    </p>
  );
}

function globLines(value: string): string[] {
  if (value.trim() === '') return [];
  return value.split(/\r?\n/).map((line) => line.trim());
}
