import { useCallback, useMemo } from 'react';
import { useSearchParams } from 'react-router';
import {
  DeliveryInboxV1Schema,
  type DeliveryInboxPullRequestV1,
  type DeliveryInboxV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { CenteredState, ReadSection, type ReadState } from '../../ui/ReadSection.tsx';
import { WorkspaceHeader } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { ReadOnlyProviderBadge } from './deliveryChrome.tsx';
import { useProjectOverview, type DeliveryContext } from './deliveryContext.ts';
import {
  DELIVERY_MODES,
  modeLabel,
  readDeliveryLocation,
  writeDeliveryLocation,
  type DeliveryLocation,
  type DeliveryLocationPatch,
  type DeliveryMode,
} from './deliveryLocation.ts';
import { edgesFor, filterInbox, projectFor } from './inboxFilter.ts';
import { providerServes } from './evidence.ts';
import { pullRequestNumberLabel } from './deliveryReading.ts';
import { buildUmbrellas } from './umbrella.ts';
import { InboxWorkspace } from './InboxWorkspace.tsx';
import { UmbrellaWorkspace } from './UmbrellaWorkspace.tsx';
import { JourneyWorkspace } from './JourneyWorkspace.tsx';
import { ReviewWorkspace } from './ReviewWorkspace.tsx';
import { LocalFirstWing } from './LocalFirstWing.tsx';

/**
 * Delivery, channel 08.
 *
 * One DOM shell, four workspace modes over two real read authorities:
 *
 * - `GET /api/delivery/inbox`, the registry-admitted, indexed-head-joined
 *   pull request inbox across every registered project, with server-owned
 *   attention and membership edges.
 * - `GET /api/projects/{id}/delivery/overview`, the selected project's eight
 *   independently typed Git/provider projections (changes, commits, pull
 *   requests, reviews, CI checks, failure localization, releases, freshness).
 *
 * Umbrellas, journeys and the review workspace are projections over those two
 * reads joined only through named identities and graded bases. Nothing here
 * writes to a provider; nothing turns a missing authority into an empty list.
 */
export function DeliveryPage() {
  const inbox = useEnvelope(['delivery', 'inbox'], '/api/delivery/inbox', DeliveryInboxV1Schema);
  const [params, setParams] = useSearchParams();
  const location = useMemo(() => readDeliveryLocation(params), [params]);
  const navigate = useCallback(
    (patch: DeliveryLocationPatch) => setParams(writeDeliveryLocation(params, patch)),
    [params, setParams],
  );
  const payload = inbox.data?.outcome === 'envelope' ? inbox.data.envelope.payload : null;
  const selectedRow =
    payload?.pull_requests.find((row) => row.id === location.pullRequest) ?? null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorkspaceHeader
        path="delivery"
        title="Delivery"
        note={headerNote(location, selectedRow)}
        actions={
          <div className="ml-auto flex min-w-0 max-w-full flex-wrap items-center gap-2 max-sm:w-full">
            <ModeTabs location={location} navigate={navigate} selected={selectedRow} />
            <ReadOnlyProviderBadge className="max-md:hidden" />
          </div>
        }
      />
      <ReadSection
        title="Delivery inbox"
        chrome="centered"
        state={inboxReadState(inbox.isPending, inbox.data)}
      >
        {(value) => (
          <DeliveryBody
            payload={value}
            location={location}
            navigate={navigate}
            params={params}
            selectedRow={selectedRow}
          />
        )}
      </ReadSection>
    </div>
  );
}

function headerNote(
  location: DeliveryLocation,
  selected: DeliveryInboxPullRequestV1 | null,
): string {
  switch (location.mode) {
    case 'inbox':
      return location.project === null
        ? 'all registered projects · indexed PR heads · explicit attention evidence'
        : `project ${location.project} · local-first · correlated PRs qualified by basis`;
    case 'umbrella':
      return 'correlation projection · every edge names its basis and grade';
    case 'journey':
      return selected === null
        ? 'select a pull request to open its journey'
        : `${selected.project_id} · ${pullRequestNumberLabel(selected.pull_request)} · time is X, source is Y`;
    case 'review':
      return selected === null
        ? 'select a pull request to open its review workspace'
        : `${selected.project_id} · ${pullRequestNumberLabel(selected.pull_request)} · exact threads, checks, identity`;
    default: {
      const unhandled: never = location.mode;
      return unhandled;
    }
  }
}

function ModeTabs({
  location,
  navigate,
  selected,
}: {
  location: DeliveryLocation;
  navigate: (patch: DeliveryLocationPatch) => void;
  selected: DeliveryInboxPullRequestV1 | null;
}) {
  return (
    <nav aria-label="Delivery modes" className="flex max-w-full flex-wrap items-center border border-edge-subtle">
      {DELIVERY_MODES.map((mode) => {
        const requiresSelection = modeRequiresSelection(mode) && selected === null;
        const active = location.mode === mode;
        return (
          <button
            key={mode}
            type="button"
            aria-current={active ? 'page' : undefined}
            disabled={requiresSelection}
            title={requiresSelection ? 'Select a pull request in the inbox first' : undefined}
            className={cn(
              'td-hit relative px-3 text-2xs uppercase tracking-[0.14em] transition-colors',
              active ? 'text-text-primary' : 'text-text-muted hover:text-text-secondary',
              requiresSelection && 'cursor-not-allowed opacity-50',
            )}
            onClick={() => navigate({ mode })}
          >
            {active ? (
              <span aria-hidden className="absolute inset-x-2 bottom-1.5 h-px bg-accent" />
            ) : null}
            {modeLabel(mode)}
          </button>
        );
      })}
    </nav>
  );
}

function modeRequiresSelection(mode: DeliveryMode): boolean {
  switch (mode) {
    case 'inbox':
    case 'umbrella':
      return false;
    case 'journey':
    case 'review':
      return true;
    default: {
      const unhandled: never = mode;
      return unhandled;
    }
  }
}

function inboxReadState(
  pending: boolean,
  result: EnvelopeResult<DeliveryInboxV1> | undefined,
): ReadState<DeliveryInboxV1> {
  if (pending) {
    return { kind: 'blocked', state: 'loading', detail: 'reading the admitted delivery inbox' };
  }
  if (!result) {
    return { kind: 'blocked', state: 'unknown', detail: 'no inbox response recorded' };
  }
  if (result.outcome === 'transport') {
    return {
      kind: 'blocked',
      state: result.state,
      detail: result.detail ?? 'the delivery inbox could not be read',
    };
  }
  if (result.envelope.authorization.outcome !== 'authorized') {
    return {
      kind: 'blocked',
      state: result.envelope.authorization.outcome,
      detail: 'delivery evidence was not disclosed',
    };
  }
  return { kind: 'ready', value: result.envelope.payload };
}

function DeliveryBody({
  payload,
  location,
  navigate,
  params,
  selectedRow,
}: {
  payload: DeliveryInboxV1;
  location: DeliveryLocation;
  navigate: (patch: DeliveryLocationPatch) => void;
  params: URLSearchParams;
  selectedRow: DeliveryInboxPullRequestV1 | null;
}) {
  const umbrellas = useMemo(() => buildUmbrellas(payload), [payload]);
  const context: DeliveryContext = { inbox: payload, umbrellas, location, params, navigate };

  if (payload.registry_state === 'unavailable') {
    return (
      <CenteredState
        title="Project registry unavailable"
        kind="unavailable"
        detail="The daemon could not enumerate registered TraceDecay projects. This is not an empty inbox."
      />
    );
  }

  switch (location.mode) {
    case 'inbox':
      return <InboxMode context={context} selectedRow={selectedRow} />;
    case 'umbrella':
      return <UmbrellaWorkspace context={context} />;
    case 'journey':
      return selectedRow === null ? (
        <RequiresSelection mode="journey" />
      ) : (
        <JourneyWorkspace context={context} row={selectedRow} edges={edgesFor(payload, selectedRow)} />
      );
    case 'review':
      return selectedRow === null ? (
        <RequiresSelection mode="review" />
      ) : (
        <ReviewWorkspace context={context} row={selectedRow} />
      );
    default: {
      const unhandled: never = location.mode;
      return unhandled;
    }
  }
}

function RequiresSelection({ mode }: { mode: DeliveryMode }) {
  return (
    <CenteredState
      title={`${modeLabel(mode)} requires a pull request`}
      kind="unknown"
      detail="No admitted pull request is selected. Choose one in the inbox; the URL then addresses its journey and review."
    />
  );
}

/**
 * The inbox, plus the local-first Repositories wing when the scoped project's
 * provider cannot serve pull requests: local Git evidence stays useful and the
 * provider absence is printed with the daemon's own reason, not as zero PRs.
 */
function InboxMode({
  context,
  selectedRow,
}: {
  context: DeliveryContext;
  selectedRow: DeliveryInboxPullRequestV1 | null;
}) {
  const rows = useMemo(
    () => filterInbox(context.inbox, context.location),
    [context.inbox, context.location],
  );
  const scopedProject = projectFor(context.inbox, context.location.project);
  const localFirst = scopedProject !== null && !providerServes(scopedProject.provider_state);
  const overview = useProjectOverview(localFirst ? scopedProject.project_id : null);
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {localFirst ? <LocalFirstWing project={scopedProject} overview={overview} /> : null}
      <InboxWorkspace context={context} rows={rows} selectedRow={selectedRow} />
    </div>
  );
}
