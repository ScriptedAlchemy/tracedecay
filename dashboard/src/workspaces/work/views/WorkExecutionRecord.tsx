import type { WorkAttemptListCoverageV1 } from '../../../contracts/index.ts';
import { StateChip, type DomainStateKind } from '../../../ui/StateChip.tsx';
import { Panel } from '../../../ui/instrument.tsx';
import { cn } from '../../../ui/cn.ts';
import { kindColorVars } from '../../../viz/graph/kindColor.ts';
import type {
  WorkAttemptLineage,
  WorkAttemptLink,
  WorkAttemptReading,
  WorkCancellationLadder,
  WorkExecutorReading,
} from '../workAttemptModel.ts';
import type { WorkWeaveReading } from '../workViewsModel.ts';
import { ChannelAbsence, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * The execution record under the weave: who ran, how often they had to, and
 * what the cancellation ladder did.
 *
 * These three readings used to be the timeline's absences. They are drawn here
 * rather than folded into the weave itself because they answer a different
 * question about the same page — the weave says which runs touched which tasks,
 * and this says what actually executed — and because their coverage is its own:
 * the weave is drawn over the snapshot's page and this over the attempt list's,
 * two reads with two independent caps. Merging them would produce one caption
 * that could not honestly describe either.
 *
 * A capped page is stated at the top and repeated on the counts it makes into
 * floors. Nothing here is totalled across pages, because this build asks for
 * one page.
 */

function coverageSentence(coverage: WorkAttemptListCoverageV1): string {
  switch (coverage.coverage) {
    case 'complete':
      return `${coverage.returned} ${coverage.returned === 1 ? 'attempt' : 'attempts'} · the whole authorized set`;
    case 'capped':
      return `${coverage.returned} of ${coverage.returned + coverage.remaining} attempts · ${coverage.remaining} beyond this page`;
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

/** The attempt read's own state, as the panel's chip. */
function attemptStatus(reading: WorkAttemptReading): { kind: DomainStateKind; detail: string } {
  switch (reading.state) {
    case 'pending':
      return { kind: 'loading', detail: 'reading attempts' };
    case 'refused':
      return { kind: reading.chip, detail: reading.detail };
    case 'absent':
      return { kind: 'denied', detail: 'no attempts in scope, or not authorized' };
    case 'listed':
      // The figures belong to the caption; the chip says which kind of reading
      // the caption is, so the two are not the same sentence twice.
      return reading.page.partial
        ? { kind: 'partial', detail: 'one page of a larger set' }
        : { kind: 'ready', detail: 'the whole authorized set' };
    default: {
      const unhandled: never = reading;
      return unhandled;
    }
  }
}

export function WorkExecutionRecord({
  reading,
  selected,
  onSelect,
}: {
  reading: WorkWeaveReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const attempts = reading.attempts;
  const status = attemptStatus(attempts);
  const page = attempts.state === 'listed' ? attempts.page : null;

  return (
    <Panel
      legend="Execution record"
      actions={<StateChip kind={status.kind} detail={status.detail} />}
      elevation="well"
    >
      <div className="flex min-w-0 flex-col gap-3" data-work-execution-record={attempts.state}>
        {page === null ? (
          // Not an empty panel: the reason the record is missing is the reading,
          // and it is the same sentence every channel below would have carried.
          <ChannelAbsence
            measure="the execution record"
            channel={reading.executorIdentity}
          />
        ) : (
          <>
            <ViewCaption
              population={coverageSentence(page.coverage)}
              note={`topology generation ${page.topology.generation} · ${page.topology.task_count} ${page.topology.task_count === 1 ? 'task' : 'tasks'}`}
            />
            {page.partial ? (
              <p
                className="text-3xs leading-snug text-state-partial"
                data-work-attempt-coverage="capped"
              >
                This page was capped by the daemon, so every count below is a floor and not a
                total. The remaining attempts are not summarised here: a figure assembled from
                a page the daemon did not return would be this build's arithmetic rather than
                its reading.
              </p>
            ) : null}

            <Executors channel={reading.executorIdentity} partial={page.partial} />
            <ObservedOrder channel={reading.observedOrder} />
            <Ladder channel={reading.cancellationLadder} attempts={page.attemptCount} />
            <RetryWeave
              channel={reading.retryWeave}
              partial={page.partial}
              selected={selected}
              onSelect={onSelect}
            />
          </>
        )}
      </div>
    </Panel>
  );
}

/**
 * Who ran the attempts.
 *
 * Rows are keyed by the route that actually ran. `diverted` and `unobserved`
 * are printed rather than folded into the total, because a row that quietly
 * mixed observed execution with the request that asked for it would be exactly
 * the invented attribution the timeline spent its first version refusing to
 * draw.
 */
function Executors({
  channel,
  partial,
}: {
  channel: WorkWeaveReading['executorIdentity'];
  partial: boolean;
}) {
  if (!channel.available) {
    return <ChannelAbsence measure="executor identity" channel={channel} />;
  }
  const ceiling = Math.max(1, ...channel.value.map((row) => row.attempts));
  return (
    <section className="flex min-w-0 flex-col gap-1.5" aria-label="Executors">
      <h3 className="td-legend text-text-secondary">Executors</h3>
      <ul className="flex min-w-0 flex-col gap-1" data-work-executors={channel.value.length}>
        {channel.value.map((row) => (
          <li key={`${row.providerId}/${row.routeId}`} className="min-w-0">
            <ExecutorRow row={row} ceiling={ceiling} partial={partial} />
          </li>
        ))}
      </ul>
    </section>
  );
}

function ExecutorRow({
  row,
  ceiling,
  partial,
}: {
  row: WorkExecutorReading;
  ceiling: number;
  partial: boolean;
}) {
  const notes: string[] = [];
  if (row.diverted > 0) notes.push(`${row.diverted} diverted here from another route`);
  if (row.unobserved > 0) notes.push(`${row.unobserved} attributed by request, not observed`);
  return (
    <div
      className="flex min-w-0 flex-col gap-1 border border-edge-subtle bg-surface-1 p-2"
      data-work-executor={`${row.providerId}/${row.routeId}`}
      data-work-executor-attempts={row.attempts}
      data-work-executor-diverted={row.diverted > 0 ? row.diverted : undefined}
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <span
          aria-hidden
          style={kindColorVars(row.providerId)}
          className="h-3 w-1 shrink-0 bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
        />
        <span className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary">
          {row.providerId} · {row.routeId}
        </span>
        <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {row.attempts} {row.attempts === 1 ? 'attempt' : 'attempts'}
          {partial ? ' or more' : ''}
        </span>
      </div>
      <div
        aria-hidden
        className="h-1 w-full bg-surface-3"
        // The count again as a length, so routes rank without reading digits.
        // Hidden: the figure is printed beside it.
      >
        <span
          className="block h-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
          style={{ ...kindColorVars(row.providerId), width: `${(row.attempts / ceiling) * 100}%` }}
        />
      </div>
      {notes.length > 0 ? (
        <p className="text-3xs leading-snug text-text-muted">{notes.join(' · ')}</p>
      ) : null}
    </div>
  );
}

/**
 * The order attempts were observed to finish in.
 *
 * A sequence and deliberately not an axis. `terminal.observed_at` is the one
 * real instant on the page, so the attempts that reached a terminal can be put
 * in the order they reached it — and that is the whole of what this build can
 * say about time here. Spacing the marks by the gaps between those instants
 * would draw durations out of end points, so the caption says the spacing means
 * nothing and the weave above carries the wall-clock absence itself.
 */
function ObservedOrder({ channel }: { channel: WorkWeaveReading['observedOrder'] }) {
  return (
    <section className="flex min-w-0 flex-col gap-1.5" aria-label="Observed terminal order">
      <h3 className="td-legend text-text-secondary">Observed terminal order</h3>
      {channel.available ? (
        <>
          <ol
            className="flex min-w-0 flex-wrap items-center gap-1"
            data-work-terminal-order={channel.value.length}
          >
            {channel.value.map((observation, index) => (
              <li
                key={observation.attemptId}
                className="flex min-w-0 items-center gap-1 border border-edge-subtle px-1 py-px text-3xs"
                data-work-terminal-outcome={observation.outcome}
              >
                <span className="td-value text-text-muted" data-cell="numeric">
                  {index + 1}
                </span>
                <span className="min-w-0 truncate font-mono text-text-secondary">
                  {observation.taskId}
                </span>
                <span className="text-text-primary">{observation.outcome}</span>
              </li>
            ))}
          </ol>
          <p className="text-3xs leading-snug text-text-muted">
            Rank, not position in time: the marks are evenly spaced because the gaps between
            them are not drawn. Attempts still running hold no place in this order, having
            reached no terminal to be observed at.
          </p>
        </>
      ) : (
        <ChannelAbsence measure="a terminal instant" channel={channel} />
      )}
    </section>
  );
}

/**
 * The cancellation ladder.
 *
 * Every rung is printed including the empty ones, because "nothing reached this
 * rung" is a reading about the page and a rung that vanished when it hit zero
 * would make the ladder's shape depend on its own values. `unrecorded` is the
 * disagreement row: an attempt whose recorded state claims a cancellation its
 * cancellation record does not carry. Neither side is preferred.
 */
function Ladder({
  channel,
  attempts,
}: {
  channel: WorkWeaveReading['cancellationLadder'];
  attempts: number;
}) {
  if (!channel.available) {
    return <ChannelAbsence measure="the cancellation ladder" channel={channel} />;
  }
  const ladder: WorkCancellationLadder = channel.value;
  const rungs = [
    { rung: 'requested', count: ladder.requested },
    { rung: 'acknowledged', count: ladder.acknowledged },
    { rung: 'escalated', count: ladder.escalated },
  ] as const;
  const reached = ladder.requested + ladder.acknowledged + ladder.escalated;

  return (
    <section className="flex min-w-0 flex-col gap-1.5" aria-label="Cancellation ladder">
      <h3 className="td-legend text-text-secondary">Cancellation ladder</h3>
      <ul
        className="flex min-w-0 flex-wrap gap-1"
        data-work-ladder-reached={reached}
      >
        {rungs.map((entry) => (
          <li
            key={entry.rung}
            className={cn(
              'flex min-w-0 flex-col gap-0.5 border px-2 py-1',
              entry.count > 0 ? 'border-edge-strong' : 'border-dashed border-edge-subtle',
            )}
            data-work-ladder-rung={entry.rung}
            data-work-ladder-count={entry.count}
          >
            <span className="td-value text-2xs text-text-primary" data-cell="numeric">
              {entry.count}
            </span>
            <span className="text-3xs text-text-muted">{entry.rung}</span>
          </li>
        ))}
      </ul>
      <p className="text-3xs leading-snug text-text-muted">
        {reached === 0
          ? `No attempt on this page carries a cancellation record; all ${attempts} sit at rung zero.`
          : `${reached} of ${attempts} attempts entered the ladder. A rung counts the furthest point an attempt reached, not a running total.`}
      </p>
      {ladder.unrecorded > 0 ? (
        <p className="text-3xs leading-snug text-state-conflicting" data-work-ladder-unrecorded={ladder.unrecorded}>
          {ladder.unrecorded} {ladder.unrecorded === 1 ? 'attempt reports' : 'attempts report'} a
          cancellation state while carrying no cancellation record. The two disagree; this build
          shows both rather than choosing one.
        </p>
      ) : null}
    </section>
  );
}

/**
 * The retry chains.
 *
 * A restart here is a link followed through `recovery.source_attempt_id`, which
 * is what makes it a measurement rather than the weave's inference from a
 * second evidence row. A chain whose root sits on an earlier page says so:
 * `truncated` means the count beside it is a floor.
 */
function RetryWeave({
  channel,
  partial,
  selected,
  onSelect,
}: {
  channel: WorkWeaveReading['retryWeave'];
  partial: boolean;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  if (!channel.available) {
    return <ChannelAbsence measure="the attempt chain per task" channel={channel} />;
  }
  const retried = channel.value.filter((lineage) => lineage.restarts > 0);
  return (
    <section className="flex min-w-0 flex-col gap-1.5" aria-label="Attempt chains">
      <h3 className="td-legend text-text-secondary">Attempt chains</h3>
      {retried.length === 0 ? (
        <EmptyReading>
          Every chain on this page is a single attempt: nothing was restarted, resumed, or left
          needing recovery{partial ? ' among the attempts this page returned' : ''}.
        </EmptyReading>
      ) : (
        <p className="text-3xs leading-snug text-text-muted">
          {retried.length} of {channel.value.length}{' '}
          {channel.value.length === 1 ? 'chain' : 'chains'} took more than one attempt.
        </p>
      )}
      <ul className="flex min-w-0 flex-col gap-1" data-work-lineages={channel.value.length}>
        {channel.value.map((lineage) => (
          <li key={`${lineage.taskId}/${lineage.runId}`} className="min-w-0">
            <Lineage
              lineage={lineage}
              selected={selected === lineage.taskId}
              onSelect={onSelect}
            />
          </li>
        ))}
      </ul>
    </section>
  );
}

function Lineage({
  lineage,
  selected,
  onSelect,
}: {
  lineage: WorkAttemptLineage;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  const chain = `${lineage.links.length} ${lineage.links.length === 1 ? 'attempt' : 'attempts'}`;
  const restarts =
    lineage.restarts === 0
      ? 'first attempt only'
      : `${lineage.restarts} ${lineage.restarts === 1 ? 'restart' : 'restarts'}${lineage.truncated ? ' or more' : ''}`;
  return (
    <button
      type="button"
      onClick={() => onSelect(lineage.taskId)}
      aria-pressed={selected}
      aria-label={`Task ${lineage.taskId} on run ${lineage.runId}: ${chain}, ${restarts}, ${lineage.open ? 'still open' : 'terminated'}${lineage.truncated ? ', chain begins before this page' : ''}`}
      // 44px explicitly: this app's root font size is 14px, so `min-h-11`
      // computes under the target size the accessibility gate measures.
      className={cn(
        'relative flex min-h-[44px] w-full min-w-0 flex-col justify-center gap-1',
        'border bg-transparent px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        selected ? 'border-accent' : 'border-edge-subtle',
        'hover:border-edge-strong',
      )}
      data-work-task={lineage.taskId}
      data-work-lineage={`${lineage.taskId}/${lineage.runId}`}
      data-work-restarts={lineage.restarts}
      data-work-lineage-truncated={lineage.truncated ? 'true' : undefined}
    >
      {selected ? <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" /> : null}
      <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <span className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary">
          {lineage.taskId} · {lineage.runId}
        </span>
        <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {restarts}
        </span>
      </span>
      <span aria-hidden className="flex min-w-0 flex-wrap items-center gap-1">
        {lineage.links.map((link) => (
          <LinkMark key={link.attemptId} link={link} />
        ))}
      </span>
      {lineage.truncated ? (
        <span className="truncate text-3xs text-text-muted">
          chain begins before this page
        </span>
      ) : null}
    </button>
  );
}

/** One attempt in a chain. Terminated attempts carry their outcome; an open one
 * is drawn dashed, because it has no outcome to draw rather than an outcome
 * that is pending. */
function LinkMark({ link }: { link: WorkAttemptLink }) {
  const outcome = link.outcome;
  return (
    <span
      className={cn(
        'flex min-w-0 items-center gap-1 border px-1 py-px text-3xs',
        outcome === null ? 'border-dashed border-edge-subtle' : 'border-edge-strong',
      )}
      data-work-link={link.attemptId}
      data-work-link-origin={link.origin}
      data-work-link-outcome={outcome?.outcome}
    >
      <span className="text-text-muted">{link.origin}</span>
      <span className="text-text-primary">{outcome === null ? link.state : outcome.outcome}</span>
      {link.reason === null ? null : <span className="text-text-muted">{link.reason}</span>}
    </span>
  );
}
