/**
 * CURATION CONSOLE — what the curator has done, what its runs recorded, and
 * what it is configured to do.
 *
 * Three routes, in the order a reader needs them:
 *
 *   `/curation/activity`  the in-process event stream the deterministic curator
 *                         pushes as it plans and applies. Ephemeral by
 *                         construction — the daemon keeps the last 300 in
 *                         memory — so an empty stream after a restart is not an
 *                         idle curator, and this panel says so.
 *   `/curation/runs`      the append-only sidecar ledger of standalone
 *                         automation-backend runs. Durable, and the only place a
 *                         failed run survives. It answers HTTP 200 with an
 *                         `error` string when the ledger itself cannot be read,
 *                         so the error is checked before the records are.
 *   `/curation/config`    the layered automation configuration — profile
 *                         global, project overlay, resolved effective — plus the
 *                         daemon's own probe of the selected backend.
 *
 * The config surface is archetype 4 (config surface, plan 11a): layered values
 * side by side, and a write that is a distinct, guarded step. The guard is not
 * a confirmation dialog; it is three separate refusals stacked in front of the
 * request, none of which is cosmetic:
 *
 *   1. Scope. Only the active project accepts writes through the gateway. The
 *      control is disabled on `scopeWritable`, and the mutation refuses on the
 *      same reading rather than dispatching into a 405 it cannot interpret.
 *   2. Shape. `CurationConfigPatch` admits five booleans. `allow_job_commands`
 *      and `backend: external_command` are rejected by the handler itself, so
 *      they are absent from the type — a control whose only outcome is a 400 is
 *      not a control.
 *   3. Observation. PATCH answers with the same layered payload GET does, and
 *      that answer is what lands in the cache. Nothing here is optimistic: the
 *      reader never sees a setting this dashboard has not observed the daemon
 *      resolve.
 */
import { useState } from 'react';

import { LegacyBoundary } from '../../ui/ReadSection.tsx';
import { Panel, Readout } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { scopeWriteSentence, type ScopeWritability } from '../../data/scope/store.ts';
import {
  useCurationActivity,
  useCurationConfig,
  useCurationConfigPatch,
  useCurationRuns,
  type CurationActivityPayload,
  type CurationConfigPatch,
  type CurationConfigPayload,
  type CurationConfigWriteResult,
  type CurationRunsPayload,
} from '../../data/query/memory.ts';
import { KnowledgeCuration } from './KnowledgeCuration.tsx';
import { curationRunsReading, runStatusState } from './memoryModel.ts';

export function CurationConsole() {
  const activity = useCurationActivity();
  const runs = useCurationRuns();
  const config = useCurationConfig();
  const patch = useCurationConfigPatch();

  return (
    // Scrollable regions need keyboard operation (WCAG 2.1.1). This column
    // scrolls and most of what is in it is read-out — figures, sentences, log
    // rows — so the column itself takes the tab stop and carries its own name.
    <div
      role="region"
      aria-label="Curation console"
      tabIndex={0}
      className="flex h-full flex-col gap-3 overflow-auto p-3"
    >
      {/* The curator's own status and current plan, unchanged: this console
        * surrounds that panel rather than restating it. */}
      <KnowledgeCuration />
      <Panel legend="Curator activity" elevation="well">
        <LegacyBoundary
          title="Curator activity"
          pending={activity.isPending}
          result={activity.data}
        >
          {(data) => <ActivityBody data={data} />}
        </LegacyBoundary>
      </Panel>
      <Panel legend="Backend run history" elevation="well">
        <LegacyBoundary title="Backend run history" pending={runs.isPending} result={runs.data}>
          {(data) => <RunsBody data={data} />}
        </LegacyBoundary>
      </Panel>
      <Panel legend="Curation configuration">
        <LegacyBoundary
          title="Curation configuration"
          pending={config.isPending}
          result={config.data}
        >
          {(data) => (
            <ConfigBody
              data={data}
              writability={patch.writability}
              pending={patch.isPending}
              failure={configWriteFailure(patch.data)}
              onPatch={(next) => patch.mutate(next)}
            />
          )}
        </LegacyBoundary>
      </Panel>
    </div>
  );
}

/* ---- activity ------------------------------------------------------------ */

function ActivityBody({ data }: { data: CurationActivityPayload }) {
  if (data.error !== '') {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        curator activity could not be read: {data.error}
      </p>
    );
  }
  if (data.events.length === 0) {
    return (
      <p className="text-2xs leading-relaxed text-text-muted">
        no curator events are held in memory. This stream is the running daemon's, not a
        record — a restart empties it, so this is not evidence that the curator has been idle.
        The durable account is the run history below.
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <p className="text-3xs leading-relaxed text-text-muted">
        the {data.events.length.toLocaleString()} most recent of the last{' '}
        {data.limit.toLocaleString()} events this daemon holds in memory, newest first
      </p>
      <ol
        role="region"
        aria-label="Curator activity events"
        tabIndex={0}
        className="flex max-h-72 flex-col gap-1 overflow-auto"
      >
        {[...data.events].reverse().map((event, index) => (
          <li
            key={`${event.ts}-${index}`}
            className="flex flex-wrap items-baseline gap-x-2 border-l-2 border-edge-subtle pl-2 text-3xs"
          >
            <span className="td-value text-text-muted" data-cell="numeric">
              {event.ts}
            </span>
            <span className="text-text-muted">{event.phase}</span>
            {event.dry_run ? <span className="text-state-partial">dry run</span> : null}
            {event.level !== 'info' ? (
              <span className="text-state-partial">{event.level}</span>
            ) : null}
            <span className="min-w-0 text-2xs leading-relaxed text-text-secondary">
              {event.message}
            </span>
          </li>
        ))}
      </ol>
    </div>
  );
}

/* ---- runs ---------------------------------------------------------------- */

function RunsBody({ data }: { data: CurationRunsPayload }) {
  const reading = curationRunsReading(data);
  // Checked before `records`: an unreadable ledger and a project that has never
  // run automation both answer with an empty array and HTTP 200, and only this
  // string tells them apart.
  if (reading.ledgerError !== null) {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        the run ledger could not be read: {reading.ledgerError}
      </p>
    );
  }
  if (reading.records.length === 0) {
    return (
      <p className="text-2xs leading-relaxed text-text-muted">
        the run ledger is readable and holds no records — no automation backend run has ever
        completed against this project
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-end gap-4">
        <Readout label="runs" size="sm" value={reading.records.length.toLocaleString()} />
        <Readout label="not completed" size="sm" value={reading.failed.toLocaleString()} />
      </div>
      <dl className="flex flex-col gap-0.5 border-y border-edge-subtle py-2">
        {reading.tasks.map((task) => (
          <div key={task.task} className="flex items-baseline justify-between gap-2 text-2xs">
            <dt className="min-w-0 truncate text-text-primary">{task.task}</dt>
            <dd className="td-value shrink-0 text-text-secondary" data-cell="numeric">
              {task.runs.toLocaleString()} runs · {task.accepted.toLocaleString()} accepted ·{' '}
              {task.rejected.toLocaleString()} rejected · {task.skipped.toLocaleString()} skipped
            </dd>
          </div>
        ))}
      </dl>
      <ol
        role="region"
        aria-label="Automation run records"
        tabIndex={0}
        className="flex max-h-80 flex-col gap-1.5 overflow-auto"
      >
        {reading.records.map((record) => (
          <li
            key={record.run_id}
            className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2"
          >
            <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-3xs text-text-muted">
              <StateChip kind={runStatusState(record.status)} detail={record.status} />
              <span className="text-text-secondary">{record.task}</span>
              <span>{record.trigger}</span>
              <span>{record.backend}</span>
              {/* `model` is `skip_serializing_if = Option::is_none`, so its
                * absence means the run recorded no model — not a null one. */}
              {record.model ? <span>{record.model}</span> : <span>model unrecorded</span>}
            </p>
            <p className="td-value text-3xs text-text-muted" data-cell="numeric">
              {record.started_at} → {record.completed_at} ·{' '}
              {record.accepted_count.toLocaleString()} accepted ·{' '}
              {record.rejected_count.toLocaleString()} rejected ·{' '}
              {record.skipped_count.toLocaleString()} skipped
            </p>
            {record.error ? (
              <p className="text-2xs leading-relaxed text-state-error">{record.error}</p>
            ) : null}
          </li>
        ))}
      </ol>
    </div>
  );
}

/* ---- config -------------------------------------------------------------- */

/** The five members this dashboard may send, with the sentence each one means.
 * Named here rather than inline so the control list and the patch type stay one
 * thing: a member added to `CurationConfigPatch` without a row here is a
 * setting nothing can reach. */
const CONFIG_TOGGLES: readonly {
  key: keyof CurationConfigPatch;
  label: string;
  note: string;
}[] = [
  {
    key: 'enabled',
    label: 'Automation enabled',
    note: 'the scheduler runs the tasks below when they come due',
  },
  {
    key: 'auto_apply_memory_ops',
    label: 'Apply validated memory operations',
    note: 'off retains accepted curation operations as proposals instead of applying them',
  },
  {
    key: 'auto_enable_skills',
    label: 'Enable generated skills automatically',
    note: 'off leaves a written skill in draft until it is approved',
  },
  {
    key: 'export_memory_digest',
    label: 'Export the memory digest',
    note: 'writes the curated fact digest into the host instruction files',
  },
  {
    key: 'combine_due_tasks',
    label: 'Combine due tasks',
    note: 'runs tasks that come due together in one backend invocation',
  },
];

function ConfigBody({
  data,
  writability,
  pending,
  failure,
  onPatch,
}: {
  data: CurationConfigPayload;
  writability: ScopeWritability;
  pending: boolean;
  failure: string | null;
  onPatch: (patch: CurationConfigPatch) => void;
}) {
  // Disabled before dispatch, on the same reading the mutation refuses on. A
  // disabled control whose reason lives elsewhere reads as a broken control, so
  // the sentence sits with it.
  const blocked = writability.state !== 'writable';
  const effective = data.effective;
  const availability = data.backend_availability;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <StateChip
          kind={availability.available ? 'ready' : 'unavailable'}
          detail={
            availability.available
              ? (availability.executable ?? availability.backend)
              : (availability.reason ?? `backend ${availability.backend}`)
          }
        />
        <span className="text-2xs text-text-secondary">
          backend {effective.backend} · host mode {effective.host_mode} · scheduler tick{' '}
          {effective.scheduler_tick_secs.toLocaleString()}s · timeout{' '}
          {effective.timeout_secs.toLocaleString()}s
        </span>
      </div>
      {/* The layering, stated. `project: null` is a project with no automation
        * file at all; `{}` is a file that overrides nothing. Both resolve to
        * the same effective config and they are not the same fact. */}
      <p className="text-3xs leading-relaxed text-text-muted">
        {data.project === null
          ? `no project overlay exists (${data.project_config_path} has not been written), so every value below is the profile default`
          : Object.keys(data.project).length === 0
            ? `the project overlay at ${data.project_config_path} exists and overrides nothing, so every value below is the profile default`
            : `${Object.keys(data.project).length.toLocaleString()} value(s) at ${data.project_config_path} override the profile default`}
      </p>
      <ul className="flex flex-col gap-1">
        {CONFIG_TOGGLES.map((toggle) => (
          <li key={toggle.key}>
            <label className="flex items-center gap-1.5 text-2xs">
              <input
                type="checkbox"
                className="td-check"
                checked={effective[toggle.key] === true}
                disabled={pending || blocked}
                aria-describedby="curation-config-scope"
                onChange={(event) => onPatch({ [toggle.key]: event.target.checked })}
              />
              <span className="min-w-0">
                <span className="text-text-primary">{toggle.label}</span>
                <span className="block text-3xs text-text-muted">{toggle.note}</span>
              </span>
            </label>
          </li>
        ))}
      </ul>
      {/* Present in every state, including the writable one: a write under the
        * all-projects aggregate lands on a single project, and the reader is
        * told which rather than left to assume it fans out. */}
      <p id="curation-config-scope" data-scope-writability={writability.state} className="text-2xs text-text-secondary">
        {scopeWriteSentence(writability, {
          writable: (target) => `Applies to ${target}.`,
        })}
      </p>
      <p className="text-3xs leading-relaxed text-text-muted">
        job commands and the external-command backend are refused by the daemon over HTTP and
        are therefore not offered here; both are local operator configuration.
      </p>
      {failure ? (
        <p role="status" className="text-2xs text-text-secondary">
          {failure}
        </p>
      ) : null}
      <dl className="grid grid-cols-1 gap-x-3 gap-y-0.5 border-t border-edge-subtle pt-2 text-2xs sm:grid-cols-3">
        {(['memory_curator', 'session_reflector', 'skill_writer'] as const).map((task) => {
          const config = effective.tasks[task];
          return (
            <div key={task} className="flex flex-col gap-0.5">
              <dt className="td-legend">{task}</dt>
              <dd className="text-3xs text-text-secondary">
                {config.enabled ? 'enabled' : 'disabled'}
                {config.schedule === null ? ' · no schedule' : ` · ${config.schedule}`}
                {config.interval_secs == null
                  ? ' · interval unset'
                  : ` · every ${config.interval_secs.toLocaleString()}s`}
              </dd>
            </div>
          );
        })}
      </dl>
    </div>
  );
}

/**
 * What a config write produced, worded for what actually happened.
 *
 * Exhaustive over the write union so an outcome added to `LegacyWriteResult`
 * fails to build here rather than falling into whichever arm a chain of
 * ternaries happened to end on.
 */
export function configWriteFailure(result: CurationConfigWriteResult | undefined): string | null {
  if (result === undefined) return null;
  switch (result.outcome) {
    case 'ok':
      return null;
    case 'offline':
      return 'The daemon did not answer, so the configuration was not changed.';
    case 'unauthorized':
      return 'The daemon accepted no identity for the change, so the configuration was not changed.';
    case 'denied':
      return 'This identity is not permitted to change automation configuration, so nothing was changed.';
    case 'error':
      return `The daemon refused the change (${result.detail}).`;
    case 'unsupported_schema':
      return 'The daemon answered in a shape this dashboard cannot read, so whether the configuration changed is unknown — reload to re-read it.';
    case 'unavailable':
      return `The configuration was not changed: ${result.reason ?? result.status}.`;
    case 'read_only_scope':
      return `The configuration was not changed: ${result.refusal.detail}.`;
    case 'not_dispatched':
      return scopeWriteSentence(result.writability, {
        writable: (target) =>
          `Nothing was sent, though writes to ${target} are accepted — reload to re-read the configuration.`,
        refused: (reason) => `Nothing was sent. ${reason}`,
      });
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
