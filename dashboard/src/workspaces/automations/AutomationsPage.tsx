import { useCallback, useMemo, useState, type KeyboardEvent, type ReactNode } from 'react';

import {
  AutomationSchedulerStatusV1Schema,
  type AutomationSchedulerStatusV1,
} from '../../contracts/generated.ts';
import {
  automationRunsReading,
  automationSchedulerKey,
  schedulerStatusUrl,
  useAutomationFactReceipts,
  useAutomationJobs,
  useAutomationRuns,
  useAutomationSkills,
  useSchedulerControl,
  type SchedulerControlResult,
} from '../../data/query/automation.ts';
import { usePayload } from '../../data/query/usePayload.ts';
import { scopeWriteSentence } from '../../data/scope/store.ts';
import { Corners, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import { PayloadBoundary, payloadReadState } from '../../ui/ReadSection.tsx';
import { FactOutcomesLedger, SkillsLedger, UserJobsLedger } from './AutomationLedgers.tsx';
import { RunInspector } from './RunInspector.tsx';
import { RunLedger } from './RunLedger.tsx';
import { LedgerReadouts, SchedulerBay } from './SchedulerBay.tsx';
import { ledgerWindow, receiptsByRun, sameInspected, type Inspected } from './ledger.ts';

/**
 * Automations, channel nine. Scheduler status and its one real control,
 * managed jobs, skills, automatic fact outcomes, and the durable run ledger,
 * with an inspector for whichever identity is hovered, focused or selected.
 *
 * Automation is daemon-owned. This surface reports the scheduler's readings
 * and the ledger's records; it never asks a browser operator to approve a
 * draft, and it draws no Retry, Cancel or Run-now control because the daemon
 * exposes no dashboard mutation path that returns a durable receipt for them.
 *
 * Five independent reads, five independent states: a failed jobs read is a
 * failed jobs read inside the jobs panel, beside a scheduler that answered.
 */
export function AutomationsPage() {
  const scheduler = usePayload(automationSchedulerKey, schedulerStatusUrl, AutomationSchedulerStatusV1Schema);
  const control = useSchedulerControl();
  const jobs = useAutomationJobs();
  const skills = useAutomationSkills();
  const receipts = useAutomationFactReceipts();
  const runs = useAutomationRuns();

  // Hover and focus write the transient slot; click writes the pinned one.
  // The inspector shows the transient identity while there is one, so a hover
  // previews without disturbing the selection, and yields back to the pinned
  // identity when the pointer leaves the table.
  const [pinned, setPinned] = useState<Inspected | null>(null);
  const [transient, setTransient] = useState<Inspected | null>(null);
  const inspected = transient ?? pinned;

  const inspect = useCallback((next: Inspected) => setTransient(next), []);
  const leave = useCallback(() => setTransient(null), []);
  const select = useCallback((next: Inspected) => {
    setPinned((current) => (sameInspected(current, next) ? null : next));
    setTransient(null);
  }, []);
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'Escape') return;
    setPinned(null);
    setTransient(null);
  };

  // Each source resolves to its rows or to null while blocked, so the
  // inspector and the joins can say "read blocked" rather than "not found".
  const status: AutomationSchedulerStatusV1 | null =
    scheduler.data?.outcome === 'ok' ? scheduler.data.data : null;
  const runsData = runs.data;
  const runsReading = useMemo(
    () => (runsData?.outcome === 'ok' ? automationRunsReading(runsData.data) : null),
    [runsData],
  );
  const window = useMemo(() => (runsReading ? ledgerWindow(runsReading) : null), [runsReading]);
  const runRows = runsReading?.rows ?? null;
  const receiptRows = receipts.data?.outcome === 'ok' ? receipts.data.data.receipts : null;
  const receiptsMap = useMemo(() => (receiptRows ? receiptsByRun(receiptRows) : null), [receiptRows]);
  const jobRows = jobs.data?.outcome === 'ok' ? jobs.data.data.jobs : null;
  const runsRead = payloadReadState(runs.isPending, runs.data);

  const inspectProps = { inspected, pinned, onInspect: inspect, onSelect: select, onLeave: leave };

  return (
    <div data-testid="automations-page" className="flex h-full min-h-0 min-w-0 flex-col" onKeyDown={onKeyDown}>
      <WorkspaceHeader
        path="automations"
        title="Automations"
        note="scheduler readings, managed jobs, skills, fact outcomes and the durable run ledger · daemon-owned automation authorities"
      />
      <div className="flex min-h-0 min-w-0 flex-1 max-lg:flex-col">
        <div
          role="region"
          aria-label="Automations content"
          tabIndex={0}
          className="relative flex min-w-0 flex-1 flex-col gap-3 p-3 lg:min-h-0 lg:overflow-auto"
        >
          <Corners />
          <Ticks />

          <Bay label="Scheduler">
            <PayloadBoundary title="Scheduler" pending={scheduler.isPending} result={scheduler.data}>
              {(data) => (
                <SchedulerBay
                  status={data}
                  control={{
                    pending: control.isPending,
                    failure: controlFailure(control.data),
                    writability: control.writability,
                    onToggle: (paused) => control.mutate(paused),
                  }}
                  runs={runRows}
                  {...inspectProps}
                />
              )}
            </PayloadBoundary>
          </Bay>

          <LedgerReadouts
            status={status}
            window={window}
            ledgerBlocked={runsRead.kind === 'blocked' ? `ledger ${runsRead.state.replaceAll('_', ' ')}` : null}
          />

          <div className="grid gap-3 xl:grid-cols-[minmax(0,5fr)_minmax(0,4fr)]">
            <Bay label="Jobs">
              <PayloadBoundary title="Jobs" pending={jobs.isPending} result={jobs.data}>
                {(data) => <UserJobsLedger jobs={data.jobs} count={data.count} runs={runRows} {...inspectProps} />}
              </PayloadBoundary>
            </Bay>
            <Bay label="Managed skills">
              <PayloadBoundary title="Managed skills" pending={skills.isPending} result={skills.data}>
                {(data) => <SkillsLedger skills={data.skills} count={data.count} />}
              </PayloadBoundary>
            </Bay>
          </div>

          <Bay label="Fact application outcomes">
            <PayloadBoundary title="Fact application outcomes" pending={receipts.isPending} result={receipts.data}>
              {(data) => (
                <FactOutcomesLedger receipts={data.receipts} count={data.count} limit={data.limit} {...inspectProps} />
              )}
            </PayloadBoundary>
          </Bay>

          <Bay label="Run ledger">
            <PayloadBoundary title="Run ledger" pending={runs.isPending} result={runs.data}>
              {(data) => {
                const reading = automationRunsReading(data);
                return (
                  <RunLedger
                    reading={reading}
                    window={ledgerWindow(reading)}
                    receipts={receiptsMap}
                    {...inspectProps}
                  />
                );
              }}
            </PayloadBoundary>
          </Bay>
        </div>

        <aside
          aria-label="Inspector"
          className="w-[24rem] shrink-0 bg-surface-1 max-xl:w-80 max-lg:w-full lg:min-h-0 lg:overflow-auto lg:border-l lg:border-edge-subtle max-lg:border-t max-lg:border-edge-subtle"
        >
          <RunInspector
            inspected={inspected}
            pinned={inspected !== null && sameInspected(inspected, pinned)}
            runs={runRows}
            tasks={status?.tasks ?? null}
            jobs={jobRows}
            receipts={receiptRows}
            receiptsByRun={receiptsMap}
            onSelect={select}
          />
        </aside>
      </div>
    </div>
  );
}

/** One read's landmark, present in every state of that read so a blocked
 * jobs read is still found under "Jobs" beside neighbours that answered. */
function Bay({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section aria-label={label} className="flex min-w-0 flex-col">
      {children}
    </section>
  );
}

/** The sentence a failed or undispatched control attempt is reported with.
 * Exhaustive over the control result so a new outcome fails to build rather
 * than rendering as nothing. */
function controlFailure(result: SchedulerControlResult | undefined): string | null {
  if (result === undefined || result.outcome === 'ok') return null;
  switch (result.outcome) {
    case 'offline':
      return 'The daemon did not answer, so the scheduler was not changed.';
    case 'unauthorized':
      return 'The daemon accepted no identity for the change, so the scheduler was not changed.';
    case 'denied':
      return 'This identity is not permitted to control the scheduler, so it was not changed.';
    case 'error':
      return `The daemon refused the change (${result.detail}).`;
    case 'unsupported_schema':
      return 'The daemon answered in a shape this dashboard cannot read, so whether the scheduler changed is unknown, reload to re-read it.';
    case 'unavailable':
      return `The scheduler was not changed: ${result.reason ?? result.status}.`;
    case 'read_only_scope':
      return `The scheduler was not changed: ${result.refusal.detail}.`;
    case 'not_dispatched':
      return scopeWriteSentence(result.writability, {
        writable: (target) => `Nothing was sent, though writes to ${target} are accepted, reload to re-read the scheduler.`,
        refused: (reason) => `Nothing was sent. ${reason}`,
      });
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
