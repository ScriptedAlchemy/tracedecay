/** Explorer's right column: everything the daemon actually returned about one
 * selected row, plus the two session reads a transcript row can be expanded
 * into. Nothing here is computed about the row — it reports the field each
 * value was read from so a reader can check it. */
import { InspectorPanel, RawFields } from '../../ui/archetypes/ExplorerSplit.tsx';
import { StateChip } from '../../ui/StateChip';
import { Highlight, MetaLabel } from '../../ui/search/Highlight.tsx';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import type { ExplorerReadContextV1, ExplorerSessionSizeV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { useExplorerSessionContext } from './controller.ts';
import { LANE_BY_ID, LANE_ICON } from './laneChrome.ts';
import { relativeTime, type Hit } from './model.ts';

export function HitInspector({
  hit,
  terms,
  onClose,
}: {
  hit: Hit;
  terms: readonly string[];
  onClose: () => void;
}) {
  const spec = LANE_BY_ID[hit.lane];
  const Icon = LANE_ICON[hit.lane];
  const age = relativeTime(hit.stamp);
  const sessionId = sessionIdOf(hit);
  const session = useExplorerSessionContext(sessionId);
  return (
    <InspectorPanel
      title={hit.title}
      eyebrow={
        <>
          <Icon aria-hidden size={11} className={spec.textClass} />
          {spec.label} · rank {hit.rank}
        </>
      }
      onClose={onClose}
    >
      <div className="flex flex-col gap-4">
        <section className="flex flex-col gap-1.5">
          <MetaLabel>Why this is here</MetaLabel>
          <p className="text-2xs leading-relaxed text-text-secondary">
            {terms.length === 0 ? (
              <>
                Browsing {spec.browseLabel}; position {hit.rank} is the order the daemon
                returned, not a score.
              </>
            ) : hit.matchedIn.length > 0 ? (
              <>
                Position {hit.rank} in {hit.orderLabel}. The query text occurs in{' '}
                <span className="font-mono text-text-primary">
                  {hit.matchedIn.join(', ')}
                </span>
                .
              </>
            ) : (
              <>
                Position {hit.rank} in {hit.orderLabel}. The daemon matched on its
                own index; the literal terms do not appear in the fields it returned.
              </>
            )}
          </p>
        </section>
        {hit.context ? (
          <section className="flex flex-col gap-1">
            <MetaLabel>Where</MetaLabel>
            <Highlight
              text={hit.context}
              terms={terms}
              className="break-all font-mono text-2xs text-text-secondary"
            />
          </section>
        ) : null}
        {hit.signal ? (
          <section className="flex flex-col gap-1.5">
            <MetaLabel>Measured</MetaLabel>
            <span className="flex items-center gap-2">
              <Meter
                fraction={hit.signal.max > 0 ? hit.signal.value / hit.signal.max : null}
                className="w-10 rounded-full"
                tone="bg-accent/80"
                ariaLabel={`${hit.signal.field} ${hit.signal.value}`}
              />
              <span className="tabular text-xs text-text-primary">
                {hit.signal.display}
              </span>
              <span className="font-mono text-2xs text-text-muted">
                {hit.signal.field}
              </span>
            </span>
          </section>
        ) : null}
        {hit.body ? (
          <section className="flex flex-col gap-1">
            <MetaLabel>{hit.lane === 'code' ? 'Signature' : 'Body'}</MetaLabel>
            <Highlight
              text={hit.body}
              terms={terms}
              className={cn(
                'whitespace-pre-wrap break-words text-xs leading-relaxed text-text-secondary',
                hit.lane === 'code' && 'font-mono',
              )}
            />
          </section>
        ) : null}
        <section className="flex flex-col gap-1">
          <MetaLabel>{hit.titleField}</MetaLabel>
          <Highlight
            text={hit.title}
            terms={terms}
            className={cn(
              'whitespace-pre-wrap break-words text-xs leading-[1.6] text-text-primary',
              hit.lane === 'code' && 'font-mono',
            )}
          />
          {age ? <span className="text-2xs text-text-muted">{age} ago</span> : null}
        </section>
        {sessionId ? (
          <SessionContextDetails
            sessionId={sessionId}
            size={session.size}
            readContext={session.readContext}
            pending={session.pending}
          />
        ) : null}
        <RawFields value={hit.raw} label="Payload provenance" />
      </div>
    </InspectorPanel>
  );
}

/** The session a transcript row belongs to, when the row named one. A row
 * without a usable `session_id` opens no session read at all rather than one
 * against an empty identifier. */
function sessionIdOf(hit: Hit): string | undefined {
  if (hit.lane !== 'sessions') return undefined;
  const raw = hit.raw['session_id'];
  if (typeof raw !== 'string') return undefined;
  const trimmed = raw.trim();
  return trimmed === '' ? undefined : trimmed;
}

function SessionContextDetails({
  sessionId,
  size,
  readContext,
  pending,
}: {
  sessionId: string;
  size: EnvelopeResult<ExplorerSessionSizeV1> | undefined;
  readContext: EnvelopeResult<ExplorerReadContextV1> | undefined;
  pending: boolean;
}) {
  const sizePayload = size?.outcome === 'envelope' ? size.envelope.payload : undefined;
  const contextPayload =
    readContext?.outcome === 'envelope' ? readContext.envelope.payload : undefined;
  if (pending && !sizePayload && !contextPayload) {
    return (
      <section className="flex flex-col gap-1.5">
        <MetaLabel>Session context</MetaLabel>
        <StateChip kind="loading" detail={sessionId} />
      </section>
    );
  }
  if (!sizePayload && !contextPayload) {
    const offline =
      size?.outcome === 'transport' && size.state === 'offline'
        ? true
        : readContext?.outcome === 'transport' && readContext.state === 'offline';
    return (
      <section className="flex flex-col gap-1.5">
        <MetaLabel>Session context</MetaLabel>
        <StateChip kind={offline ? 'offline' : 'error'} detail={sessionId} />
      </section>
    );
  }
  return (
    <section className="flex flex-col gap-2">
      <MetaLabel>Session context</MetaLabel>
      {sizePayload ? (
        <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-2xs">
          <dt className="text-text-muted">Messages</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.message_count.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Summary nodes</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.summary_node_count.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Raw token estimate</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.token_estimate_total.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Store</dt>
          <dd className="text-text-secondary">{sizePayload.storage_scope}</dd>
        </dl>
      ) : null}
      {contextPayload ? (
        <>
          <p className="text-2xs leading-relaxed text-text-muted">
            Loaded {contextPayload.messages.length.toLocaleString()} raw messages and{' '}
            {contextPayload.summary_nodes.length.toLocaleString()} summary nodes in{' '}
            {contextPayload.order} order
            {contextPayload.has_more ? '; more rows remain' : '; this read is complete'}.
          </p>
          <RawFields
            value={contextPayload}
            label="Session read context returned by the daemon"
          />
        </>
      ) : null}
    </section>
  );
}
