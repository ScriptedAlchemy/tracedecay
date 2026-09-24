import { useId, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import type { WorkflowDefinition } from '../../contracts/index.ts';
import { cn } from '../../ui/cn.ts';
import { Absence, GradeTag } from '../../ui/EvidenceGrade.tsx';
import { formatMicrosUtcClock } from '../../ui/format.ts';
import { Lamp, Panel } from '../../ui/instrument.tsx';
import { moveRovingFocus } from '../../ui/rovingFocus.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import type { WorkResult } from '../work/workApi.ts';
import {
  definitionKey,
  filterRegistry,
  groupRegistry,
  lifecycleStateTone,
  type LifecycleReceipt,
  type ReceiptLedger,
  type RegistryEntry,
} from './workflowLedger.ts';

/**
 * The registry column: every stable workflow identity the daemon serves, one
 * row each, with the immutable versions folded under it. Hover and focus
 * inspect a row (the panel below re-aims at it); click selects. Nothing here
 * changes lifecycle state, and no row claims a disposition the daemon has not
 * answered this session.
 */

const INPUT_CLASS =
  'min-h-[var(--touch-target-min)] w-full rounded-panel border border-edge-subtle bg-surface-1 px-2 font-mono text-2xs text-text-primary placeholder:text-text-muted focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent';

export function WorkflowRegistryPanel({
  result,
  pending,
  selectedId,
  inspectedId,
  receipts,
  onSelect,
  onInspect,
}: {
  result: WorkResult<WorkflowDefinition[]> | undefined;
  pending: boolean;
  selectedId: string | null;
  inspectedId: string | null;
  receipts: ReceiptLedger;
  onSelect: (definitionId: string) => void;
  onInspect: (definitionId: string | null) => void;
}) {
  const [query, setQuery] = useState('');
  const listRef = useRef<HTMLUListElement | null>(null);
  const searchId = useId();
  const entries = useMemo(
    () => (result?.outcome === 'value' ? groupRegistry(result.value) : []),
    [result],
  );
  const visible = useMemo(() => filterRegistry(entries, query), [entries, query]);
  const versionTotal = entries.reduce((sum, entry) => sum + entry.versions.length, 0);

  const onKeyDown = (event: KeyboardEvent) => {
    moveRovingFocus(listRef.current, event);
  };

  return (
    <Panel
      legend="Definition registry"
      elevation="well"
      bodyClassName="flex min-w-0 flex-col gap-2 p-2"
      footer={
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1 text-3xs text-text-muted">
          <span className="td-value text-3xs" data-testid="workflow-registry-count">
            {result?.outcome === 'value'
              ? `${entries.length} ${entries.length === 1 ? 'definition' : 'definitions'} · ${versionTotal} ${versionTotal === 1 ? 'version' : 'versions'}${query.trim() === '' ? '' : ` · ${visible.length} shown`}`
              : 'registry unread'}
          </span>
          <span className="min-w-0">
            disposition <span className="text-text-secondary">unread</span> until a compare-and-swap
            answers it this session
          </span>
        </div>
      }
    >
      <label className="flex min-w-0 flex-col gap-0.5 text-3xs text-text-muted" htmlFor={searchId}>
        <span className="td-legend">Filter by identity</span>
        <input
          id={searchId}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="workflow identity substring"
          className={INPUT_CLASS}
          disabled={result?.outcome !== 'value' || entries.length === 0}
        />
      </label>

      {pending ? (
        <StateChip kind="loading" detail="reading registered workflow definitions" />
      ) : result === undefined ? (
        <StateChip kind="unknown" detail="the definitions read returned no result" />
      ) : result.outcome === 'refused' ? (
        // A refused registry and an empty registry must never render alike.
        <StateChip kind={result.state} detail={result.detail} />
      ) : entries.length === 0 ? (
        <StateChip
          kind="complete_zero_findings"
          detail="the daemon answered: no workflow definitions are registered in this scope"
        />
      ) : visible.length === 0 ? (
        <p className="text-3xs text-text-muted" data-testid="workflow-registry-no-match">
          No registered identity contains "{query.trim()}". {entries.length} remain registered.
        </p>
      ) : (
        <div className="min-w-0">
          <div
            aria-hidden
            className="flex items-baseline justify-between gap-2 border-b border-edge-subtle px-2 pb-1 text-3xs"
          >
            <span className="td-legend min-w-0 truncate">identity · latest · versions · steps</span>
            <span className="td-legend shrink-0">disposition</span>
          </div>
          <ul
            ref={listRef}
            onKeyDown={onKeyDown}
            onMouseLeave={() => onInspect(null)}
            onBlur={(event) => {
              // Focus moving to another row keeps inspecting; leaving the list ends it.
              if (!listRef.current?.contains(event.relatedTarget)) onInspect(null);
            }}
            className="flex min-w-0 flex-col"
            data-workflow-definitions={entries.length}
            data-workflow-versions={versionTotal}
            aria-label="Registered workflow definitions"
          >
            {visible.map((entry) => (
              <RegistryRow
                key={entry.definitionId}
                entry={entry}
                selected={entry.definitionId === selectedId}
                inspected={entry.definitionId === inspectedId}
                receipt={
                  receipts.get(
                    definitionKey(entry.definitionId, entry.latest.definition_version),
                  ) ?? null
                }
                onSelect={() => onSelect(entry.definitionId)}
                onInspect={() => onInspect(entry.definitionId)}
              />
            ))}
          </ul>
        </div>
      )}
    </Panel>
  );
}

function RegistryRow({
  entry,
  selected,
  inspected,
  receipt,
  onSelect,
  onInspect,
}: {
  entry: RegistryEntry;
  selected: boolean;
  inspected: boolean;
  receipt: LifecycleReceipt | null;
  onSelect: () => void;
  onInspect: () => void;
}) {
  return (
    <li className="min-w-0 border-b border-edge-subtle last:border-b-0">
      <button
        type="button"
        onClick={onSelect}
        onMouseEnter={onInspect}
        onFocus={onInspect}
        aria-pressed={selected}
        data-workflow-registry-row={entry.definitionId}
        data-inspected={inspected || undefined}
        className={cn(
          'relative flex min-h-[44px] w-full min-w-0 flex-col gap-0.5 px-2 py-1.5 text-left',
          // Selection is the cyan gutter; inspection raises the face. Neither
          // is colour alone: `aria-pressed` and `data-inspected` carry both.
          selected ? 'bg-surface-2' : inspected ? 'td-raised' : 'hover:bg-surface-3',
        )}
      >
        {selected ? (
          <span aria-hidden className="absolute inset-y-0 left-0 w-[3px] bg-accent" />
        ) : null}
        <span className="flex min-w-0 items-center justify-between gap-2">
          <span
            className="td-value min-w-0 truncate text-2xs text-text-primary"
            title={entry.definitionId}
          >
            {entry.definitionId}
          </span>
          <DispositionCell receipt={receipt} className="shrink-0" />
        </span>
        <span className="td-value flex min-w-0 flex-wrap gap-x-2 text-3xs text-text-muted">
          <span data-cell="numeric">v{entry.latest.definition_version}</span>
          <span data-cell="numeric">
            {entry.versions.length} {entry.versions.length === 1 ? 'version' : 'versions'}
          </span>
          <span data-cell="numeric">
            {entry.latest.steps.length} {entry.latest.steps.length === 1 ? 'step' : 'steps'}
          </span>
        </span>
      </button>
    </li>
  );
}

/** The disposition column: the daemon's own answer when it gave one this
 * session, otherwise `unread`, dotted, muted, and spelled out, because the
 * registry serves no disposition and a blank cell would read as "none". */
export function DispositionCell({
  receipt,
  className,
}: {
  receipt: LifecycleReceipt | null;
  className?: string;
}) {
  if (receipt === null) {
    return (
      <span
        className={cn(
          'inline-flex w-fit max-w-full items-center gap-1 border border-dotted border-edge-subtle px-1 text-3xs uppercase tracking-[0.1em] text-text-muted',
          className,
        )}
        data-disposition="unread"
      >
        unread
      </span>
    );
  }
  return (
    <span
      className={cn('inline-flex min-w-0 items-center gap-1.5 text-3xs', className)}
      data-disposition={receipt.disposition.state}
      title={`answered ${new Date(receipt.answeredAtMillis).toISOString()} · revision ${receipt.disposition.revision}`}
    >
      <Lamp tone={lifecycleStateTone(receipt.disposition.state)} />
      <span className="uppercase tracking-[0.1em] text-text-secondary">
        {receipt.disposition.state}
      </span>
      <span className="td-value text-3xs text-text-muted" data-cell="numeric">
        r{receipt.disposition.revision}
      </span>
    </span>
  );
}

/** The inspect bay under the registry. Hover or focus re-aims it without
 * touching selection; with nothing under the pointer it shows the selection;
 * with nothing selected it says how to use it. */
export function RegistryInspectPanel({
  entry,
  mode,
  receipts,
}: {
  entry: RegistryEntry | null;
  mode: 'inspecting' | 'selected' | 'idle';
  receipts: ReceiptLedger;
}) {
  return (
    <Panel
      legend="Registry inspect"
      actions={
        <span className="td-legend shrink-0 text-text-muted" data-testid="workflow-inspect-mode">
          {mode === 'inspecting' ? 'hover · inspect only' : mode === 'selected' ? 'selection' : 'idle'}
        </span>
      }
      bodyClassName="p-2.5"
    >
      {entry === null ? (
        <p className="text-3xs text-text-muted">
          Hover or focus a registry row to inspect it here. Click or press Enter to select; inspection
          never changes the selection.
        </p>
      ) : (
        <InspectBody entry={entry} receipts={receipts} />
      )}
    </Panel>
  );
}

function InspectBody({ entry, receipts }: { entry: RegistryEntry; receipts: ReceiptLedger }) {
  const latest = entry.latest;
  const receipt = receipts.get(definitionKey(entry.definitionId, latest.definition_version)) ?? null;
  return (
    <div className="flex min-w-0 flex-col gap-2" data-workflow-inspect={entry.definitionId}>
      <div className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <span className="td-value min-w-0 break-all text-xs text-text-primary">
          {entry.definitionId}
        </span>
        <GradeTag grade="EXACT" source="registry" />
      </div>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-3xs">
        <InspectFact label="latest version" value={`v${latest.definition_version}`} />
        <InspectFact label="versions served" value={String(entry.versions.length)} />
        <InspectFact label="steps (latest)" value={String(latest.steps.length)} />
        {entry.projectAgreement === 'agree' ? (
          <InspectFact label="project" value={latest.project_id} />
        ) : (
          <div className="col-span-2 flex min-w-0 flex-col gap-0.5">
            <dt className="td-legend">project</dt>
            <dd className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5 text-text-secondary">
              <GradeTag grade="AMBIGUOUS" source="registry" />
              <span className="min-w-0">
                the served versions name different projects:{' '}
                {[...new Set(entry.versions.map((version) => version.project_id))].join(', ')}
              </span>
            </dd>
          </div>
        )}
      </dl>
      <div className="flex flex-col gap-1 border-t border-edge-subtle pt-2">
        <span className="td-legend">pinned by v{latest.definition_version}</span>
        <DigestLine label="policy" digest={latest.pinned_policy_digest} />
        <DigestLine label="configuration" digest={latest.pinned_configuration_digest} />
        <DigestLine label="catalog" digest={latest.pinned_catalog_digest} />
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 border-t border-edge-subtle pt-2 text-3xs">
        <span className="td-legend">disposition</span>
        <DispositionCell receipt={receipt} />
        {receipt === null ? (
          <span className="min-w-0 text-text-muted">
            no read route serves it; a compare-and-swap answer records it
          </span>
        ) : (
          <span className="min-w-0 text-text-muted">
            transitioned {formatMicrosUtcClock(receipt.disposition.transitioned_at)} UTC
          </span>
        )}
      </div>
      <Absence
        field="updated"
        reason="the definition contract carries no timestamps"
        className="border-t border-edge-subtle pt-2"
      />
    </div>
  );
}

function InspectFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend">{label}</dt>
      <dd className="td-value min-w-0 truncate text-3xs text-text-secondary" title={value}>
        {value}
      </dd>
    </div>
  );
}

/** A digest, whole. Truncated in the row, never in the record: `title`
 * carries the full string and the cell wraps at 200% zoom instead of clipping. */
export function DigestLine({ label, digest }: { label: string; digest: string }) {
  return (
    <div className="flex min-w-0 items-baseline gap-2 text-3xs">
      <span className="w-20 shrink-0 uppercase tracking-[0.08em] text-text-muted">{label}</span>
      <span className="td-value min-w-0 break-all text-3xs text-text-secondary" title={digest}>
        {digest}
      </span>
    </div>
  );
}
