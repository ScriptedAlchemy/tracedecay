import type { AutomationRunRowV1 } from '../../contracts/generated.ts';
import { useAutomationRunArtifacts, type ListReading } from '../../data/query/automation.ts';
import { Panel } from '../../ui/instrument.tsx';
import { Absent, Cell, InspectRow, LedgerTable, ToneWord } from './LedgerTable.tsx';
import {
  formatDuration,
  formatUtc,
  integrityTone,
  runStatusTone,
  runTiming,
  sameInspected,
  type Inspected,
  type LedgerWindow,
  type RunReceipts,
} from './ledger.ts';

/**
 * The run ledger: the newest page of `/api/automation/runs`, newest first as
 * the route serves it, one row per durable ledger record.
 *
 * Every cell is a measurement of the record or a typed absence. The integrity
 * column is the daemon's chain verdict from the per-run artifacts route, which
 * is read only once a run has been inspected, so a row that has not been
 * inspected says `unchecked`, never `verified`, and a run with no artifacts
 * has nothing to verify and says so.
 */
export function RunLedger({
  reading,
  window,
  receipts,
  inspected,
  pinned,
  onInspect,
  onSelect,
  onLeave,
}: {
  reading: ListReading<AutomationRunRowV1>;
  window: LedgerWindow;
  /** Fact receipts filed by run id, or null when that read is blocked. */
  receipts: ReadonlyMap<string, RunReceipts> | null;
  inspected: Inspected | null;
  pinned: Inspected | null;
  onInspect: (next: Inspected) => void;
  onSelect: (next: Inspected) => void;
  onLeave: () => void;
}) {
  return (
    <Panel
      legend="Run ledger · latest first"
      elevation="well"
      bodyClassName="p-0"
      footer={<LedgerFooter window={window} />}
    >
      {reading.rows.length === 0 ? (
        <p className="px-3 py-3 text-2xs text-text-muted">
          {reading.complete
            ? 'no automation runs are recorded in this ledger'
            : `Showing a partial list: ${reading.reason}.`}
        </p>
      ) : (
        <>
          {reading.complete ? null : (
            <p role="status" className="border-b border-edge-subtle px-3 py-1.5 text-2xs leading-relaxed text-text-secondary">
              Showing a partial list: {reading.reason}.
            </p>
          )}
          <LedgerTable
            columns={['start (utc)', 'task', 'duration', 'outcome', 'fact receipts', 'artifacts', 'integrity']}
            caption={`Run ledger: ${window.loaded} loaded runs, newest first`}
            onPointerLeave={onLeave}
          >
            {reading.rows.map((run) => {
              const identity: Inspected = { kind: 'run', runId: run.run_id };
              return (
                <RunLine
                  key={run.run_id}
                  run={run}
                  receipts={receipts}
                  inspected={sameInspected(inspected, identity)}
                  selected={sameInspected(pinned, identity)}
                  onInspect={() => onInspect(identity)}
                  onSelect={() => onSelect(identity)}
                />
              );
            })}
          </LedgerTable>
        </>
      )}
    </Panel>
  );
}

function LedgerFooter({ window }: { window: LedgerWindow }) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5 text-3xs text-text-muted">
      <span>
        ledger window: newest {window.loaded} {window.loaded === 1 ? 'run' : 'runs'} served by the daemon
        {window.bounded ? ' · bounded' : ' · complete'}
      </span>
      <span className="td-value text-3xs text-text-muted">
        {window.oldestStart !== null && window.newestStart !== null
          ? `${formatUtc(window.oldestStart)} → ${formatUtc(window.newestStart)} UTC`
          : window.loaded === 0
            ? 'no rows'
            : 'start stamps not all parseable'}
      </span>
    </div>
  );
}

function RunLine({
  run,
  receipts,
  inspected,
  selected,
  onInspect,
  onSelect,
}: {
  run: AutomationRunRowV1;
  receipts: ReadonlyMap<string, RunReceipts> | null;
  inspected: boolean;
  selected: boolean;
  onInspect: () => void;
  onSelect: () => void;
}) {
  const timing = runTiming(run);
  const tone = runStatusTone(run.status);
  const joined = receipts?.get(run.run_id);
  return (
    <InspectRow
      testId={`run-row-${run.run_id}`}
      label={`Run ${run.run_id}: ${run.task_key ?? run.task}, ${run.status}`}
      inspected={inspected}
      selected={selected}
      onInspect={onInspect}
      onSelect={onSelect}
      identity={
        <span className="flex min-w-0 flex-col gap-0.5">
          <span className="td-value text-2xs text-text-primary">
            {timing.kind === 'unparsed' ? timing.startedAt : formatUtc(timing.startedAt)}
          </span>
          <span className="truncate text-3xs text-text-muted">{run.run_id}</span>
        </span>
      }
    >
      <Cell>
        <span className="flex min-w-0 flex-col gap-0.5">
          <span className="td-value truncate text-2xs text-text-primary">{run.task_key ?? run.task}</span>
          <span className="truncate text-3xs text-text-muted">
            {run.trigger} · {run.backend}
            {run.model ? ` · ${run.model}` : ''}
          </span>
        </span>
      </Cell>
      <Cell numeric>
        <Duration timing={timing} />
      </Cell>
      <Cell>
        <span className="flex min-w-0 flex-col gap-0.5">
          <ToneWord tone={tone} word={run.status} />
          <span className="td-value text-3xs text-text-muted">
            {run.accepted_count} acc · {run.rejected_count} rej · {run.skipped_count} skip
          </span>
        </span>
      </Cell>
      <Cell numeric>
        {receipts === null ? (
          <Absent>receipt read blocked</Absent>
        ) : joined === undefined ? (
          <Absent>none in loaded page</Absent>
        ) : (
          <span>
            {joined.applied} applied
            {joined.quarantined > 0 ? ` · ${joined.quarantined} quarantined` : ''}
          </span>
        )}
      </Cell>
      <Cell numeric>
        {run.artifact_kinds.length === 0 ? <Absent>none recorded</Absent> : run.artifact_kinds.length}
      </Cell>
      <Cell>
        <IntegrityCell runId={run.run_id} artifactCount={run.artifact_kinds.length} />
      </Cell>
    </InspectRow>
  );
}

function Duration({ timing }: { timing: ReturnType<typeof runTiming> }) {
  switch (timing.kind) {
    case 'measured':
      return <>{formatDuration(timing.durationSecs)}</>;
    case 'open':
      return <Absent>not settled</Absent>;
    case 'inverted':
      return <Absent>stamps inverted</Absent>;
    case 'unparsed':
      return <Absent>stamps unparsed</Absent>;
    default: {
      const exhaustive: never = timing;
      return exhaustive;
    }
  }
}

/** The daemon's verdict, read from the artifacts query cache only. This cell
 * never issues the read itself: fifty eager chain verifications per page view
 * would be fifty ledger scans nobody asked for. Inspecting the run issues it,
 * and once cached the verdict appears here for as long as the cache holds. */
function IntegrityCell({ runId, artifactCount }: { runId: string; artifactCount: number }) {
  const artifacts = useAutomationRunArtifacts(runId, false);
  if (artifactCount === 0) return <Absent>no artifact to verify</Absent>;
  const result = artifacts.data;
  if (result === undefined) return <Absent>unchecked · inspect run</Absent>;
  if (result.outcome !== 'ok') return <Absent>verdict {result.outcome.replaceAll('_', ' ')}</Absent>;
  const status = result.data.artifact_chain.integrity_status;
  return <ToneWord tone={integrityTone(status)} word={status.replaceAll('_', ' ')} />;
}
