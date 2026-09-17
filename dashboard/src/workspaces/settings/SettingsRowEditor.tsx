/**
 * The proposal control for one bound key: the input the value's contract calls
 * for, and nothing about applying it.
 *
 * Holds no state. It renders the draft value it is handed, marks itself with
 * the field error the machine reports, and reports edits back. Which control a
 * key gets is decided by its binding's `input`, never re-guessed from the value.
 */

import { useId } from 'react';
import type {
  CodeIndexWorkerSelectionV1,
  CodeIndexWorkerStatusV1,
} from '../../contracts/generated.ts';
import { settingsCheckboxRowClass, settingsInputClass } from './settingsChrome.ts';
import { isWorkerSelection, type SettingsBinding } from './settingsRows.ts';

export function SettingsRowEditor({
  binding,
  value,
  label,
  error,
  disabled,
  workerStatus,
  onChange,
}: {
  binding: SettingsBinding;
  value: unknown;
  /** The accessible name every control here carries: the full key. */
  label: string;
  error: string | undefined;
  disabled: boolean;
  workerStatus: CodeIndexWorkerStatusV1 | null;
  onChange: (value: unknown) => void;
}) {
  const errorId = useId();
  const described = error ? errorId : undefined;
  switch (binding.input) {
    case 'globs':
      return (
        <div className="grid gap-1">
          <textarea
            rows={3}
            aria-label={label}
            aria-invalid={error ? true : undefined}
            aria-describedby={described}
            disabled={disabled}
            value={Array.isArray(value) ? value.map(String).join('\n') : ''}
            onChange={(event) => onChange(globLines(event.target.value))}
            className={`${settingsInputClass} h-auto min-h-16 py-1.5 font-mono`}
          />
          <span className="text-3xs text-text-muted">one glob per line</span>
          <FieldError id={errorId} error={error} />
        </div>
      );
    case 'integer':
    case 'duration':
      return (
        <div className="grid gap-1">
          <input
            aria-label={label}
            inputMode={binding.input === 'integer' ? 'numeric' : undefined}
            aria-invalid={error ? true : undefined}
            aria-describedby={described}
            disabled={disabled}
            value={typeof value === 'string' ? value : ''}
            onChange={(event) => onChange(event.target.value)}
            className={`${settingsInputClass} font-mono`}
          />
          {binding.input === 'duration' ? (
            <span className="text-3xs text-text-muted">a duration like 2s, 15s, or 1m</span>
          ) : null}
          <FieldError id={errorId} error={error} />
        </div>
      );
    case 'boolean':
      return (
        <div className="grid gap-1">
          <label className={settingsCheckboxRowClass}>
            <input
              type="checkbox"
              className="td-check"
              aria-label={label}
              aria-invalid={error ? true : undefined}
              aria-describedby={described}
              disabled={disabled}
              checked={value === true}
              onChange={(event) => onChange(event.target.checked)}
            />
            <span className="td-value min-w-0 pr-2 text-2xs">
              {value === true ? 'true' : 'false'}
            </span>
          </label>
          <FieldError id={errorId} error={error} />
        </div>
      );
    case 'workers':
      return (
        <WorkerSelectionEditor
          label={label}
          value={value}
          error={error}
          errorId={errorId}
          disabled={disabled}
          status={workerStatus}
          onChange={onChange}
        />
      );
    default: {
      const exhaustive: never = binding;
      return exhaustive;
    }
  }
}

/**
 * The worker selection is one value with two shapes. The exact count's upper
 * bound comes from the daemon's admitted plan when it is on the wire; when it
 * is not, the contract's ceiling applies and the daemon judges the count at
 * restart, stated beside the field rather than invented as a capacity.
 */
function WorkerSelectionEditor({
  label,
  value,
  error,
  errorId,
  disabled,
  status,
  onChange,
}: {
  label: string;
  value: unknown;
  error: string | undefined;
  errorId: string;
  disabled: boolean;
  status: CodeIndexWorkerStatusV1 | null;
  onChange: (value: unknown) => void;
}) {
  const group = useId();
  const selection = readSelection(value);
  const exactWorkers = selection.mode === 'exact' ? selection.workers : 1;
  const maximum = status
    ? Math.min(status.available_logical_cpus, status.memory_safe_workers)
    : 65_535;
  return (
    <fieldset className="grid gap-1" disabled={disabled} aria-label={label}>
      <div className="grid gap-1 @md:grid-cols-2">
        <label className={settingsCheckboxRowClass}>
          <input
            type="radio"
            name={group}
            className="td-check"
            checked={selection.mode === 'automatic'}
            onChange={() => onChange({ mode: 'automatic' })}
          />
          <span className="min-w-0 pr-2 text-2xs">
            Automatic
            <span className="block text-3xs text-text-muted">
              the daemon chooses a memory-safe number of available cores
            </span>
          </span>
        </label>
        <div className="grid gap-1">
          <label className={settingsCheckboxRowClass}>
            <input
              type="radio"
              name={group}
              className="td-check"
              checked={selection.mode === 'exact'}
              onChange={() => onChange({ mode: 'exact', workers: exactWorkers })}
            />
            <span className="min-w-0 pr-2 text-2xs">Exact number of cores</span>
          </label>
          <label className="grid gap-1 text-3xs text-text-muted">
            <span>Code-index worker count</span>
            <input
              type="number"
              min={1}
              max={maximum}
              step={1}
              inputMode="numeric"
              disabled={selection.mode !== 'exact'}
              value={selection.mode === 'exact' ? String(exactWorkers) : ''}
              aria-invalid={error ? true : undefined}
              aria-describedby={error ? errorId : undefined}
              onChange={(event) =>
                onChange({ mode: 'exact', workers: Number(event.target.value) })
              }
              className={`${settingsInputClass} font-mono`}
            />
          </label>
        </div>
      </div>
      <p className="text-3xs text-text-muted">
        {status
          ? `The running daemon admits up to ${maximum} exact workers, bounded by ${status.available_logical_cpus} logical CPUs and ${status.memory_safe_workers} memory-safe workers.`
          : 'Current CPU and memory admission limits are unavailable; an exact count is judged when the daemon restarts.'}
      </p>
      {status?.environment_override_workers != null ? (
        <p className="border border-state-unsupported-schema bg-surface-0 p-2 text-2xs text-text-primary">
          TRACEDECAY_INDEX_WORKERS={status.environment_override_workers} overrides the persisted
          worker selection for this running daemon.
        </p>
      ) : null}
      <FieldError id={errorId} error={error} />
    </fieldset>
  );
}

function readSelection(value: unknown): CodeIndexWorkerSelectionV1 {
  return isWorkerSelection(value) ? value : { mode: 'automatic' };
}

export function FieldError({ id, error }: { id?: string; error?: string | undefined }) {
  if (!error) return null;
  return (
    <p id={id} role="alert" className="text-2xs text-state-error">
      {error}
    </p>
  );
}

function globLines(value: string): string[] {
  if (value.trim() === '') return [];
  return value.split(/\r?\n/).map((line) => line.trim());
}
