import { StateChip } from '../../ui/StateChip.tsx';
import { MeterRow } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { formatMoment } from '../loom/tracks.ts';
import type { CountRow } from './activity.ts';
import { failedEvents, readOutcomes, type AttemptFailureReading } from './failure.ts';

/**
 * Failure context: what went wrong, on both authorities that record it.
 *
 * The analytics fold accounts for how the window's events came out, and carries
 * a short tape of the latest ones. The work-product graph's runtime projection
 * accounts for attempts — the executions the daemon could observe, and the
 * state each is in.
 *
 * Three separations this surface exists to hold:
 *
 *   - A read that did not land is not a failure count of zero. The attempt
 *     panel keeps the daemon's refusal and prints no count at all.
 *   - Runtime coverage of `unavailable` is not zero failures. The daemon
 *     observing no attempt says nothing whatever about how attempts came out,
 *     and drawing a zero there would be the page answering a question the
 *     daemon declined.
 *   - An outcome word this build does not recognize is not a success. It is
 *     listed as unclassified, so a failure mode the daemon starts reporting
 *     under a new name shows up as an unread word rather than as a clean run.
 */
export function AgentFailureContext({
  outcomes,
  recentEvents,
  attempts,
}: {
  outcomes: readonly CountRow[];
  recentEvents: readonly CountRow[];
  attempts: AttemptFailureReading;
}) {
  const outcome = readOutcomes(outcomes);
  const tape = failedEvents(recentEvents);

  return (
    <div className="flex min-w-0 flex-col gap-3" data-agent-failure-context="read">
      <section aria-label="Event outcomes" className="flex min-w-0 flex-col gap-1.5">
        <h3 className="td-legend text-text-secondary">Outcomes in the window</h3>
        {outcome.counted === 0 ? (
          <p className="text-2xs leading-relaxed text-text-muted" data-agent-outcomes="none">
            The fold served no outcome accounting for this window, so how its events came out is
            unknown here.
          </p>
        ) : (
          <>
            <p className="text-xs leading-relaxed text-text-primary">
              <span className="td-value">{outcome.failedTotal.toLocaleString()}</span> of{' '}
              <span className="td-value">{outcome.counted.toLocaleString()}</span> accounted
              events came out as a failure
              {outcome.counted > 0
                ? ` — ${((outcome.failedTotal / outcome.counted) * 100).toFixed(outcome.failedTotal === 0 ? 0 : 2)}%`
                : ''}
              .
            </p>
            {outcome.failed.map((row) => (
              <MeterRow
                key={row.label}
                label={row.label}
                fraction={outcome.counted > 0 ? row.count / outcome.counted : null}
                value={row.count.toLocaleString()}
              />
            ))}
            {outcome.failed.length === 0 ? (
              <p className="text-2xs leading-relaxed text-text-muted">
                No outcome word in this window is one this build reads as a failure.
              </p>
            ) : null}
            {outcome.unclassified.length > 0 ? (
              <p
                className="text-2xs leading-relaxed text-state-conflicting"
                data-agent-outcomes-unclassified={outcome.unclassified.length}
              >
                {outcome.unclassifiedTotal.toLocaleString()} events came out as{' '}
                {outcome.unclassified.map((row) => row.label).join(', ')} — {' '}
                {outcome.unclassified.length === 1 ? 'a word' : 'words'} this build does not
                classify either way. {outcome.unclassified.length === 1 ? 'It is' : 'They are'}{' '}
                counted in the denominator above and in neither the failures nor the settled
                events, rather than being read as success.
              </p>
            ) : null}
            <p className="text-3xs leading-relaxed text-text-muted">
              Shares are of the {outcome.counted.toLocaleString()} events this accounting
              described, which is not necessarily the whole window — the fold counts the window
              and describes the outcomes separately.
            </p>
          </>
        )}
      </section>

      <section aria-label="Recent failures" className="flex min-w-0 flex-col gap-1.5">
        <h3 className="td-legend text-text-secondary">On the served tape</h3>
        {tape.served === 0 ? (
          <p className="text-2xs leading-relaxed text-text-muted" data-agent-failure-tape="none">
            The fold served no recent events, so there is no tape to read failures off. The
            outcome accounting above is unaffected by this — it is a separate measurement.
          </p>
        ) : tape.events.length === 0 ? (
          <p className="text-2xs leading-relaxed text-text-muted" data-agent-failure-tape="clean">
            None of the {tape.served} events on the served tape failed. This is a reading of
            those {tape.served}, not of the window: the tape is the latest few events and the
            failures counted above are mostly older than it.
          </p>
        ) : (
          <>
            <ol className="flex min-w-0 flex-col" data-agent-failure-tape={tape.events.length}>
              {tape.events.map((event, index) => (
                <li
                  key={`${event.timestamp}-${index}`}
                  className="flex items-baseline gap-2 border-b border-edge-subtle py-1 text-2xs last:border-b-0"
                  data-agent-failure-outcome={event.outcome}
                >
                  <span
                    className="td-value shrink-0 text-3xs text-text-muted"
                    data-cell="numeric"
                  >
                    {formatMoment(event.timestamp)}
                  </span>
                  <span
                    className="min-w-0 flex-1 truncate text-text-primary"
                    title={event.tool || event.kind}
                  >
                    {event.tool || event.kind || 'unnamed event'}
                  </span>
                  <span className="td-legend shrink-0 text-text-primary">{event.outcome}</span>
                </li>
              ))}
            </ol>
            <p className="text-3xs leading-relaxed text-text-muted">
              {tape.events.length} of the {tape.served} events the fold served came out as a
              failure. The tape carries no message, stack or argument — only the tool, the
              instant and the outcome — so nothing here explains why any of them failed.
            </p>
          </>
        )}
      </section>

      <AttemptFailures reading={attempts} />
    </div>
  );
}

/**
 * Attempts, from the runtime projection on the work-product graph read.
 *
 * The coverage reading is load-bearing and is checked before any count is
 * printed. `unavailable` means the daemon observed no attempt at all, which is
 * a statement about the observation and not about the attempts.
 */
function AttemptFailures({ reading }: { reading: AttemptFailureReading }) {
  if (reading.state === 'pending') {
    return (
      <section aria-label="Attempt failures" className="flex min-w-0 flex-col gap-1.5">
        <h3 className="td-legend text-text-secondary">Attempts</h3>
        <StateChip kind="loading" detail="reading the runtime projection" />
      </section>
    );
  }
  if (reading.state === 'refused') {
    return (
      <section
        aria-label="Attempt failures"
        className="flex min-w-0 flex-col gap-1.5"
        data-agent-attempt-failures="refused"
      >
        <h3 className="td-legend text-text-secondary">Attempts</h3>
        <StateChip kind={reading.chip} detail="attempt failures" />
        <p className="text-3xs leading-snug text-text-muted">{reading.detail}</p>
        <p className="text-3xs leading-snug text-text-muted">
          No attempt count is shown: the runtime projection was not read, so there is nothing to
          report as zero.
        </p>
      </section>
    );
  }
  if (reading.coverage === 'unavailable') {
    return (
      <section
        aria-label="Attempt failures"
        className="flex min-w-0 flex-col gap-1.5"
        data-agent-attempt-failures="unavailable"
      >
        <h3 className="td-legend text-text-secondary">Attempts</h3>
        <StateChip kind="unavailable" detail="runtime projection observed no attempt" />
        <p className="text-3xs leading-snug text-text-muted">
          The graph answered at version {reading.graphVersion} and its runtime projection
          reports coverage <span className="td-value">unavailable</span> — the daemon could
          observe no attempt. That is the daemon declining to say how attempts came out, so no
          failure count is drawn from it.
        </p>
      </section>
    );
  }

  const { failures, byState, attempts, coverage, unobserved, graphVersion } = reading;
  return (
    <section
      aria-label="Attempt failures"
      className="flex min-w-0 flex-col gap-1.5"
      data-agent-attempt-failures={failures.length}
    >
      <h3 className="td-legend text-text-secondary">Attempts</h3>
      <p className="text-xs leading-relaxed text-text-primary">
        <span className="td-value">{failures.length}</span> of{' '}
        <span className="td-value">{attempts}</span> observed{' '}
        {attempts === 1 ? 'attempt' : 'attempts'} on graph version {graphVersion}{' '}
        {failures.length === 1 ? 'is' : 'are'} in a state this build reads as not having come
        out clean.
      </p>
      {coverage === 'partial' ? (
        <p
          className="text-2xs leading-relaxed text-state-partial"
          data-agent-attempt-coverage="partial"
        >
          Coverage is partial: {unobserved} further{' '}
          {unobserved === 1 ? 'attempt was' : 'attempts were'} named by the daemon as
          unobservable and {unobserved === 1 ? 'is' : 'are'} absent from the count above, which
          is therefore a floor.
        </p>
      ) : null}
      {byState.map((row) => (
        <MeterRow
          key={row.label}
          label={row.label.replace(/_/g, ' ')}
          fraction={attempts > 0 ? row.count / attempts : null}
          value={row.count.toLocaleString()}
        />
      ))}
      {failures.length === 0 ? (
        <p className="text-2xs leading-relaxed text-text-muted">
          Every observed attempt on this version is running, leased or succeeded. With{' '}
          {coverage === 'complete' ? 'complete' : 'partial'} coverage this is a measurement of
          the attempts the daemon could see.
        </p>
      ) : (
        <ul className="flex min-w-0 flex-col">
          {failures.map((failure) => (
            <li
              key={failure.attemptId}
              className="flex items-baseline gap-2 border-b border-edge-subtle py-1 text-2xs last:border-b-0"
              data-agent-attempt-state={failure.state}
            >
              <span className="min-w-0 flex-1 truncate font-mono text-text-secondary">
                {failure.taskId} · {failure.runId}
              </span>
              <span
                className={cn(
                  'td-legend shrink-0',
                  failure.state === 'failed' || failure.state === 'timed_out'
                    ? 'text-text-primary'
                    : 'text-text-muted',
                )}
              >
                {failure.state.replace(/_/g, ' ')}
              </span>
            </li>
          ))}
        </ul>
      )}
      <p className="text-3xs leading-relaxed text-text-muted">
        The runtime projection carries an attempt identity and a state and nothing else — no
        message, no exit code, no instant — so this names which attempts did not come out clean
        and cannot say why.
      </p>
    </section>
  );
}
