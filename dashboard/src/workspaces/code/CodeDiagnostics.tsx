/**
 * CODE DIAGNOSTICS — the broker resource at `/api/plugins/code-diagnostics`:
 * `GET` for the snapshot, `POST /refresh` and `POST /refresh/{language}` to
 * make the analyzers run, and `PATCH` to change which of them run at all.
 *
 * The dashboard-owned LSP diagnostics broker: which analyzer engines are
 * mounted for this project, what state each is in, and the diagnostics they
 * currently hold, attributed where possible to the enclosing indexed symbol.
 *
 * Everything shown is the broker's own snapshot. Engine states (`ready`,
 * `crashed`, `unavailable`, …) are the server's words rendered directly; a
 * broker with no engines mounted is an honest empty, and an unreachable
 * authority renders as the boundary's unavailable state — never as a clean
 * zero-error report.
 *
 * The controls hold the same line. Three distinct in-flight facts live on this
 * panel and none of them may be drawn as another:
 *
 *   - `pending_refreshes` and an engine in state `refreshing` are the BROKER's
 *     work, running whether or not this browser asked for it;
 *   - a control this reader dispatched is THIS browser's request, and stops
 *     being in flight when the daemon answers, not when the analyzer finishes;
 *   - a scope that will not accept writes means nothing was sent at all.
 *
 * Nothing here is optimistic: every control re-reads, and the panel repaints
 * from the snapshot the server returned. See `data/query/codeDiagnostics.ts`.
 */
import { RefreshCw } from 'lucide-react';

import { PayloadBoundary } from '../../ui/ReadSection.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn';
import {
  controlFailure,
  sameCommand,
  useCodeDiagnostics,
  useDiagnosticsControl,
  type Diagnostic,
  type DiagnosticsCommand,
  type DiagnosticsSnapshot,
  type EngineStatus,
} from '../../data/query/codeDiagnostics.ts';

/** The broker's engine words mapped onto the shared chip vocabulary; each is
 * a direct reading, not an inference. */
const ENGINE_CHIP: Record<EngineStatus['state'], DomainStateKind> = {
  unavailable: 'unavailable',
  disabled: 'cancelled',
  inactive: 'unknown',
  available: 'partial',
  ready: 'ready',
  refreshing: 'loading',
  crashed: 'error',
};

/** How many diagnostics the panel prints; the totals above state the rest. */
const SHOWN = 8;

/** The controls, resolved for one render: what may be dispatched, what is in
 * flight right now, and what the last attempt reported. Assembled once and
 * handed down so every control on the panel answers from one reading rather
 * than each taking its own. */
interface Controls {
  readonly run: (command: DiagnosticsCommand) => void;
  readonly inFlight: (command: DiagnosticsCommand) => boolean;
  readonly busy: boolean;
  /** Absent when the scope accepts writes; otherwise the daemon's own reason,
   * which is what the disabled controls are titled with. */
  readonly refusal: string | undefined;
  readonly failure: string | null;
}

export function CodeDiagnostics() {
  const snapshot = useCodeDiagnostics();
  const control = useDiagnosticsControl();
  const variables = control.variables;
  const controls: Controls = {
    run: (command) => control.mutate(command),
    inFlight: (command) =>
      control.isPending && variables !== undefined && sameCommand(variables, command),
    busy: control.isPending,
    refusal:
      control.writability.state === 'writable' ? undefined : control.writability.reason,
    // Only a settled attempt reports; an in-flight one has produced no reading
    // yet, and the previous attempt's failure must not be read as this one's.
    failure:
      control.isPending || control.data === undefined ? null : controlFailure(control.data),
  };
  return (
    <section className="flex flex-col gap-1.5" aria-label="Code diagnostics">
      <div className="flex items-center gap-2.5">
        <div className="td-legend shrink-0">diagnostics</div>
        <span aria-hidden className="td-rule" />
        <RefreshButton
          controls={controls}
          command={{ kind: 'refresh_all' }}
          label="Refresh every analyzer engine"
          text="refresh all"
        />
      </div>
      <PayloadBoundary title="Diagnostics" pending={snapshot.isPending} result={snapshot.data}>
        {(data) => <SnapshotBody data={data} controls={controls} />}
      </PayloadBoundary>
    </section>
  );
}

function SnapshotBody({ data, controls }: { data: DiagnosticsSnapshot; controls: Controls }) {
  const shown = data.diagnostics.slice(0, SHOWN);
  const rest = data.diagnostics.length - shown.length;
  // A settings write is a compare-and-set against the settings the broker
  // holds. When it could not read them the body still carries values — the
  // built-in defaults — and a patch sent against those would write this
  // panel's guess over a file whose contents nobody here has seen. So the
  // settings controls are withheld, and the refresh controls, which carry no
  // revision and overwrite nothing, are not.
  const settingsWritable = data.settings_unavailable === undefined;
  return (
    <div className="flex flex-col gap-2">
      {data.settings_unavailable ? (
        <p role="status" className="text-2xs leading-relaxed text-state-error">
          analyzer settings could not be read ({data.settings_unavailable.reason}); defaults are
          in effect, custom analyzers are missing from this snapshot, and settings cannot be
          changed from here until they can be read
        </p>
      ) : null}
      {controls.failure !== null ? (
        <p role="status" className="text-2xs leading-relaxed text-state-error">
          {controls.failure}
        </p>
      ) : null}
      {controls.refusal !== undefined ? (
        <p className="text-3xs leading-relaxed text-text-muted">
          this scope is served read-only, so the controls are disabled: {controls.refusal}
        </p>
      ) : null}
      <dl className="grid grid-cols-3 gap-x-3 gap-y-0.5 text-2xs">
        <Figure label="errors" value={data.summary.total_errors} emphasis="error" />
        <Figure label="warnings" value={data.summary.total_warnings} emphasis="warning" />
        {/* The broker's own count of analyzer work in flight — not this
          * browser's request, which is reported on the buttons. */}
        <Figure label="broker refreshing" value={data.summary.pending_refreshes} />
      </dl>
      <p className="text-3xs text-text-muted">
        {data.summary.last_refresh_age_seconds === null
          ? 'no refresh has completed since this broker started'
          : `last refresh completed ${formatAge(data.summary.last_refresh_age_seconds)} ago`}
        {' · idle backfill '}
        {data.settings.idle_backfill}
      </p>
      <IdleBackfill data={data} controls={controls} writable={settingsWritable} />
      {data.engines.length === 0 ? (
        <p className="text-2xs text-text-muted">
          no diagnostic engines are mounted for this project
        </p>
      ) : (
        <ul className="flex flex-col gap-1" aria-label="Diagnostic engines">
          {data.engines.map((engine) => (
            <EngineLine
              key={engine.language}
              engine={engine}
              revision={data.settings_revision}
              controls={controls}
              settingsWritable={settingsWritable}
            />
          ))}
        </ul>
      )}
      {data.diagnostics.length === 0 ? (
        data.engines.some((engine) => engine.state === 'ready') ? (
          <p className="text-2xs text-text-muted">the mounted engines report no diagnostics</p>
        ) : null
      ) : (
        <ul className="flex flex-col gap-1" aria-label="Current diagnostics">
          {shown.map((row, index) => (
            <DiagnosticLine key={`${row.file}:${row.line_start}:${index}`} row={row} />
          ))}
          {rest > 0 ? (
            <li className="text-3xs leading-relaxed text-text-muted">
              {rest.toLocaleString()} more diagnostics are in the snapshot; the totals above
              count them all
            </li>
          ) : null}
        </ul>
      )}
    </div>
  );
}

/**
 * Whether the broker backfills diagnostics for files nobody opened.
 *
 * A select rather than a toggle because the route's `IdleBackfillMode` is an
 * enum, and rendering it as a checkbox would hard-code the assumption that it
 * stays two-valued — the same assumption that turns a third mode, when one is
 * added, into a control that silently cannot express it.
 */
function IdleBackfill({
  data,
  controls,
  writable,
}: {
  data: DiagnosticsSnapshot;
  controls: Controls;
  writable: boolean;
}) {
  const mode = data.settings.idle_backfill;
  const command: DiagnosticsCommand = {
    kind: 'set_idle_backfill',
    mode,
    revision: data.settings_revision,
  };
  const pending = controls.inFlight(command);
  const disabled = !writable || controls.refusal !== undefined || controls.busy;
  return (
    <label className="flex items-center gap-1.5 text-2xs">
      <span className="td-legend shrink-0 normal-case tracking-normal text-text-muted">
        idle backfill
      </span>
      <select
        value={mode}
        disabled={disabled}
        aria-label="Idle backfill mode"
        aria-busy={pending}
        title={controls.refusal ?? (writable ? undefined : 'analyzer settings are unreadable')}
        onChange={(event) =>
          controls.run({
            kind: 'set_idle_backfill',
            // The revision of the reading this control is rendered from, which
            // is the state the operator is editing.
            revision: data.settings_revision,
            mode: event.target.value === 'off' ? 'off' : 'idle',
          })
        }
        className="h-5 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-1 text-2xs text-text-primary focus:border-accent/60 focus:outline-none disabled:opacity-50"
      >
        <option value="idle">idle</option>
        <option value="off">off</option>
      </select>
    </label>
  );
}

function EngineLine({
  engine,
  revision,
  controls,
  settingsWritable,
}: {
  engine: EngineStatus;
  revision: string;
  controls: Controls;
  settingsWritable: boolean;
}) {
  const toggle: DiagnosticsCommand = {
    kind: 'set_language_enabled',
    language: engine.language,
    enabled: !engine.enabled,
    revision,
  };
  const togglePending = controls.inFlight(toggle);
  return (
    <li className="flex items-center gap-2 text-2xs">
      <StateChip kind={ENGINE_CHIP[engine.state]} detail={engine.last_error ?? undefined} />
      <span className="text-text-primary">{engine.language}</span>
      <span className="min-w-0 flex-1 truncate text-text-muted" title={engine.command}>
        {engine.command}
      </span>
      <label className="flex shrink-0 items-center gap-1 text-3xs text-text-muted">
        <input
          type="checkbox"
          checked={engine.enabled}
          disabled={!settingsWritable || controls.refusal !== undefined || controls.busy}
          aria-label={`Run the ${engine.language} analyzer`}
          aria-busy={togglePending}
          onChange={() => controls.run(toggle)}
          className="td-check"
        />
        enabled
      </label>
      <RefreshButton
        controls={controls}
        command={{ kind: 'refresh_language', language: engine.language }}
        label={`Refresh the ${engine.language} analyzer`}
        text="refresh"
      />
    </li>
  );
}

/**
 * One refresh control.
 *
 * `aria-busy` rather than a label swap: the button keeps its accessible name
 * while the request is out, so a screen reader is not told the control has
 * become a different control. The word beside it changes because a sighted
 * reader needs the same fact, and it says `sent` rather than `refreshing` —
 * what this browser knows is that the request went out, not that an analyzer
 * is running, which is what the engine's own chip says.
 */
function RefreshButton({
  controls,
  command,
  label,
  text,
}: {
  controls: Controls;
  command: DiagnosticsCommand;
  label: string;
  text: string;
}) {
  const pending = controls.inFlight(command);
  const disabled = controls.refusal !== undefined || controls.busy;
  return (
    <button
      type="button"
      onClick={() => controls.run(command)}
      disabled={disabled}
      aria-label={label}
      aria-busy={pending}
      title={controls.refusal}
      className="flex min-h-[var(--touch-target-min)] shrink-0 items-center gap-1 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-1.5 py-0.5 text-3xs text-text-secondary hover:border-accent/60 hover:text-text-primary focus:border-accent/60 focus:outline-none disabled:opacity-50 disabled:hover:border-edge-subtle disabled:hover:text-text-secondary"
    >
      <RefreshCw aria-hidden size={9} className={cn(pending && 'animate-spin')} />
      {pending ? 'sent' : text}
    </button>
  );
}

/** Seconds as the coarsest unit that still states the quantity. */
function formatAge(seconds: number): string {
  if (seconds < 90) return `${Math.round(seconds)}s`;
  if (seconds < 5_400) return `${Math.round(seconds / 60)}m`;
  if (seconds < 172_800) return `${Math.round(seconds / 3_600)}h`;
  return `${Math.round(seconds / 86_400)}d`;
}

function Figure({
  label,
  value,
  emphasis,
}: {
  label: string;
  value: number;
  emphasis?: 'error' | 'warning';
}) {
  return (
    <div className="flex flex-col">
      <dt className="td-legend">{label}</dt>
      <dd
        className={cn(
          'tabular text-sm',
          emphasis === 'error' && value > 0 && 'text-state-error',
          emphasis === 'warning' && value > 0 && 'text-state-partial',
        )}
      >
        {value.toLocaleString()}
      </dd>
    </div>
  );
}

function DiagnosticLine({ row }: { row: Diagnostic }) {
  return (
    <li className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2">
      <div className="flex items-baseline gap-1.5 text-3xs text-text-muted">
        <span
          className={cn(
            'uppercase tracking-wide',
            row.severity === 'error' ? 'text-state-error' : 'text-state-partial',
          )}
        >
          {row.severity}
        </span>
        <span className="min-w-0 truncate" title={`${row.file}:${row.line_start}`}>
          {row.file}:{row.line_start}
        </span>
        {row.code != null ? <span>[{row.code}]</span> : null}
      </div>
      <p className="text-2xs leading-relaxed text-text-secondary">{row.message}</p>
      {row.enclosing_node != null ? (
        <p className="text-3xs text-text-muted">in {row.enclosing_node}</p>
      ) : null}
    </li>
  );
}
