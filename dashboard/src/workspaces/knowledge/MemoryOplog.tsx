/**
 * MEMORY OPLOG — the store's own record of what changed.
 *
 * `/oplog` reads canonical lineage operations, newest first. Its rows account
 * for committed memory mutations; the separate automation run ledger records
 * invocations and their terminal outcomes rather than replacing store lineage.
 *
 * The route reports canonical lineage identity only: event sequence, time,
 * operation, and optional canonical fact ID. It does not expose mutation
 * detail.
 */
import { PayloadBoundary } from '../../ui/ReadSection.tsx';
import { Panel, Readout } from '../../ui/instrument.tsx';
import { useMemoryOplog, type OplogEvent, type OplogPayload } from '../../data/query/memory.ts';
import { formatUtcMicros, oplogReading } from './memoryModel.ts';

export function MemoryOplog() {
  const oplog = useMemoryOplog();
  return (
    <div className="flex h-full min-h-0 flex-col p-3">
      <Panel legend="Memory oplog" className="min-h-0 flex-1" bodyClassName="min-h-0 flex flex-col" elevation="well">
        <PayloadBoundary title="Memory oplog" pending={oplog.isPending} result={oplog.data}>
          {(data) => <OplogBody data={data} />}
        </PayloadBoundary>
      </Panel>
    </div>
  );
}

function OplogBody({ data }: { data: OplogPayload }) {
  const reading = oplogReading(data);
  // The handler answers HTTP 200 with an `error` string when the store cannot
  // be opened, so an unreadable store and a store with no operations arrive
  // identically in `events`. This is the only thing that separates them.
  if (reading.storeError !== null) {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        the memory oplog could not be read: {reading.storeError}
      </p>
    );
  }
  const tallyMatches = reading.events.length === data.count;
  if (reading.events.length === 0 && tallyMatches) {
    return (
      <p className="text-2xs leading-relaxed text-text-muted">
        the audit is readable and holds no operations — nothing has ever written to this
        memory store
      </p>
    );
  }
  return (
    <div className="flex min-h-0 flex-col gap-2">
      {!tallyMatches ? (
        <p role="status" className="text-2xs leading-relaxed text-state-partial">
          the store counted {data.count.toLocaleString()} operations but sent {reading.events.length.toLocaleString()}, so this list is incomplete
        </p>
      ) : reading.events.length >= data.limit ? (
        <p role="status" className="text-2xs leading-relaxed text-state-partial">
          this is the first {data.limit.toLocaleString()} operations, the request cap, so older operations may exist
        </p>
      ) : null}
      <div className="flex flex-wrap items-end gap-4">
        <Readout label="operations" size="sm" value={reading.events.length.toLocaleString()} />
      </div>
      <ul
        aria-label="Operations by kind"
        className="flex flex-wrap gap-x-3 gap-y-0.5 border-y border-edge-subtle py-1.5 text-3xs text-text-muted"
      >
        {reading.operations.map((row) => (
          <li key={row.op}>
            {row.op} · {row.count.toLocaleString()}
          </li>
        ))}
      </ul>
      <p className="text-3xs leading-relaxed text-text-muted">
        {reading.events.length.toLocaleString()} most recent operations returned, newest first
      </p>
      {/* The log is the one thing on this view that scrolls, and it holds no
        * focusable content of its own — so it takes the tab stop and carries
        * the accessible name on the node that actually scrolls (WCAG 2.1.1). */}
      <ol
        role="region"
        aria-label="Memory operations"
        tabIndex={0}
        className="flex min-h-0 flex-1 flex-col gap-1.5 overflow-auto"
      >
        {reading.events.map((event) => (
          <OplogRow key={String(event.id)} event={event} />
        ))}
      </ol>
    </div>
  );
}

function OplogRow({ event }: { event: OplogEvent }) {
  return (
    <li className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2">
      <p className="flex flex-wrap items-baseline gap-x-2 text-3xs text-text-muted">
        <span className="td-value" data-cell="numeric">
          {formatUtcMicros(event.ts)}
        </span>
        <span className="text-text-secondary">{event.op}</span>
        {/* `fact_id` is null only for an operation with no canonical fact target. */}
        <span className="td-value" data-cell="numeric">
          {event.fact_id == null ? 'no fact target' : event.fact_id}
        </span>
      </p>
    </li>
  );
}
