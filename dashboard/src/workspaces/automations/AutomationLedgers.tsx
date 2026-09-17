import {
  tallied,
  talliedFactReceipts,
  type AutomaticFactReceipt,
  type JobRow,
  type RunRow,
  type SkillRow,
} from '../../data/query/automation.ts';
import { Panel } from '../../ui/instrument.tsx';
import { Absent, Cell, InspectRow, LedgerTable, ToneWord } from './LedgerTable.tsx';
import {
  epochSeconds,
  formatUtc,
  jobScheduleWord,
  jobTaskKey,
  latestRunForKey,
  receiptStateTone,
  runStatusTone,
  sameInspected,
  skillStateTone,
  type Inspected,
} from './ledger.ts';

/**
 * The three smaller ledgers: user-defined jobs, managed skills, and the
 * terminal automatic fact receipts. Each reads one route and reports its own
 * tally; a job is joined to the run ledger only on its exact `task_key`.
 */

interface InspectProps {
  inspected: Inspected | null;
  pinned: Inspected | null;
  onInspect: (next: Inspected) => void;
  onSelect: (next: Inspected) => void;
  onLeave: () => void;
}

function PartialNotice({ reason }: { reason: string }) {
  return (
    <p role="status" className="border-b border-edge-subtle px-3 py-1.5 text-2xs leading-relaxed text-text-secondary">
      Showing a partial list: {reason}.
    </p>
  );
}

function EmptyNotice({ children }: { children: string }) {
  return <p className="px-3 py-3 text-2xs text-text-muted">{children}</p>;
}

/* ---- user jobs ---------------------------------------------------------- */

export function UserJobsLedger({
  jobs,
  count,
  runs,
  ...inspect
}: {
  jobs: readonly JobRow[];
  count: number;
  /** Loaded ledger rows, or null while that read is blocked. */
  runs: readonly RunRow[] | null;
} & InspectProps) {
  const reading = tallied(jobs, count, 'jobs');
  return (
    <Panel legend="Managed jobs · user defined" elevation="well" bodyClassName="p-0">
      {reading.complete ? null : <PartialNotice reason={reading.reason} />}
      {reading.rows.length === 0 ? (
        reading.complete ? <EmptyNotice>no automation jobs defined</EmptyNotice> : null
      ) : (
        <LedgerTable
          columns={['job', 'schedule', 'state', 'last run (loaded page)', 'outcome']}
          caption={`User-defined automation jobs: ${reading.rows.length} listed`}
          onPointerLeave={inspect.onLeave}
        >
          {reading.rows.map((job) => {
            const identity: Inspected = { kind: 'job', jobId: job.id };
            const latest = runs === null ? undefined : latestRunForKey(runs, jobTaskKey(job.id));
            return (
              <InspectRow
                key={job.id}
                testId={`job-row-${job.id}`}
                label={`Job ${job.name}: ${job.enabled ? 'enabled' : 'disabled'}, ${jobScheduleWord(job)}`}
                inspected={sameInspected(inspect.inspected, identity)}
                selected={sameInspected(inspect.pinned, identity)}
                onInspect={() => inspect.onInspect(identity)}
                onSelect={() => inspect.onSelect(identity)}
                identity={
                  <span className="flex min-w-0 flex-col gap-0.5">
                    <span className="truncate text-2xs text-text-primary">{job.name}</span>
                    <span className="td-value truncate text-3xs text-text-muted">{jobTaskKey(job.id)}</span>
                  </span>
                }
              >
                <Cell numeric>{jobScheduleWord(job)}</Cell>
                <Cell>
                  <ToneWord
                    tone={job.enabled ? runStatusTone('succeeded') : runStatusTone('skipped')}
                    word={job.enabled ? 'enabled' : 'disabled'}
                  />
                </Cell>
                <Cell numeric>
                  {runs === null ? (
                    <Absent>ledger read blocked</Absent>
                  ) : latest === undefined ? (
                    <Absent>none in loaded page</Absent>
                  ) : (
                    <Stamp stamp={latest.started_at} />
                  )}
                </Cell>
                <Cell>
                  {latest ? <ToneWord tone={runStatusTone(latest.status)} word={latest.status} /> : <Absent>n/a</Absent>}
                </Cell>
              </InspectRow>
            );
          })}
        </LedgerTable>
      )}
    </Panel>
  );
}

function Stamp({ stamp }: { stamp: string }) {
  const secs = epochSeconds(stamp);
  return <>{secs === null ? stamp || <Absent>empty stamp</Absent> : formatUtc(secs)}</>;
}

/* ---- managed skills ----------------------------------------------------- */

export function SkillsLedger({ skills, count }: { skills: readonly SkillRow[]; count: number }) {
  const reading = tallied(skills, count, 'managed skills');
  return (
    <Panel legend="Skills · managed" elevation="well" bodyClassName="p-0">
      {reading.complete ? null : <PartialNotice reason={reading.reason} />}
      {reading.rows.length === 0 ? (
        reading.complete ? <EmptyNotice>no managed skills have been activated</EmptyNotice> : null
      ) : (
        <LedgerTable columns={['skill', 'state', 'authority', 'targets']} caption={`Managed skills: ${reading.rows.length} listed`}>
          {reading.rows.map((skill) => {
            const meta = skill.metadata;
            return (
              <tr key={meta.id} className="border-b border-edge-subtle last:border-b-0" data-testid={`skill-row-${meta.id}`}>
                <Cell className="py-2">
                  <span className="flex min-w-0 flex-col gap-0.5">
                    <span className="truncate text-2xs text-text-primary">{meta.title}</span>
                    <span className="td-value truncate text-3xs text-text-muted">
                      {meta.id}
                      {meta.category ? ` · ${meta.category}` : ''}
                    </span>
                  </span>
                </Cell>
                <Cell>
                  <ToneWord tone={skillStateTone(meta.state)} word={meta.state} />
                </Cell>
                <Cell>
                  {meta.provenance ? (
                    <span className="flex min-w-0 flex-col gap-0.5">
                      <span className="td-value text-2xs text-text-secondary">{meta.provenance.source.replaceAll('_', ' ')}</span>
                      <span className="truncate text-3xs text-text-muted">
                        {meta.provenance.actor}
                        {meta.provenance.run_id ? ` · ${meta.provenance.run_id}` : ''}
                      </span>
                    </span>
                  ) : (
                    <Absent>not served</Absent>
                  )}
                </Cell>
                <Cell>
                  {meta.targets && meta.targets.length > 0 ? (
                    <span className="td-value text-3xs text-text-secondary">{meta.targets.join(' · ')}</span>
                  ) : (
                    <Absent>not served</Absent>
                  )}
                </Cell>
              </tr>
            );
          })}
        </LedgerTable>
      )}
    </Panel>
  );
}

/* ---- automatic fact outcomes ------------------------------------------- */

export function FactOutcomesLedger({
  receipts,
  count,
  limit,
  ...inspect
}: {
  receipts: readonly AutomaticFactReceipt[];
  count: number;
  limit: number;
} & InspectProps) {
  const reading = talliedFactReceipts(receipts, count, limit);
  return (
    <Panel legend="Automatic fact outcomes · newest first" elevation="well" bodyClassName="p-0">
      {reading.complete ? null : <PartialNotice reason={reading.reason} />}
      {reading.rows.length === 0 ? (
        reading.complete ? <EmptyNotice>no fact application outcomes are recorded</EmptyNotice> : null
      ) : (
        <LedgerTable
          columns={['recorded (utc)', 'run', 'state', 'fact', 'evidence']}
          caption={`Automatic fact outcomes: ${reading.rows.length} receipts, newest first`}
          onPointerLeave={inspect.onLeave}
        >
          {reading.rows.map((receipt) => {
            const identity: Inspected = { kind: 'receipt', applyId: receipt.apply_id };
            const content = receipt.add_fact_request.content;
            return (
              <InspectRow
                key={receipt.apply_id}
                testId={`receipt-row-${receipt.apply_id}`}
                label={`Fact receipt ${receipt.apply_id}: ${receipt.state}`}
                inspected={sameInspected(inspect.inspected, identity)}
                selected={sameInspected(inspect.pinned, identity)}
                onInspect={() => inspect.onInspect(identity)}
                onSelect={() => inspect.onSelect(identity)}
                identity={
                  <span className="flex min-w-0 flex-col gap-0.5">
                    <span className="td-value text-2xs text-text-primary">
                      {formatUtc(Math.floor(receipt.recorded_at_micros / 1_000_000))}
                    </span>
                    <span className="td-value truncate text-3xs text-text-muted">{receipt.apply_id}</span>
                  </span>
                }
              >
                <Cell numeric>
                  <span className="truncate">{receipt.run_id}</span>
                </Cell>
                <Cell>
                  <ToneWord tone={receiptStateTone(receipt.state)} word={receipt.state} />
                </Cell>
                <Cell className="max-w-[28rem]">
                  {content !== undefined ? (
                    <span className="line-clamp-2 break-words text-2xs text-text-secondary">{content}</span>
                  ) : (
                    <Absent>receipt carries no fact text</Absent>
                  )}
                  {receipt.quarantine_reason ? (
                    <span className="block truncate text-3xs text-state-error">quarantine: {receipt.quarantine_reason}</span>
                  ) : null}
                </Cell>
                <Cell numeric>
                  {receipt.evidence_hash ? (
                    <span className="text-3xs">{receipt.evidence_hash.slice(0, 16)}</span>
                  ) : (
                    <Absent>none</Absent>
                  )}
                </Cell>
              </InspectRow>
            );
          })}
        </LedgerTable>
      )}
    </Panel>
  );
}
