/**
 * Explorer's inspector: everything the daemon actually returned about one
 * result, graded, plus the two session reads a transcript row can be
 * expanded into once it is selected.
 *
 * It opens two ways and says which. A PEEK is a pointer resting on a row or
 * keyboard focus landing on it, the panel shows the row from what is already
 * on screen and fetches nothing. A SELECTION is a click or Enter, it persists,
 * and only then does a transcript row open its session reads. Nothing here is
 * computed about the row; every value names the field it was read from.
 */
import { ArrowUpRight } from 'lucide-react';
import { Link } from 'react-router';
import { InspectorPanel, RawFields } from '../../ui/archetypes/ExplorerSplit.tsx';
import { StateChip } from '../../ui/StateChip';
import { Highlight, MetaLabel } from '../../ui/search/Highlight.tsx';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type {
  ExplorerReadContextV1,
  ExplorerSessionSizeV1,
} from '../../contracts/generated.ts';
import { scopedWorkspacePath, useScope } from '../../data/scope/store.ts';
import { compactRelativeAge } from '../../ui/time.ts';
import { writeCodeLocation } from '../code/codeView.ts';
import { useExplorerSessionContext } from './controller.ts';
import { hitEvidence } from './evidence.ts';
import { LANE_BY_ID, LANE_ICON } from './laneChrome.ts';
import { type Hit } from './model.ts';

export type InspectMode = 'peek' | 'selected';

export function HitInspector({
  hit,
  mode,
  terms,
  onClose,
}: {
  hit: Hit;
  mode: InspectMode;
  terms: readonly string[];
  onClose: () => void;
}) {
  const spec = LANE_BY_ID[hit.lane];
  const Icon = LANE_ICON[hit.lane];
  const evidence = hitEvidence(hit);
  const age = compactRelativeAge(hit.stamp, Date.now() / 1000);
  const sessionId = sessionIdOf(hit);
  const session = useExplorerSessionContext(sessionId, mode === 'selected');
  const identityKey = identityFieldOf(hit);
  return (
    <InspectorPanel
      title={hit.title}
      eyebrow={
        <>
          <Icon aria-hidden size={11} className={spec.textClass} />
          {spec.label} · {mode === 'peek' ? 'inspecting' : 'selected'}
        </>
      }
      onClose={mode === 'selected' ? onClose : undefined}
    >
      <div className="flex flex-col gap-4" data-inspect-mode={mode}>
        <section className="flex flex-col gap-1.5">
          <MetaLabel>Source identity</MetaLabel>
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-body">
            <dt className="text-text-muted">Lane</dt>
            <dd className="text-text-secondary">{spec.label}</dd>
            <dt className="text-text-muted">Source class</dt>
            <dd className="td-value text-sm text-text-secondary">{evidence.sourceClass}</dd>
            <dt className="text-text-muted">Identity</dt>
            <dd className="td-value text-sm text-text-secondary" data-evidence-grade={evidence.identity}>
              {evidence.identity}
            </dd>
            {identityKey ? (
              <>
                <dt className="text-text-muted">{identityKey.field}</dt>
                <dd className="min-w-0 break-all font-mono text-sm text-text-primary">
                  {identityKey.value}
                </dd>
              </>
            ) : null}
            {hit.context ? (
              <>
                <dt className="text-text-muted">Where</dt>
                <dd className="min-w-0">
                  <Highlight
                    text={hit.context}
                    terms={terms}
                    className="break-all font-mono text-sm text-text-secondary"
                  />
                </dd>
              </>
            ) : null}
            {hit.facet ? (
              <>
                <dt className="text-text-muted">{spec.facetLabel}</dt>
                <dd className="text-text-secondary">{hit.facet}</dd>
              </>
            ) : null}
            <dt className="text-text-muted">Position</dt>
            <dd className="text-text-secondary">
              #{hit.rank} in {hit.orderLabel}
            </dd>
            {age ? (
              <>
                <dt className="text-text-muted">{hit.stampField}</dt>
                <dd className="tabular text-text-secondary">{age} ago</dd>
              </>
            ) : null}
          </dl>
        </section>

        <section className="flex flex-col gap-1">
          <span className="flex items-baseline justify-between gap-2">
            <MetaLabel>
              Snippet · <span className="font-mono normal-case">{hit.titleField}</span>
            </MetaLabel>
            <span className="td-legend text-text-secondary" data-evidence-grade={evidence.text}>
              {evidence.text}
            </span>
          </span>
          <Highlight
            text={hit.title}
            terms={terms}
            className={cn(
              'whitespace-pre-wrap break-words border border-edge-subtle bg-surface-0 px-2 py-1.5 text-xs leading-[1.6] text-text-primary',
              hit.lane === 'code' && 'font-mono',
            )}
          />
          {hit.body && hit.bodyField ? (
            <>
              <MetaLabel className="mt-1">
                <span className="font-mono normal-case">{hit.bodyField}</span>
              </MetaLabel>
              <Highlight
                text={hit.body}
                terms={terms}
                className={cn(
                  'whitespace-pre-wrap break-words text-xs leading-relaxed text-text-secondary',
                  hit.lane === 'code' && 'font-mono',
                )}
              />
            </>
          ) : null}
        </section>

        {hit.signal ? (
          <section className="flex flex-col gap-1.5">
            <MetaLabel>Measured</MetaLabel>
            <span className="flex items-center gap-2">
              <Meter
                fraction={hit.signal.max > 0 ? hit.signal.value / hit.signal.max : null}
                className="w-10"
                tone="bg-accent/80"
                ariaLabel={`${hit.signal.field} ${hit.signal.value}`}
              />
              <span className="tabular text-xs text-text-primary">{hit.signal.display}</span>
              <span className="font-mono text-sm text-text-muted">{hit.signal.field}</span>
            </span>
            <span className="text-body text-text-muted">{hit.signal.basis}</span>
          </section>
        ) : null}

        <section className="flex flex-col gap-1.5">
          <MetaLabel>Why this is here</MetaLabel>
          <p className="text-body leading-relaxed text-text-secondary">
            {terms.length === 0 ? (
              <>
                Browsing {spec.browseLabel}; position {hit.rank} is the order the daemon
                returned, not a score.
              </>
            ) : hit.matchedIn.length > 0 ? (
              <>
                Position {hit.rank} in {hit.orderLabel}. The query text occurs in{' '}
                <span className="font-mono text-text-primary">{hit.matchedIn.join(', ')}</span>.
              </>
            ) : (
              <>
                Position {hit.rank} in {hit.orderLabel}. The daemon matched on its own index;
                the literal terms do not appear in the fields it returned.
              </>
            )}
          </p>
        </section>

        <section className="flex flex-col gap-1.5">
          <MetaLabel>Provenance</MetaLabel>
          <p className="text-body leading-relaxed text-text-muted">{evidence.basis}</p>
          <CodePivot hit={hit} />
          {sessionId ? (
            <SessionContextDetails
              sessionId={sessionId}
              mode={mode}
              size={session.size}
              readContext={session.readContext}
              pending={session.pending}
            />
          ) : null}
          <RawFields value={hit.raw} label="Payload provenance" />
        </section>
      </div>
    </InspectorPanel>
  );
}

/** The field a row's identity was read from, so the inspector can print the
 * exact key beside its grade instead of only the display title. */
function identityFieldOf(hit: Hit): { field: string; value: string } | null {
  const field = ((): string | null => {
    switch (hit.lane) {
      case 'code':
        return 'id';
      case 'sessions':
        return typeof hit.raw['message_id'] === 'string' ? 'message_id' : 'node_id';
      case 'knowledge':
        return 'fact_id';
      default: {
        const exhaustive: never = hit.lane;
        return exhaustive;
      }
    }
  })();
  if (field === null) return null;
  const value = hit.raw[field];
  return typeof value === 'string' && value !== '' ? { field, value } : null;
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

/**
 * The one pivot a result can make today: a code row's graph node id is the
 * same identity the Code workspace focuses on, so the link is exact. Session
 * and fact rows have no deep-link target in their workspaces yet and get no
 * link, a control to nowhere would be worse than none.
 */
function CodePivot({ hit }: { hit: Hit }) {
  const scope = useScope((s) => s.scope);
  if (hit.lane !== 'code') return null;
  const id = hit.raw['id'];
  if (typeof id !== 'string' || id === '') return null;
  const base = scopedWorkspacePath(scope, 'code');
  const [pathname, search = ''] = base.split('?');
  const params = writeCodeLocation(new URLSearchParams(search), {
    view: 'cortex',
    focusId: id,
  });
  return (
    <Link
      to={`${pathname}?${params.toString()}`}
      className="td-hit inline-flex w-fit items-center gap-1.5 border border-edge-subtle px-2 text-body text-text-secondary hover:border-accent hover:text-text-primary"
    >
      Open symbol in Code
      <ArrowUpRight aria-hidden size={11} />
    </Link>
  );
}

function SessionContextDetails({
  sessionId,
  mode,
  size,
  readContext,
  pending,
}: {
  sessionId: string;
  mode: InspectMode;
  size: EnvelopeResult<ExplorerSessionSizeV1> | undefined;
  readContext: EnvelopeResult<ExplorerReadContextV1> | undefined;
  pending: boolean;
}) {
  if (mode === 'peek') {
    return (
      <section className="flex flex-col gap-1.5">
        <MetaLabel>Session context</MetaLabel>
        <p className="text-body leading-relaxed text-text-muted">
          Session <span className="font-mono">{sessionId}</span>. Select this row to read its
          size and context; inspecting does not open reads.
        </p>
      </section>
    );
  }
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
    const blocked =
      size?.outcome === 'transport'
        ? size
        : readContext?.outcome === 'transport'
          ? readContext
          : undefined;
    return (
      <section className="flex flex-col gap-1.5">
        <MetaLabel>Session context</MetaLabel>
        <StateChip kind={blocked?.state ?? 'error'} detail={blocked?.detail ?? sessionId} />
      </section>
    );
  }
  return (
    <section className="flex flex-col gap-2">
      <MetaLabel>Session context</MetaLabel>
      {sizePayload ? (
        <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-body">
          <dt className="text-text-muted">Messages</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.message_count.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Summary nodes</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.summary_node_count.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Source tokens</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.source_token_count?.toLocaleString() ?? 'unavailable'}
          </dd>
          <dt className="text-text-muted">Store</dt>
          <dd className="text-text-secondary">{sizePayload.storage_scope}</dd>
        </dl>
      ) : null}
      {contextPayload ? (
        <>
          <p className="text-body leading-relaxed text-text-muted">
            Loaded {contextPayload.messages.length.toLocaleString()} raw messages and{' '}
            {contextPayload.summary_nodes.length.toLocaleString()} summary nodes in{' '}
            {contextPayload.order} order
            {contextPayload.has_more ? '; more rows remain' : '; this read is complete'}.
          </p>
          <RawFields value={contextPayload} label="Session read context returned by the daemon" />
        </>
      ) : null}
    </section>
  );
}
