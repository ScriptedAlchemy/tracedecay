import { ChevronLeft, ChevronRight, X } from 'lucide-react';
import { useEffect, useRef } from 'react';
import { useSearchParams } from 'react-router';

import { StateChip } from '../../ui/StateChip.tsx';
import {
  inspectorEntryKey,
  parseInspectorEntry,
  serializeInspectorEntry,
  useInspectorStack,
  type InspectorEntry,
} from './inspectorStack.ts';

const INSPECT_PARAM = 'inspect';
const ACTIVE_PARAM = 'inspectActive';

function sameEntries(left: readonly InspectorEntry[], right: readonly InspectorEntry[]): boolean {
  return (
    left.length === right.length &&
    left.every((entry, index) => inspectorEntryKey(entry) === inspectorEntryKey(right[index]!))
  );
}

/** Keeps the bounded inspector stack in repeated URL parameters. Each value is
 * a complete scope/entity/evidence identity, so changing order or removing one
 * entry never changes what any remaining entry names. */
export function InspectorUrlSync() {
  const [searchParams, setSearchParams] = useSearchParams();
  const entries = useInspectorStack((state) => state.entries);
  const activeKey = useInspectorStack((state) => state.activeKey);
  const replace = useInspectorStack((state) => state.replace);
  const applyingFromUrl = useRef(false);

  useEffect(() => {
    const fromUrl = searchParams
      .getAll(INSPECT_PARAM)
      .map(parseInspectorEntry)
      .filter((entry): entry is InspectorEntry => entry !== null);
    const current = useInspectorStack.getState();
    const requestedActive = searchParams.get(ACTIVE_PARAM);
    if (sameEntries(fromUrl, current.entries) && requestedActive === current.activeKey) return;
    applyingFromUrl.current = true;
    replace(fromUrl, requestedActive);
    applyingFromUrl.current = false;
  }, [replace, searchParams]);

  useEffect(() => {
    if (applyingFromUrl.current) return;
    const current = useInspectorStack.getState();
    if (entries !== current.entries || activeKey !== current.activeKey) return;
    const encoded = searchParams.getAll(INSPECT_PARAM);
    const nextEncoded = entries.map(serializeInspectorEntry);
    if (
      encoded.length === nextEncoded.length &&
      encoded.every((value, index) => value === nextEncoded[index]) &&
      searchParams.get(ACTIVE_PARAM) === activeKey
    ) {
      return;
    }
    const next = new URLSearchParams(searchParams);
    next.delete(INSPECT_PARAM);
    for (const entry of entries) next.append(INSPECT_PARAM, serializeInspectorEntry(entry));
    if (activeKey === null) next.delete(ACTIVE_PARAM);
    else next.set(ACTIVE_PARAM, activeKey);
    setSearchParams(next, { replace: true });
  }, [activeKey, entries, searchParams, setSearchParams]);

  return null;
}

function InspectorIdentity({ entry }: { entry: InspectorEntry }) {
  return (
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-2 text-2xs">
      <dt className="td-legend">Scope</dt>
      <dd className="font-mono text-text-secondary">
        {entry.scope.kind === 'all' ? 'all projects' : entry.scope.project_id}
      </dd>
      <dt className="td-legend">Entity</dt>
      <dd className="min-w-0">
        <span className="text-text-muted">{entry.entity.kind}</span>{' '}
        <span className="break-all font-mono text-text-primary">{entry.entity.id}</span>
      </dd>
      <dt className="td-legend">Evidence</dt>
      <dd className="min-w-0">
        {entry.evidence === undefined ? (
          <span className="text-text-muted">not supplied</span>
        ) : (
          <>
            <span className="text-text-muted">{entry.evidence.kind}</span>{' '}
            <span className="break-all font-mono text-text-primary">{entry.evidence.id}</span>
          </>
        )}
      </dd>
    </dl>
  );
}

/** Shell-owned inspector chrome. Workspace adapters register the actual entity
 * readers; until one is loaded, the shell keeps the exact identity visible and
 * reports content as unavailable instead of inventing an empty record. */
export function InspectorStack() {
  const entries = useInspectorStack((state) => state.entries);
  const activeKey = useInspectorStack((state) => state.activeKey);
  const activate = useInspectorStack((state) => state.activate);
  const close = useInspectorStack((state) => state.close);
  const move = useInspectorStack((state) => state.move);
  if (entries.length === 0) return null;

  const active = entries.find((entry) => inspectorEntryKey(entry) === activeKey) ?? entries.at(-1);
  if (active === undefined) return null;

  return (
    <aside
      aria-label="Inspector stack"
      className="z-20 flex w-96 shrink-0 flex-col border-l border-edge-subtle bg-surface-1 max-xl:w-80 max-md:absolute max-md:inset-0 max-md:w-full"
    >
      <div
        role="tablist"
        aria-label="Open inspectors"
        className="flex min-h-[var(--touch-target-min)] shrink-0 overflow-x-auto border-b border-edge-subtle"
      >
        {entries.map((entry, index) => {
          const key = inspectorEntryKey(entry);
          const selected = key === activeKey;
          return (
            <div
              key={key}
              role="presentation"
              className="flex shrink-0 items-stretch border-r border-edge-subtle"
            >
              <button
                type="button"
                role="tab"
                aria-selected={selected}
                onClick={() => activate(key)}
                className="min-w-24 px-2 text-left text-2xs hover:bg-surface-2"
              >
                <span className="td-legend block">{entry.entity.kind}</span>
                <span className="block max-w-32 truncate font-mono text-text-primary">
                  {entry.entity.id}
                </span>
              </button>
              <div className="flex items-center">
                <button
                  type="button"
                  aria-label={`Move ${entry.entity.id} earlier`}
                  disabled={index === 0}
                  onClick={() => move(key, -1)}
                  className="flex size-[var(--touch-target-min)] items-center justify-center text-text-muted enabled:hover:text-text-primary disabled:opacity-30"
                >
                  <ChevronLeft aria-hidden size={13} />
                </button>
                <button
                  type="button"
                  aria-label={`Move ${entry.entity.id} later`}
                  disabled={index === entries.length - 1}
                  onClick={() => move(key, 1)}
                  className="flex size-[var(--touch-target-min)] items-center justify-center text-text-muted enabled:hover:text-text-primary disabled:opacity-30"
                >
                  <ChevronRight aria-hidden size={13} />
                </button>
                <button
                  type="button"
                  aria-label={`Close ${entry.entity.id}`}
                  onClick={() => close(key)}
                  className="flex size-[var(--touch-target-min)] items-center justify-center text-text-muted hover:text-text-primary"
                >
                  <X aria-hidden size={13} />
                </button>
              </div>
            </div>
          );
        })}
      </div>
      <div role="tabpanel" className="min-h-0 flex-1 overflow-auto p-3">
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <StateChip kind="unavailable" detail="reader not loaded" />
          <span className="text-3xs text-text-muted">
            The identity remains linkable; no payload is inferred.
          </span>
        </div>
        <InspectorIdentity entry={active} />
      </div>
    </aside>
  );
}
