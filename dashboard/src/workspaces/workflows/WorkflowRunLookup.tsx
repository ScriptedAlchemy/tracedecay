import { useId, useState, type ReactNode } from 'react';
import type { WorkflowRunEvent, WorkflowRunProjection } from '../../contracts/index.ts';
import { cn } from '../../ui/cn.ts';
import { GradeTag } from '../../ui/EvidenceGrade.tsx';
import { formatMicrosUtcClock } from '../../ui/format.ts';
import { Lamp, Panel } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import type { WorkResult } from '../work/workApi.ts';
import { DigestLine } from './WorkflowRegistry.tsx';
import {
  definitionKey,
  formatDurationMicros,
  runStatusTone,
  runStepSequence,
  runTiming,
  stepStatusTone,
  succeededStepCount,
  type RunStepRow,
} from './workflowLedger.ts';

/**
 * Exact run lookup. Independent of the registry: a loaded definition does not
 * imply a run exists, and a run's pinned definition is its own copy. Timing
 * is read from the run's event journal; a run that has not finished has an
 * elapsed figure, never a duration.
 */

const INPUT_CLASS =
  'min-h-[var(--touch-target-min)] rounded-panel border border-edge-subtle bg-surface-1 px-2 font-mono text-2xs text-text-primary placeholder:text-text-muted focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent';

const BUTTON_CLASS =
  'min-h-[var(--touch-target-min)] rounded-panel border border-edge-subtle px-2.5 py-1 text-2xs text-text-primary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:text-text-muted';

const CELL = 'border border-edge-subtle p-1 align-top';
const HEAD = `${CELL} td-legend whitespace-normal text-left text-text-muted`;

export function RunLookupPanel({
  runId,
  result,
  pending,
  registryKeys,
  onLookup,
  onSelectDefinition,
}: {
  runId: string | null;
  result: WorkResult<WorkflowRunProjection> | undefined;
  pending: boolean;
  /** Every `id@version` the registry currently serves, so a run can pivot to
   * its pinned definition only when that definition is actually listed. */
  registryKeys: ReadonlySet<string>;
  onLookup: (runId: string | null) => void;
  onSelectDefinition: (definitionId: string, version: number) => void;
}) {
  const [draft, setDraft] = useState('');
  const inputId = useId();
  const lookupState =
    runId === null
      ? 'idle'
      : pending
        ? 'reading'
        : result === undefined
          ? 'unknown'
          : result.outcome === 'refused'
            ? result.state
            : 'loaded';

  return (
    <Panel
      legend="Run lookup · exact run id"
      elevation="well"
      bodyClassName="flex min-w-0 flex-col gap-2 p-2.5"
      actions={
        <span className="td-legend shrink-0 text-text-muted" data-testid="workflow-run-state">
          {lookupState}
        </span>
      }
    >
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          const trimmed = draft.trim();
          onLookup(trimmed === '' ? null : trimmed);
        }}
      >
        <label className="flex min-w-0 flex-1 flex-col gap-0.5 text-3xs text-text-muted" htmlFor={inputId}>
          <span className="td-legend">Run id</span>
          <input
            id={inputId}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder="run identity, exactly as its owning surface minted it"
            className={cn(INPUT_CLASS, 'w-full')}
          />
        </label>
        <button type="submit" disabled={draft.trim() === ''} className={BUTTON_CLASS}>
          Read run
        </button>
      </form>

      {runId === null ? (
        <p className="text-3xs text-text-muted">
          No run is loaded. A run is read by its exact id; the registry does not list runs, and a
          loaded definition does not imply one exists.
        </p>
      ) : pending ? (
        <StateChip kind="loading" detail={`reading run ${runId}`} />
      ) : result === undefined ? (
        <StateChip kind="unknown" detail="the run read returned no result" />
      ) : result.outcome === 'refused' ? (
        <div className="flex flex-col gap-1">
          <StateChip kind={result.state} detail={result.detail} />
          <p className="text-3xs text-text-muted">
            Nothing is projected for <span className="td-value">{runId}</span>. A concealed, denied,
            or unavailable run is not an empty run.
          </p>
        </div>
      ) : (
        <RunProjectionView
          projection={result.value}
          registryKeys={registryKeys}
          onSelectDefinition={onSelectDefinition}
        />
      )}
    </Panel>
  );
}

function RunProjectionView({
  projection,
  registryKeys,
  onSelectDefinition,
}: {
  projection: WorkflowRunProjection;
  registryKeys: ReadonlySet<string>;
  onSelectDefinition: (definitionId: string, version: number) => void;
}) {
  const timing = runTiming(projection);
  const steps = runStepSequence(projection);
  const total = projection.definition.steps.length;
  const succeeded = succeededStepCount(projection);
  const pinnedKey = definitionKey(
    projection.definition.definition_id,
    projection.definition.definition_version,
  );
  const listed = registryKeys.has(pinnedKey);

  return (
    <div className="flex min-w-0 flex-col gap-3" data-workflow-run={projection.run_id}>
      <div className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="td-value min-w-0 break-all text-xs text-text-primary">
          {projection.run_id}
        </span>
        <span
          className="inline-flex items-center gap-1.5 text-3xs"
          data-run-status={projection.status}
        >
          <Lamp tone={runStatusTone(projection.status)} live={projection.status === 'running'} />
          <span className="uppercase tracking-[0.1em] text-text-secondary">{projection.status}</span>
        </span>
        <GradeTag grade="EXACT" source="run journal" />
      </div>

      <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-3xs">
        <RunFact label="workflow">
          <span className="td-value break-all">{pinnedKey}</span>
          {listed ? (
            <button
              type="button"
              onClick={() =>
                onSelectDefinition(
                  projection.definition.definition_id,
                  projection.definition.definition_version,
                )
              }
              className="ml-1.5 border border-edge-subtle px-1 text-3xs text-accent hover:bg-surface-3"
            >
              select in registry
            </button>
          ) : (
            <span className="ml-1.5 text-text-muted">pinned copy · not in the loaded registry</span>
          )}
        </RunFact>
        <RunFact label="sequence">
          <span className="td-value">{projection.sequence}</span>
          <span className="ml-1 text-text-muted">· {timing.events} journal events</span>
        </RunFact>
        <RunFact label="admitted (utc)">
          <span className="td-value">{formatMicrosUtcClock(timing.admittedAt)}</span>
          {timing.admittedAt === null ? (
            <span className="ml-1 text-text-muted">no admitted event in the served journal</span>
          ) : null}
        </RunFact>
        <RunFact label="last event (utc)">
          <span className="td-value">{formatMicrosUtcClock(timing.lastEventAt)}</span>
        </RunFact>
        <RunFact label={timing.terminal ? 'duration' : 'elapsed to last event'}>
          <span className="td-value" data-testid="workflow-run-span">
            {formatDurationMicros(timing.terminal ? timing.durationMicros : timing.elapsedMicros)}
          </span>
          {timing.terminal ? null : (
            <span className="ml-1 text-text-muted">· not finished, so not a duration</span>
          )}
        </RunFact>
        <RunFact label="steps succeeded">
          <span className="td-value">
            {succeeded} / {total}
          </span>
        </RunFact>
        <RunFact label="fan-out attempts">
          <span className="td-value">
            {projection.released_fan_out_attempts.length} released ·{' '}
            {projection.settled_fan_out_attempts.length} settled
          </span>
        </RunFact>
        <RunFact label="fan-out plans">
          <span className="td-value">{Object.keys(projection.fan_out_plans).length}</span>
        </RunFact>
      </dl>

      <div className="flex min-w-0 flex-col gap-1 border-t border-edge-subtle pt-2">
        <span className="td-legend">pinned by this run</span>
        <DigestLine label="topology" digest={projection.pinned_topology_digest} />
        <DigestLine label="providers" digest={projection.pinned_provider_registry_digest} />
        <DigestLine label="policy" digest={projection.definition.pinned_policy_digest} />
        <DigestLine label="configuration" digest={projection.definition.pinned_configuration_digest} />
        <DigestLine label="catalog" digest={projection.definition.pinned_catalog_digest} />
      </div>

      <RunStepTable rows={steps} runId={projection.run_id} />
      <RunJournal events={projection.history} />
    </div>
  );
}

function RunFact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend">{label}</dt>
      <dd className="min-w-0 break-words text-text-secondary">{children}</dd>
    </div>
  );
}

function RunStepTable({ rows, runId }: { rows: RunStepRow[]; runId: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1 border-t border-edge-subtle pt-2">
      <span className="td-legend">decoded step sequence · {runId}</span>
      <div className="min-w-0 overflow-x-auto">
        <table className="w-full border-collapse text-3xs" data-workflow-run-steps={rows.length}>
          <caption className="sr-only">Steps of run {runId} in pinned definition order</caption>
          <thead>
            <tr>
              {['step', 'status · effect', 'started (utc) · duration', 'placement'].map(
                (column) => (
                  <th key={column} scope="col" className={HEAD}>
                    {column}
                  </th>
                ),
              )}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr
                key={row.stepId}
                data-workflow-run-step={row.stepId}
                data-step-status={row.status}
                className={cn(!row.declared && 'bg-surface-2')}
              >
                <th scope="row" className={cn(CELL, 'td-value break-words text-left text-text-primary')}>
                  <span className="mr-1 text-text-muted" data-cell="numeric">
                    {row.index}
                  </span>
                  {row.stepId}
                  {row.declared ? null : (
                    <span className="block text-text-muted">not in pinned definition</span>
                  )}
                </th>
                <td className={CELL}>
                  <span className="flex flex-col gap-0.5">
                    <span className="inline-flex items-center gap-1.5">
                      <Lamp tone={stepStatusTone(row.status)} />
                      <span className="uppercase tracking-[0.08em] text-text-secondary">
                        {row.status === 'absent' ? 'absent from projection' : row.status}
                      </span>
                    </span>
                    {row.effect === null ? (
                      <span className="text-text-muted">no effect receipt</span>
                    ) : (
                      <span className="uppercase tracking-[0.08em] text-text-secondary">
                        effect {row.effect}
                      </span>
                    )}
                  </span>
                </td>
                <td className={cn(CELL, 'td-value text-text-secondary')} data-cell="numeric">
                  <span className="flex flex-col gap-0.5">
                    {row.startedAt === null ? (
                      <span className="text-text-muted">not started</span>
                    ) : (
                      <ClockStamp micros={row.startedAt} />
                    )}
                    {row.durationMicros === null ? (
                      <span className="text-text-muted">
                        {row.startedAt === null ? '—' : 'not settled'}
                      </span>
                    ) : (
                      <span>{formatDurationMicros(row.durationMicros)}</span>
                    )}
                  </span>
                </td>
                <td className={cn(CELL, 'text-text-secondary')}>
                  {row.placement === null ? (
                    <span className="text-text-muted">no placement receipt</span>
                  ) : (
                    <span className="td-value flex flex-col gap-0.5 break-words">
                      <span>{row.placement.backend}</span>
                      <span className="text-text-muted">{row.placement.model}</span>
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/** A UTC instant that may break between its date and its time in a narrow
 * column, so the table never scrolls just to keep one stamp on one line. The
 * text content stays `YYYY-MM-DD HH:MM:SS`. */
function ClockStamp({ micros }: { micros: number }) {
  const [date, time] = formatMicrosUtcClock(micros).split(' ');
  return (
    <span className="inline-flex flex-wrap gap-x-1">
      <span>{date}</span> <span>{time}</span>
    </span>
  );
}

/** The journal, verbatim: sequence, instant, event kind, and the step it
 * names. Bounded in height and scrollable; nothing is summarised away. */
function RunJournal({ events }: { events: readonly WorkflowRunEvent[] }) {
  return (
    <div className="flex min-w-0 flex-col gap-1 border-t border-edge-subtle pt-2">
      <span className="td-legend">run journal · {events.length} events</span>
      {events.length === 0 ? (
        <p className="text-3xs text-text-muted">The served projection carries no events.</p>
      ) : (
        <div
          role="region"
          aria-label="Run journal events"
          tabIndex={0}
          className="max-h-56 min-w-0 overflow-auto"
        >
          <table className="w-full min-w-[20rem] border-collapse text-3xs" data-workflow-run-journal={events.length}>
            <caption className="sr-only">Run journal events in sequence order</caption>
            <thead>
              <tr>
                {['seq', 'occurred (utc)', 'event', 'step'].map((column) => (
                  <th key={column} scope="col" className={HEAD}>
                    {column}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {[...events]
                .sort((a, b) => a.sequence - b.sequence)
                .map((event) => (
                  <tr key={`${event.sequence}:${event.command_id}`}>
                    <td className={cn(CELL, 'td-value text-text-muted')} data-cell="numeric">
                      {event.sequence}
                    </td>
                    <td className={cn(CELL, 'td-value text-text-secondary')} data-cell="numeric">
                      {formatMicrosUtcClock(event.occurred_at)}
                    </td>
                    <td className={cn(CELL, 'td-value text-text-secondary')}>{event.event.type}</td>
                    <td className={cn(CELL, 'td-value text-text-secondary')}>
                      {'step_id' in event.event ? (
                        event.event.step_id
                      ) : (
                        <span className="text-text-muted">— run</span>
                      )}
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
