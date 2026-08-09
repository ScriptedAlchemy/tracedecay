/**
 * MEMORY OPLOG — the store's own record of what changed.
 *
 * `/oplog` reads the authoritative compatibility audit, newest first. Its rows
 * are the only place a memory mutation is accounted for after the fact: the
 * curation activity stream is in-process and dies with the daemon, and the run
 * ledger records automation invocations rather than the store operations they
 * caused.
 *
 * The detail column is why this view exists as its own surface rather than a
 * list of strings. `memory_api::oplog` emits three DIFFERENT objects per row —
 * `{summary}`, `{redacted: true}`, `{availability: "unknown"}` — and they are
 * three distinct domain states, not one optional string. A redacted row has a
 * detail this reader may not see; an unknown row never recorded whether it had
 * one. Rendering both as an empty cell would claim the second is the first, and
 * claim of the first that nothing was ever written. Both keep their chip.
 */
import { PayloadBoundary } from '../../ui/ReadSection.tsx';
import { Panel, Readout } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { useMemoryOplog, type OplogEvent, type OplogPayload } from '../../data/query/memory.ts';
import { oplogDetailReading, oplogReading } from './memoryModel.ts';

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
  if (reading.events.length === 0) {
    return (
      <p className="text-2xs leading-relaxed text-text-muted">
        the audit is readable and holds no operations — nothing has ever written to this
        memory store
      </p>
    );
  }
  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex flex-wrap items-end gap-4">
        <Readout label="operations" size="sm" value={reading.events.length.toLocaleString()} />
        <Readout label="detail withheld" size="sm" value={reading.redacted.toLocaleString()} />
        <Readout
          label="detail unrecorded"
          size="sm"
          value={reading.unknownDetail.toLocaleString()}
        />
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
        the {reading.events.length.toLocaleString()} most recent of the last{' '}
        {data.limit.toLocaleString()} operations this store will report, newest first
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
  const detail = oplogDetailReading(event.detail);
  return (
    <li className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2">
      <p className="flex flex-wrap items-baseline gap-x-2 text-3xs text-text-muted">
        <span className="td-value" data-cell="numeric">
          {event.ts}
        </span>
        <span className="text-text-secondary">{event.op}</span>
        {/* `fact_id` is null for an operation with no legacy-addressable fact,
          * which is a real reading rather than a missing one. */}
        <span className="td-value" data-cell="numeric">
          {event.fact_id == null ? 'no fact target' : `fact #${event.fact_id}`}
        </span>
      </p>
      {detail.kind === 'summary' ? (
        <p className="text-2xs leading-relaxed text-text-secondary">{detail.summary}</p>
      ) : (
        <StateChip kind={detail.state} detail={detail.sentence} />
      )}
    </li>
  );
}
