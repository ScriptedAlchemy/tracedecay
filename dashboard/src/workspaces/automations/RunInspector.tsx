import { useState, type ReactNode } from 'react';

import type { AutomationTaskStatusV1 } from '../../contracts/generated.ts';
import {
  useAutomationRunArtifactPayload,
  useAutomationRunArtifacts,
  type AutomaticFactReceipt,
  type JobRow,
  type RunArtifactRow,
  type RunArtifactsPayload,
  type RunRow,
} from '../../data/query/automation.ts';
import { cn } from '../../ui/cn';
import { Corners } from '../../ui/instrument.tsx';
import { PayloadBoundary } from '../../ui/ReadSection.tsx';
import { Absent, Term, ToneWord } from './LedgerTable.tsx';
import {
  artifactPayloadBelongsTo,
  epochSeconds,
  failureClassTone,
  formatDuration,
  formatUtc,
  integrityTone,
  jobScheduleWord,
  jobTaskKey,
  latestRunForKey,
  missingArtifactKinds,
  readLastSchedulerRun,
  receiptStateTone,
  runIsTerminal,
  runStatusTone,
  runTiming,
  type Inspected,
  type RunReceipts,
} from './ledger.ts';

/**
 * The inspector: exact evidence for whichever identity is being inspected.
 *
 * A run shows its own ledger record, the fact receipts filed under its id,
 * and — read only once the run is inspected — the daemon's artifact list and
 * chain-integrity verdict, then one artifact's payload once that artifact is
 * chosen. A scheduler task, a user job and a fact receipt each show their own
 * record and link to a run only through an exact recorded `run_id`.
 */
export function RunInspector({
  inspected,
  pinned,
  runs,
  tasks,
  jobs,
  receipts,
  receiptsByRun,
  onSelect,
}: {
  inspected: Inspected | null;
  /** Whether the inspected identity is the pinned selection or a preview. */
  pinned: boolean;
  /** Each source is null while its read is blocked, so the inspector can say
   * "not readable" rather than "not found". */
  runs: readonly RunRow[] | null;
  tasks: readonly AutomationTaskStatusV1[] | null;
  jobs: readonly JobRow[] | null;
  receipts: readonly AutomaticFactReceipt[] | null;
  receiptsByRun: ReadonlyMap<string, RunReceipts> | null;
  onSelect: (next: Inspected) => void;
}) {
  return (
    <section
      aria-label="Run inspector"
      className="relative flex min-h-full min-w-0 flex-col border border-edge-subtle bg-surface-1"
      data-testid="run-inspector"
    >
      <Corners tone={inspected ? 'signal' : 'edge'} />
      <header className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <h2 className="td-title truncate">Run inspector</h2>
        <span aria-hidden className="td-rule" />
        <span className="td-legend shrink-0" data-inspector-mode={inspected ? (pinned ? 'selected' : 'preview') : 'empty'}>
          {inspected ? (pinned ? 'selected' : 'preview') : 'no selection'}
        </span>
      </header>
      <div className="flex min-w-0 flex-1 flex-col gap-3 p-3">
        {inspected === null ? (
          <EmptyInspector />
        ) : (
          <InspectedBody
            inspected={inspected}
            runs={runs}
            tasks={tasks}
            jobs={jobs}
            receipts={receipts}
            receiptsByRun={receiptsByRun}
            onSelect={onSelect}
          />
        )}
      </div>
      <footer className="shrink-0 border-t border-edge-subtle px-2.5 py-1.5 text-3xs text-text-muted">
        read-only · every value is the daemon&apos;s own ledger, receipt or verdict
      </footer>
    </section>
  );
}

function EmptyInspector() {
  return (
    <div className="flex flex-col gap-2 text-2xs leading-relaxed text-text-muted">
      <p className="text-text-secondary">No run, task, job or receipt is selected.</p>
      <p>Hover or focus a ledger row to preview it here. Click or press Enter to select it; Escape clears the selection.</p>
    </div>
  );
}

function InspectedBody({
  inspected,
  runs,
  tasks,
  jobs,
  receipts,
  receiptsByRun,
  onSelect,
}: {
  inspected: Inspected;
  runs: readonly RunRow[] | null;
  tasks: readonly AutomationTaskStatusV1[] | null;
  jobs: readonly JobRow[] | null;
  receipts: readonly AutomaticFactReceipt[] | null;
  receiptsByRun: ReadonlyMap<string, RunReceipts> | null;
  onSelect: (next: Inspected) => void;
}) {
  switch (inspected.kind) {
    case 'run': {
      if (runs === null) return <Gap>the run ledger read is blocked, so this run cannot be shown</Gap>;
      const run = runs.find((row) => row.run_id === inspected.runId);
      if (!run) return <Gap>run {inspected.runId} is not in the loaded ledger page</Gap>;
      return (
        <RunDetail
          run={run}
          receipts={receiptsByRun === null ? null : (receiptsByRun.get(run.run_id) ?? { applied: 0, quarantined: 0, rows: [] })}
          onSelect={onSelect}
        />
      );
    }
    case 'task': {
      if (tasks === null) return <Gap>the scheduler status read is blocked, so this task cannot be shown</Gap>;
      const task = tasks.find((row) => row.task === inspected.task);
      if (!task) return <Gap>task {inspected.task} is not in the scheduler status</Gap>;
      return <TaskDetail task={task} runs={runs} onSelect={onSelect} />;
    }
    case 'job': {
      if (jobs === null) return <Gap>the jobs read is blocked, so this job cannot be shown</Gap>;
      const job = jobs.find((row) => row.id === inspected.jobId);
      if (!job) return <Gap>job {inspected.jobId} is not in the jobs list</Gap>;
      return <JobDetail job={job} runs={runs} onSelect={onSelect} />;
    }
    case 'receipt': {
      if (receipts === null) return <Gap>the fact receipt read is blocked, so this receipt cannot be shown</Gap>;
      const receipt = receipts.find((row) => row.apply_id === inspected.applyId);
      if (!receipt) return <Gap>receipt {inspected.applyId} is not in the loaded receipt page</Gap>;
      return <ReceiptDetail receipt={receipt} runs={runs} onSelect={onSelect} />;
    }
    default: {
      const exhaustive: never = inspected;
      return exhaustive;
    }
  }
}

function Gap({ children }: { children: ReactNode }) {
  return (
    <p role="status" className="text-2xs leading-relaxed text-text-secondary">
      {children}
    </p>
  );
}

function Box({ legend, trailing, children }: { legend: string; trailing?: ReactNode; children: ReactNode }) {
  return (
    <section aria-label={legend} className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-0 p-2.5">
      <div className="flex items-center gap-2">
        <h3 className="td-legend text-text-secondary">{legend}</h3>
        <span aria-hidden className="td-rule" />
        {trailing}
      </div>
      {children}
    </section>
  );
}

/** A link to another inspectable identity, rendered as a compact control. */
function InspectLink({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="group inline-flex min-h-[var(--touch-target-min)] min-w-0 max-w-full items-center text-left"
    >
      <span className="break-all border-b border-accent/50 text-2xs text-accent group-hover:border-accent">{children}</span>
    </button>
  );
}

/* ---- run ---------------------------------------------------------------- */

function RunDetail({
  run,
  receipts,
  onSelect,
}: {
  run: RunRow;
  receipts: RunReceipts | null;
  onSelect: (next: Inspected) => void;
}) {
  const timing = runTiming(run);
  const tone = runStatusTone(run.status);
  return (
    <>
      <div className="flex min-w-0 flex-col gap-1 border-b border-edge-subtle pb-2">
        <span className="td-value text-sm text-text-primary">
          {timing.kind === 'unparsed' ? timing.startedAt : `${formatUtc(timing.startedAt)} UTC`}
        </span>
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
          <span className="td-value min-w-0 break-all text-2xs text-text-secondary">{run.task_key ?? run.task}</span>
          <ToneWord tone={tone} word={run.status} className="text-2xs" />
        </div>
        <span className="td-value break-all text-3xs text-text-muted">{run.run_id}</span>
      </div>

      <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
        <Term label="duration" mono>
          {timing.kind === 'measured' ? (
            formatDuration(timing.durationSecs)
          ) : timing.kind === 'open' ? (
            <Absent>run not settled</Absent>
          ) : timing.kind === 'inverted' ? (
            <Absent>completed before it started</Absent>
          ) : (
            <Absent>stamps unparsed</Absent>
          )}
        </Term>
        <Term label="trigger" mono>{run.trigger}</Term>
        <Term label="backend" mono>
          {run.backend}
          {run.model ? ` · ${run.model}` : ''}
        </Term>
        <Term label="backend attempts" mono>{run.backend_attempt_count}</Term>
        <Term label="reviewed / accepted" mono>
          {run.reviewed_count} / {run.accepted_count}
        </Term>
        <Term label="rejected / skipped" mono>
          {run.rejected_count} / {run.skipped_count}
        </Term>
        <Term label="started" mono>
          {timing.kind === 'unparsed' ? timing.startedAt : formatUtc(timing.startedAt)}
        </Term>
        <Term label="completed" mono>
          {timing.kind === 'measured' || timing.kind === 'inverted'
            ? formatUtc(timing.completedAt)
            : timing.kind === 'open'
              ? <Absent>not settled</Absent>
              : timing.completedAt || <Absent>empty stamp</Absent>}
        </Term>
      </dl>

      {run.error !== null || run.error_classification !== null ? (
        <Box legend="typed error">
          {run.error !== null ? (
            <p className="break-words text-2xs leading-relaxed text-state-error">{run.error}</p>
          ) : null}
          <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
            <Term label="class">
              {run.error_classification !== null ? (
                <ToneWord tone={failureClassTone(run.error_classification)} word={run.error_classification.replaceAll('_', ' ')} />
              ) : (
                <Absent>not classified</Absent>
              )}
            </Term>
            <Term label="retryable">
              {run.error_retryable === null ? <Absent>not recorded</Absent> : run.error_retryable ? 'yes' : 'no'}
            </Term>
          </dl>
        </Box>
      ) : null}

      <Box legend={`fact receipts${receipts ? ` (${receipts.rows.length})`: ''}`}>
        {receipts === null ? (
          <Gap>the fact receipt read is blocked</Gap>
        ) : receipts.rows.length === 0 ? (
          <p className="text-2xs text-text-muted">no fact receipt in the loaded receipt page names this run</p>
        ) : (
          <ul className="flex flex-col gap-1">
            {receipts.rows.map((receipt) => (
              <li key={receipt.apply_id} className="flex min-w-0 items-center justify-between gap-2">
                <InspectLink onClick={() => onSelect({ kind: 'receipt', applyId: receipt.apply_id })}>
                  <span className="td-value">{receipt.apply_id}</span>
                </InspectLink>
                <ToneWord tone={receiptStateTone(receipt.state)} word={receipt.state} className="text-2xs" />
              </li>
            ))}
          </ul>
        )}
      </Box>

      <RunArtifacts runId={run.run_id} recordedKinds={run.artifact_kinds} />
    </>
  );
}

/** The artifact list is read only for an inspected run that recorded any:
 * a run whose ledger row lists no artifacts has nothing to verify, and the
 * inspector says so without a request. */
function RunArtifacts({ runId, recordedKinds }: { runId: string; recordedKinds: readonly string[] }) {
  const artifacts = useAutomationRunArtifacts(runId, recordedKinds.length > 0);
  return (
    <Box legend={`artifacts (${recordedKinds.length})`}>
      {recordedKinds.length === 0 ? (
        <>
          <p className="text-2xs text-text-muted">this run recorded no artifacts in its ledger entry</p>
          <dl className="grid gap-y-2">
            <Term label="integrity verdict (daemon)">
              <Absent>no artifact to verify</Absent>
            </Term>
          </dl>
        </>
      ) : (
        <PayloadBoundary title="Artifacts" pending={artifacts.isPending} result={artifacts.data}>
          {(data) => <ArtifactList runId={runId} data={data} />}
        </PayloadBoundary>
      )}
    </Box>
  );
}

function ArtifactList({ runId, data }: { runId: string; data: RunArtifactsPayload }) {
  const [selectedKind, setSelectedKind] = useState<string | null>(null);
  const chain = data.artifact_chain;
  const missing = missingArtifactKinds(chain);
  const selected = data.artifacts.find((artifact) => artifact.kind === selectedKind) ?? null;
  const verdictTone = integrityTone(chain.integrity_status);
  return (
    <>
      <dl className="grid gap-y-2">
        <Term label="integrity verdict (daemon)">
          <ToneWord tone={verdictTone} word={chain.integrity_status.replaceAll('_', ' ')} />
        </Term>
        <Term label="chain">
          {chain.present_kinds.length} of {chain.expected_kinds.length} expected kinds present
          {missing.length > 0 ? ` · not recorded: ${missing.map((kind) => kind.replaceAll('_', ' ')).join(', ')}` : ''}
        </Term>
      </dl>
      {data.run_id !== runId ? (
        <Gap>the artifact list names run {data.run_id}, not this run</Gap>
      ) : (
        <ul className="flex flex-col">
          {data.artifacts.map((artifact) => (
            <ArtifactLine
              key={artifact.kind}
              artifact={artifact}
              selected={artifact.kind === selectedKind}
              onSelect={() => setSelectedKind((current) => (current === artifact.kind ? null : artifact.kind))}
            />
          ))}
        </ul>
      )}
      {selected !== null && data.run_id === runId ? <ArtifactPayload runId={runId} artifact={selected} /> : null}
    </>
  );
}

function ArtifactLine({
  artifact,
  selected,
  onSelect,
}: {
  artifact: RunArtifactRow;
  selected: boolean;
  onSelect: () => void;
}) {
  const created = epochSeconds(artifact.created_at);
  return (
    <li className="border-b border-edge-subtle last:border-b-0">
      <button
        type="button"
        aria-pressed={selected}
        onClick={onSelect}
        className={cn(
          'relative flex min-h-[var(--touch-target-min)] w-full min-w-0 flex-col justify-center gap-0.5 py-1 pl-2.5 pr-1 text-left hover:bg-surface-1',
          selected && 'bg-surface-2',
        )}
      >
        <span aria-hidden className={cn('absolute inset-y-0 left-0 w-[3px]', selected ? 'bg-accent' : 'bg-transparent')} />
        <span className="flex min-w-0 items-baseline justify-between gap-2">
          <span className="td-value break-words text-2xs text-text-primary">{artifact.kind.replaceAll('_', ' ')}</span>
          <span className="td-value shrink-0 text-3xs text-text-muted">{artifact.sha256.slice(0, 12)}</span>
        </span>
        <span className="break-words text-3xs text-text-muted">
          {artifact.summary ?? 'no summary recorded'}
          {created !== null ? ` · ${formatUtc(created)}` : ''}
        </span>
      </button>
    </li>
  );
}

function ArtifactPayload({ runId, artifact }: { runId: string; artifact: RunArtifactRow }) {
  const payload = useAutomationRunArtifactPayload(runId, artifact.kind, true);
  return (
    <Box legend="selected artifact payload">
      <dl className="grid gap-y-2">
        <Term label="artifact" mono>{artifact.kind}</Term>
        <Term label="stored at" mono>{artifact.path}</Term>
        <Term label="sha256" mono>{artifact.sha256}</Term>
      </dl>
      <PayloadBoundary title={`${artifact.kind} artifact`} pending={payload.isPending} result={payload.data}>
        {(data) =>
          artifactPayloadBelongsTo(data, runId, artifact) ? (
            <pre
              aria-label={`${artifact.kind} artifact payload`}
              className="max-h-64 overflow-auto whitespace-pre-wrap break-words border border-edge-subtle bg-surface-1 p-2 font-mono text-3xs text-text-secondary"
            >
              {JSON.stringify(data.payload, null, 2)}
            </pre>
          ) : (
            <p role="status" className="text-2xs text-state-error">
              the artifact payload does not belong to this run and kind
            </p>
          )
        }
      </PayloadBoundary>
    </Box>
  );
}

/* ---- scheduler task ---------------------------------------------------- */

function TaskDetail({
  task,
  runs,
  onSelect,
}: {
  task: AutomationTaskStatusV1;
  runs: readonly RunRow[] | null;
  onSelect: (next: Inspected) => void;
}) {
  const last = readLastSchedulerRun(task.last_scheduler_run);
  return (
    <>
      <div className="flex min-w-0 flex-col gap-1 border-b border-edge-subtle pb-2">
        <span className="td-value text-sm text-text-primary">{task.task}</span>
        <span className="text-3xs text-text-muted">built-in scheduler task · reading from the scheduler status route</span>
      </div>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
        <Term label="due now">{task.due ? 'yes' : 'no'}</Term>
        <Term label="skip reason" mono>
          {task.skip_reason ?? <Absent>none recorded</Absent>}
        </Term>
      </dl>
      <Box legend="last scheduler run">
        {last.kind === 'none' ? (
          <p className="text-2xs text-text-muted">no scheduler-triggered run is recorded for this task</p>
        ) : last.kind === 'unreadable' ? (
          <Gap>the scheduler attached a last-run record this build cannot read</Gap>
        ) : (
          <LastRunSummary run={last.run} runs={runs} onSelect={onSelect} />
        )}
      </Box>
    </>
  );
}

function LastRunSummary({
  run,
  runs,
  onSelect,
}: {
  run: { run_id: string; status: string; started_at: string; completed_at: string; error?: string | null | undefined };
  runs: readonly RunRow[] | null;
  onSelect: (next: Inspected) => void;
}) {
  const completed = epochSeconds(run.completed_at);
  const inPage = runs?.some((row) => row.run_id === run.run_id) ?? false;
  return (
    <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
      <Term label="run" mono>
        {inPage ? (
          <InspectLink onClick={() => onSelect({ kind: 'run', runId: run.run_id })}>{run.run_id}</InspectLink>
        ) : (
          <>
            {run.run_id}
            <span className="block text-3xs text-text-muted">
              {runs === null ? 'ledger read blocked' : 'not in the loaded ledger page'}
            </span>
          </>
        )}
      </Term>
      <Term label="outcome">
        <ToneWord tone={runStatusTone(run.status)} word={run.status} />
      </Term>
      <Term label="completed" mono>
        {!runIsTerminal(run.status) ? (
          <Absent>not settled</Absent>
        ) : completed !== null ? (
          formatUtc(completed)
        ) : (
          run.completed_at || <Absent>empty stamp</Absent>
        )}
      </Term>
      <Term label="error">
        {run.error ? <span className="text-state-error">{run.error}</span> : <Absent>none</Absent>}
      </Term>
    </dl>
  );
}

/* ---- user job ---------------------------------------------------------- */

function JobDetail({
  job,
  runs,
  onSelect,
}: {
  job: JobRow;
  runs: readonly RunRow[] | null;
  onSelect: (next: Inspected) => void;
}) {
  const key = jobTaskKey(job.id);
  const latest = runs === null ? undefined : latestRunForKey(runs, key);
  return (
    <>
      <div className="flex min-w-0 flex-col gap-1 border-b border-edge-subtle pb-2">
        <span className="break-words text-sm text-text-primary">{job.name}</span>
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
          <span className="td-value min-w-0 break-all text-2xs text-text-secondary">{key}</span>
          <ToneWord
            tone={job.enabled ? runStatusTone('succeeded') : runStatusTone('skipped')}
            word={job.enabled ? 'enabled' : 'disabled'}
            className="text-2xs"
          />
        </div>
      </div>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
        <Term label="schedule" mono>{jobScheduleWord(job)}</Term>
        <Term label="failure cooldown" mono>
          {job.cooldown_secs != null ? formatDuration(job.cooldown_secs) : <Absent>daemon default</Absent>}
        </Term>
        <Term label="skills" mono>
          {job.skill_ids && job.skill_ids.length > 0 ? job.skill_ids.join(', ') : <Absent>none attached</Absent>}
        </Term>
        <Term label="delivery" mono>
          {job.delivery ? (
            <>
              {job.delivery.mode}
              {job.delivery.path ? ` · ${job.delivery.path}` : ''}
              {job.delivery.url ? ` · ${job.delivery.url}` : ''}
            </>
          ) : (
            <Absent>not served</Absent>
          )}
        </Term>
        <Term label="pre-run command" mono>
          {job.pre_run_command ?? <Absent>none</Absent>}
        </Term>
        <Term label="updated" mono>
          {job.updated_at != null ? formatUtc(job.updated_at) : <Absent>not served</Absent>}
        </Term>
      </dl>
      <Box legend="latest run in loaded page">
        {runs === null ? (
          <Gap>the run ledger read is blocked</Gap>
        ) : latest === undefined ? (
          <p className="text-2xs text-text-muted">no run recorded under {key} in the loaded ledger page</p>
        ) : (
          <LastRunSummary run={latest} runs={runs} onSelect={onSelect} />
        )}
      </Box>
    </>
  );
}

/* ---- fact receipt ------------------------------------------------------ */

function ReceiptDetail({
  receipt,
  runs,
  onSelect,
}: {
  receipt: AutomaticFactReceipt;
  runs: readonly RunRow[] | null;
  onSelect: (next: Inspected) => void;
}) {
  const recorded = Math.floor(receipt.recorded_at_micros / 1_000_000);
  const inPage = runs?.some((row) => row.run_id === receipt.run_id) ?? false;
  const content = receipt.add_fact_request.content;
  return (
    <>
      <div className="flex min-w-0 flex-col gap-1 border-b border-edge-subtle pb-2">
        <span className="td-value text-sm text-text-primary">{formatUtc(recorded)} UTC</span>
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
          <span className="td-value min-w-0 break-all text-2xs text-text-secondary">{receipt.apply_id}</span>
          <ToneWord tone={receiptStateTone(receipt.state)} word={receipt.state} className="text-2xs" />
        </div>
      </div>
      <Box legend="fact">
        {content !== undefined ? (
          <p className="break-words text-2xs leading-relaxed text-text-primary">{content}</p>
        ) : (
          <p className="text-2xs text-text-muted">receipt carries no fact text</p>
        )}
      </Box>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
        <Term label="run" mono>
          {inPage ? (
            <InspectLink onClick={() => onSelect({ kind: 'run', runId: receipt.run_id })}>{receipt.run_id}</InspectLink>
          ) : (
            <>
              {receipt.run_id}
              <span className="block text-3xs text-text-muted">
                {runs === null ? 'ledger read blocked' : 'not in the loaded ledger page'}
              </span>
            </>
          )}
        </Term>
        <Term label="applied fact" mono>
          {receipt.applied_fact_id ?? <Absent>none applied</Absent>}
        </Term>
        <Term label="evidence hash" mono>
          {receipt.evidence_hash ?? <Absent>not recorded</Absent>}
        </Term>
        <Term label="schema" mono>v{receipt.schema_version}</Term>
      </dl>
      {receipt.quarantine_reason ? (
        <Box legend="quarantine">
          <p className="break-words text-2xs leading-relaxed text-state-error">{receipt.quarantine_reason}</p>
        </Box>
      ) : null}
      <Box legend="validation">
        {receipt.validation !== undefined ? (
          <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words border border-edge-subtle bg-surface-1 p-2 font-mono text-3xs text-text-secondary">
            {JSON.stringify(receipt.validation, null, 2)}
          </pre>
        ) : (
          <p className="text-2xs text-text-muted">no validation record attached</p>
        )}
      </Box>
    </>
  );
}
