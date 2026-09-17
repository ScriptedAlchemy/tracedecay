import { useCallback, useSyncExternalStore, type ReactNode } from 'react';
import type { LucideIcon } from 'lucide-react';
import { FolderTree, HardDrive, Link2, Rss, Search } from 'lucide-react';
import { useQueryClient } from '@tanstack/react-query';
import type { ProjectsPayloadV1 } from '../../contracts/generated.ts';
import {
  type ProjectRegistryResult,
  useProjectRegistry,
} from '../../data/query/projectRegistry.ts';
import {
  useEventStreamState,
  useProjectionSync,
  type ProjectionSync,
} from '../../data/sse/useEvents.tsx';
import { cn } from '../../ui/cn';
import { STATE_LAMP } from '../../ui/StateChip.tsx';
import {
  cancelQueryActivity,
  useActiveQueryActivities,
  useQueryCancellation,
} from '../../data/query/activity.ts';
import { useStatusRegisters } from '../../data/shell/statusRegisters.ts';

/**
 * What the transport is doing, and separately whether the data behind it has
 * been reconciled.
 *
 * These are two facts and the strip reports them as two, because a healthy
 * socket says nothing about whether the projection is current: a canonical
 * refresh can reject and leave the client knowingly behind while the link stays
 * `live`. Reading `live` alone in that state would be the strip claiming
 * freshness it has no evidence for.
 */
function feedReading(sync: ProjectionSync): {
  value: string;
  tone: string;
  detail: string | null;
} {
  switch (sync.kind) {
    case 'synced':
      return { value: 'synced', tone: 'bg-state-ready', detail: null };
    case 'resyncing':
      return { value: 'resyncing', tone: 'bg-state-loading', detail: null };
    case 'stale':
      return {
        value: 'stale',
        tone: 'bg-state-stale',
        detail: sync.reason === null ? 'a refresh is owed' : `a refresh is owed: ${sync.reason}`,
      };
    case 'failed':
      return {
        value: 'resync failed',
        tone: 'bg-state-error',
        detail: `the projection is behind and the refresh rejected: ${sync.reason}`,
      };
    case 'unmounted':
      return { value: 'no stream', tone: 'bg-state-offline', detail: null };
    default: {
      const unhandled: never = sync;
      return unhandled;
    }
  }
}

/**
 * The bottom status strip (NAVIGATION.md "Persistent regions" 6): a 32px
 * strip separating Link, Feed, Source, Query and Registry authority. Each is
 * its own labelled cell with its own reading; none vouches for another.
 * `Link live` says the `/api/events` socket is open — not that data is
 * synced, not that any activity was accepted, not that anything is healthy.
 */
export function StatusStrip({ queryActivity }: { queryActivity?: ReactNode } = {}) {
  const { state } = useEventStreamState();
  const feed = feedReading(useProjectionSync());
  const link =
    state === 'live'
      ? { value: 'live', tone: 'bg-state-ready', ink: 'text-accent', live: true }
      : state === 'connecting'
        ? { value: 'connecting', tone: 'bg-state-loading', ink: 'text-text-primary', live: true }
        : { value: 'down', tone: 'bg-state-offline', ink: 'text-text-muted', live: false };
  return (
    <footer
      className="flex min-h-[var(--shell-status)] shrink-0 items-stretch border-t border-edge-frame bg-surface-1"
      aria-label="Status"
    >
      <Cell icon={Link2} label="Link">
        <span
          aria-hidden
          className={cn('size-1.5 shrink-0', link.tone, link.live && 'td-signal')}
        />
        <span className={cn('td-value text-2xs uppercase', link.ink)} role="status">
          {link.value}
        </span>
      </Cell>
      <Cell icon={Rss} label="Feed">
        <span aria-hidden className={cn('size-1.5 shrink-0', feed.tone)} />
        {/*
         * One region over the state and its reason together.
         *
         * They used to be two elements with the live region around the word
         * alone, so a reader listening to the strip was told "stale" and never
         * told why — the sentence that says a refresh was rejected sat outside
         * the announcement, updating silently beside it. Both change at the
         * same moment and only mean anything together, so they are announced
         * together.
         */}
        <span
          role="status"
          className="flex min-w-0 items-center gap-1.5"
          data-feed-state={feed.value}
        >
          <span className="td-value text-2xs uppercase">{feed.value}</span>
          {feed.detail !== null && (
            // The state is carried by the word, not the swatch; the reason is
            // the one thing a reader needs to know that the word cannot hold.
            <span className="td-value min-w-0 truncate text-3xs normal-case text-text-muted">
              {feed.detail}
            </span>
          )}
        </span>
      </Cell>
      {queryActivity}
      <WorkspaceRegisters />
      <span aria-hidden className="flex-1 border-r border-edge-subtle" />
    </footer>
  );
}

/**
 * The registers the mounted workspace publishes about the authorities it is
 * reading — the Code workspace's graph read, index freshness and pinned
 * selection, for instance. Numbered on from the shell's own four so the strip
 * stays one status word, and gone the moment the workspace unmounts.
 */
function WorkspaceRegisters() {
  const registers = useStatusRegisters();
  return (
    <>
      {registers.map((register, index) => (
        <Cell key={register.id} icon={Link2} label={`${index + 5} · ${register.label}`}>
          <span
            aria-hidden
            className={cn(
              'size-2 shrink-0',
              register.state === 'identity'
                ? 'bg-accent'
                : (STATE_LAMP[register.state] ?? 'bg-state-unsupported-schema'),
            )}
          />
          <span
            role="status"
            data-register={register.id}
            data-state={register.state}
            className="flex min-w-0 items-center gap-1.5"
          >
            <span className="td-value max-w-56 truncate text-2xs">{register.value}</span>
            {register.detail ? (
              <span className="td-value min-w-0 max-w-64 truncate text-3xs text-text-muted">
                {register.detail}
              </span>
            ) : null}
          </span>
        </Cell>
      ))}
    </>
  );
}

/**
 * Where the readings on screen actually come from, so the strip cannot lie by
 * omission: a healthy plate over a dead link is a CAPTURED read, not a live
 * one.
 *
 *   LIVE      the event stream is up; plates follow the daemon.
 *   CAPTURED  the stream is down but resolved reads are still on screen —
 *             fixtures, or the last answers before the link dropped. Stamped
 *             in the alert register because it is exactly the state a reader
 *             must not mistake for live.
 *   NO SOURCE the stream is down and nothing has answered; the plates are
 *             empty frames, which is its own honest state.
 */
export function SourceProvenance() {
  const { state } = useEventStreamState();
  const hasData = useAnyResolvedRead();
  // Only a LIVE stream earns the live stamp. A connecting stream is a stream
  // that is not delivering: whatever the plates show meanwhile is a captured
  // read, and stamping it anything softer would be the strip vouching for
  // freshness it cannot see.
  const source =
    state === 'live'
      ? { value: 'live', tone: 'bg-state-ready', ink: 'text-text-primary' }
      : hasData
        ? { value: 'captured', tone: 'bg-alert', ink: 'text-alert' }
        : { value: 'no source', tone: 'bg-state-offline', ink: 'text-text-muted' };
  return (
    <Cell icon={HardDrive} label="Source">
      <span aria-hidden className={cn('size-1.5 shrink-0', source.tone)} />
      <span
        role="status"
        data-source-provenance={source.value}
        className={cn('td-value text-2xs uppercase', source.ink)}
      >
        {source.value}
      </span>
    </Cell>
  );
}

/** Whether any read model on screen has resolved with data — the difference
 * between CAPTURED (plates hold a real read) and NO SOURCE (empty frames). */
function useAnyResolvedRead(): boolean {
  const client = useQueryClient();
  const subscribe = useCallback(
    (onChange: () => void) => client.getQueryCache().subscribe(onChange),
    [client],
  );
  return useSyncExternalStore(subscribe, () =>
    client
      .getQueryCache()
      .getAll()
      .some((query) => query.state.data !== undefined),
  );
}

/** Query-aware cell mounted by Shell, which is inside QueryClientProvider.
 * Keeping it separate lets the transport strip remain usable in isolation
 * without inventing a second QueryClient. */
export function QueryActivityStatus() {
  const client = useQueryClient();
  const queryActivities = useActiveQueryActivities();
  const activeQuery = queryActivities[0];
  const lastCancellation = useQueryCancellation((entry) => entry.lastCancellation);

  if (activeQuery !== undefined) {
    return (
      <Cell icon={Search} label="Query">
        <span className="td-value max-w-64 truncate text-2xs" role="status">
          {activeQuery.label}
        </span>
        {queryActivities.length > 1 ? (
          <span className="td-value text-3xs text-text-muted">
            +{queryActivities.length - 1}
          </span>
        ) : null}
        {activeQuery.cancelable ? (
          // The one control on the strip, so the one thing that can push the
          // strip past its 32px: a 44px target does not fit a 32px strip, and
          // the strip grows for the duration of a cancelable query rather than
          // the target shrinking under the minimum.
          <button
            type="button"
            aria-label={`Cancel ${activeQuery.label}`}
            onClick={() => void cancelQueryActivity(client, activeQuery)}
            className="flex min-h-[var(--touch-target-min)] min-w-11 items-center justify-center border-l border-edge-subtle px-2 text-2xs uppercase text-text-secondary hover:bg-surface-2 hover:text-text-primary"
          >
            Cancel
          </button>
        ) : null}
      </Cell>
    );
  }

  return lastCancellation === null ? null : (
    <Cell icon={Search} label="Query">
      <span className="td-value max-w-72 truncate text-2xs" role="status">
        cancelled · {lastCancellation.label}
      </span>
    </Cell>
  );
}

/**
 * The project registry's own state, read off the listing the shell already
 * depends on for scope and the command palette.
 *
 * Distinct from Link and Feed: the socket can be live and the projection
 * synced while the registry — the authority that says which projects exist —
 * is missing, unopenable, or refusing this caller. It is also distinct from
 * the scope register's per-project reconciliation, which asks about one id;
 * this asks whether the registry can be read at all. The typed-state
 * vocabulary is DESIGN-SYSTEM.md's: `ready` is green and solid; `truncated`
 * is degraded, because a page is not the registry; `unavailable` and
 * `offline` are disconnected; refusals and unreadable bodies are their own
 * words. Nothing here is ever folded into a blank or a green zero.
 */
export interface RegistryAuthority {
  value: string;
  tone: string;
  detail: string | null;
}

export function registryAuthorityReading(
  result: ProjectRegistryResult<ProjectsPayloadV1> | undefined,
): RegistryAuthority {
  if (result === undefined) return { value: 'loading', tone: 'bg-state-loading', detail: null };
  switch (result.outcome) {
    case 'envelope': {
      const payload = result.envelope.payload;
      switch (payload.status) {
        case 'ok':
          return payload.truncated === true
            ? {
                value: 'truncated',
                tone: 'bg-state-partial',
                detail: `first ${payload.limit} projects only`,
              }
            : { value: 'ready', tone: 'bg-state-ready', detail: null };
        case 'missing_registry':
          return {
            value: 'unavailable',
            tone: 'bg-state-offline',
            detail: payload.error ?? 'no registry on this profile',
          };
        case 'registry_unavailable':
          return {
            value: 'unavailable',
            tone: 'bg-state-offline',
            detail: payload.error ?? 'the registry could not be opened',
          };
        default:
          return {
            value: 'unexpected status',
            tone: 'bg-state-error',
            detail: payload.status,
          };
      }
    }
    case 'transport':
      switch (result.state) {
        case 'offline':
          return { value: 'offline', tone: 'bg-state-offline', detail: null };
        case 'unauthorized':
          return { value: 'unauthorized', tone: 'bg-state-unauthorized', detail: null };
        case 'denied':
          return { value: 'denied', tone: 'bg-state-denied', detail: null };
        case 'locked':
          return { value: 'locked', tone: 'bg-state-locked', detail: result.detail ?? null };
        case 'unsupported_schema':
        case 'unsupported':
          return { value: 'unsupported schema', tone: 'bg-state-unsupported-schema', detail: null };
        case 'error':
          return { value: 'error', tone: 'bg-state-error', detail: result.detail ?? null };
        case 'cancelled':
        case 'complete_zero_findings':
        case 'conflicting':
        case 'loading':
        case 'partial':
        case 'ready':
        case 'redacted':
        case 'stale':
        case 'timed_out':
        case 'unknown':
          // `fetchProjectRegistry` never produces these for a transport
          // failure; a future one that does is shown by its own name rather
          // than as any softer word.
          return { value: result.state.replace('_', ' '), tone: 'bg-state-unknown', detail: null };
        default: {
          const unhandled: never = result.state;
          return unhandled;
        }
      }
    default: {
      const unhandled: never = result;
      return unhandled;
    }
  }
}

/** Registry-authority cell mounted by Shell, inside QueryClientProvider, for
 * the same reason {@link QueryActivityStatus} is. */
export function RegistryAuthorityStatus() {
  const registry = registryAuthorityReading(useProjectRegistry().data);
  return (
    <Cell icon={FolderTree} label="Registry">
      <span aria-hidden className={cn('size-1.5 shrink-0', registry.tone)} />
      <span
        role="status"
        data-registry-authority={registry.value}
        className="flex min-w-0 items-center gap-1.5"
      >
        <span className="td-value text-2xs uppercase">{registry.value}</span>
        {registry.detail !== null && (
          <span className="td-value min-w-0 truncate text-3xs normal-case text-text-muted">
            {registry.detail}
          </span>
        )}
      </span>
    </Cell>
  );
}

/** One register of the strip: a 14px monoline glyph, the engraved subsystem
 * stamp, and the reading after it. The glyph identifies the subsystem
 * alongside the word; it never carries the state on its own. */
function Cell({
  icon: Icon,
  label,
  children,
}: {
  icon: LucideIcon;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex min-w-0 shrink-0 items-center gap-2 border-r border-edge-subtle px-3">
      <Icon aria-hidden size={14} strokeWidth={1.5} className="shrink-0 text-text-muted" />
      <span className="td-legend">{label}</span>
      <span className="flex min-w-0 items-center gap-1.5">{children}</span>
    </div>
  );
}
