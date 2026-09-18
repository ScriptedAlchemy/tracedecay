/**
 * SESSION PROVENANCE INSPECTOR, what is durably known about one session, and
 * from where, with every gap typed.
 *
 * Three authorities meet here and stay separate:
 *
 *   index row      `GET /api/loom/temporal`, the retained session store's
 *                  own row: provider-qualified identity, recorded model
 *                  identities, start / end / last-message stamps, the message
 *                  count the store holds. Read with the index page, so a
 *                  session that is not on the loaded page has no row here and
 *                  says so.
 *   transcript     `GET /api/plugins/hermes-lcm/session/{id}`, the persisted
 *                  turns one server page at a time, the compactor's summary
 *                  nodes, the whole-session counts, and the opaque cursor that
 *                  bounds the page. A message whose body the store does not
 *                  hold is said outright, never rendered as an empty line.
 *   Git relations  the same temporal read's commits, edited-file rollups and
 *                  branch/worktree spans for this provider-qualified session,
 *                  each under its own daemon source status.
 *
 * Private chain-of-thought is not a source class. Nothing here reconstructs
 * reasoning; the inspector names that absence once rather than leaving a
 * gap a reader might take for an omission.
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Link, useSearchParams } from 'react-router';
import { ChevronLeft, ChevronRight, Waypoints } from 'lucide-react';
import {
  assertNever,
  LcmSessionPayloadV1Schema,
  type DashboardEnvelopeV1,
  type LcmMessageV1,
  type LcmSessionPayloadV1,
  type LcmSummaryNodeV1,
  type LoomSessionRowV1,
  type LoomSourceStatusV1,
  type LoomTemporalPayloadV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { scopeKey, useScope } from '../../data/scope/store.ts';
import { InspectorPanel } from '../../ui/archetypes/ExplorerSplit.tsx';
import { EvidenceGradeTag } from '../../ui/EvidenceGrade.tsx';
import { ReadSection, envelopeReadState, type ReadState } from '../../ui/ReadSection.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { Fact, Legend, Meter, Readout } from '../../ui/instrument.tsx';
import { formatStamp, splitCount } from '../../ui/format.ts';
import { formatDurationSeconds } from '../loom/tracks.ts';
import { tokenCountLabel } from './tokenLabel.ts';
import {
  commitEvidence,
  joinIndexRow,
  recordedModels,
  relationsFor,
  sessionExtent,
  type SessionRelations,
  type SessionSelection,
} from './model.ts';

/** One page of transcript. The route caps at 500; this stays well inside it so
 * an inspector open never pulls a whole corpus into the browser. */
const PAGE_SIZE = 100;

const PAGER_BEZEL =
  'inline-flex items-center gap-1 border border-edge-subtle bg-surface-2 px-2 py-1 text-3xs text-text-secondary group-hover:text-text-primary';

export interface SessionInspectorProps {
  selection: SessionSelection;
  /** The loaded index page: identity facts and Git relations are read from it. */
  index: ReadState<DashboardEnvelopeV1<LoomTemporalPayloadV1>>;
  indexPage: number;
  /** Resolves an ambiguous selection by naming the provider the reader chose. */
  onSelectProvider: (provider: string) => void;
  onClose: () => void;
}

export function SessionInspector({
  selection,
  index,
  indexPage,
  onSelectProvider,
  onClose,
}: SessionInspectorProps) {
  const identity = resolveIdentity(index, selection, indexPage);
  const provider = identity.kind === 'row' ? identity.row.provider : selection.provider;
  return (
    <InspectorPanel
      title="Session provenance"
      eyebrow={
        <>
          <span className="td-value normal-case tracking-normal">{provider ?? 'provider unknown'}</span>
          <EvidenceGradeTag grade="EXACT" sourceClass="SESSION ID" />
        </>
      }
      onClose={onClose}
    >
      <div className="flex flex-col gap-4">
        <p className="td-value break-all text-3xs text-text-primary" data-session-inspector-id>
          {selection.sessionId}
        </p>
        <IdentitySection identity={identity} onSelectProvider={onSelectProvider} />
        {provider != null ? (
          <LoomPivot provider={provider} sessionId={selection.sessionId} />
        ) : null}
        <SessionTranscript sessionId={selection.sessionId} />
        <RelationsSection identity={identity} />
      </div>
    </InspectorPanel>
  );
}

/* ------------------------------------------------------------------------ *
 * Identity, the index row, or the typed reason there is none
 * ------------------------------------------------------------------------ */

type Identity =
  | { kind: 'blocked'; state: DomainStateKind; detail: string | undefined }
  | { kind: 'store_unavailable' }
  | { kind: 'row'; row: LoomSessionRowV1; payload: LoomTemporalPayloadV1 }
  | { kind: 'ambiguous'; rows: readonly LoomSessionRowV1[] }
  | { kind: 'absent'; page: number; loaded: number; total: number };

function resolveIdentity(
  index: ReadState<DashboardEnvelopeV1<LoomTemporalPayloadV1>>,
  selection: SessionSelection,
  page: number,
): Identity {
  if (index.kind === 'blocked') {
    return { kind: 'blocked', state: index.state, detail: index.detail };
  }
  const payload = index.value.payload;
  if (payload.available === false) return { kind: 'store_unavailable' };
  const join = joinIndexRow(payload.sessions, selection);
  switch (join.kind) {
    case 'exact':
      return { kind: 'row', row: join.row, payload };
    case 'ambiguous':
      return { kind: 'ambiguous', rows: join.rows };
    case 'absent':
      return { kind: 'absent', page, loaded: payload.sessions.length, total: payload.total };
    default:
      return assertNever(join);
  }
}

function IdentitySection({
  identity,
  onSelectProvider,
}: {
  identity: Identity;
  onSelectProvider: (provider: string) => void;
}) {
  return (
    <section aria-label="Identity" className="flex flex-col gap-2">
      <Legend trailing={<EvidenceGradeTag grade={identityGrade(identity)} sourceClass="SESSION STORE" />}>
        identity
      </Legend>
      <IdentityBody identity={identity} onSelectProvider={onSelectProvider} />
    </section>
  );
}

function identityGrade(identity: Identity) {
  switch (identity.kind) {
    case 'row':
      return 'EXACT';
    case 'ambiguous':
      return 'AMBIGUOUS';
    case 'blocked':
    case 'store_unavailable':
    case 'absent':
      return 'UNAVAILABLE';
    default:
      return assertNever(identity);
  }
}

function IdentityBody({
  identity,
  onSelectProvider,
}: {
  identity: Identity;
  onSelectProvider: (provider: string) => void;
}) {
  switch (identity.kind) {
    case 'blocked':
      return (
        <div className="flex flex-col gap-1.5">
          <StateChip kind={identity.state} detail={identity.detail} />
          <p className="text-3xs leading-snug text-text-muted">
            Identity facts and Git relations are read with the index page; until it answers, only
            the transcript below can be read.
          </p>
        </div>
      );
    case 'store_unavailable':
      return <StateChip kind="unknown" detail="session store not readable" />;
    case 'ambiguous':
      return (
        <div className="flex flex-col gap-1.5" data-identity="ambiguous">
          <p className="text-3xs leading-snug text-text-secondary">
            {identity.rows.length} loaded rows answer to this id under different providers. The
            store keys a session by provider and id together; choose which one to inspect.
          </p>
          <ul className="flex flex-col gap-1">
            {identity.rows.map((row) => (
              <li key={row.provider}>
                <button
                  type="button"
                  className="td-hit group w-full justify-start"
                  onClick={() => onSelectProvider(row.provider)}
                >
                  <span className={PAGER_BEZEL}>
                    <span className="td-legend text-text-secondary">{row.provider}</span>
                    <span className="td-value">
                      {row.started_at != null ? formatStamp(row.started_at) : 'start unrecorded'} ·{' '}
                      {row.messages} msg
                    </span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      );
    case 'absent':
      return (
        <div className="flex flex-col gap-1.5" data-identity="absent">
          <StateChip
            kind="unavailable"
            detail={`not on loaded index page ${identity.page} (${identity.loaded.toLocaleString()} of ${identity.total.toLocaleString()} sessions)`}
          />
          <p className="text-3xs leading-snug text-text-muted">
            Identity facts and Git relations are read with the index page. The transcript below is
            read directly by id.
          </p>
        </div>
      );
    case 'row':
      return <IdentityFacts row={identity.row} />;
    default:
      return assertNever(identity);
  }
}

function IdentityFacts({ row }: { row: LoomSessionRowV1 }) {
  const extent = sessionExtent(row);
  const models = recordedModels(row);
  return (
    <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-2xs" data-identity="row">
      <Fact label="provider" value={row.provider} />
      <Fact label="kind" value={row.is_subagent ? 'subagent' : 'session'} />
      <div className="col-span-2 flex min-w-0 flex-col gap-0.5">
        <dt className="td-legend">title</dt>
        <dd className={row.title ? 'text-3xs text-text-secondary' : 'text-3xs italic text-text-muted'}>
          {row.title ?? 'untitled'}
        </dd>
      </div>
      <div className="col-span-2 flex min-w-0 flex-col gap-0.5">
        <dt className="td-legend">models</dt>
        <dd className="flex flex-wrap gap-1">
          {models.models.map((model) => (
            <span
              key={model}
              className="td-value border border-edge-subtle px-1.5 py-0.5 text-3xs text-text-secondary"
            >
              {model}
            </span>
          ))}
          {models.unrecorded > 0 || models.models.length === 0 ? (
            <span className="text-3xs italic text-text-muted">
              {models.models.length === 0
                ? 'model unrecorded by the provider'
                : `${models.unrecorded} message group${models.unrecorded === 1 ? '' : 's'} without a recorded model`}
            </span>
          ) : null}
        </dd>
      </div>
      <ExtentFacts extent={extent} />
      <Fact label="messages in store row" value={row.messages.toLocaleString()} />
      <Fact
        label="edited-files rollup"
        value={row.edited_files_recorded ? 'recorded' : 'not recorded'}
        muted={!row.edited_files_recorded}
      />
    </dl>
  );
}

function ExtentFacts({ extent }: { extent: ReturnType<typeof sessionExtent> }) {
  switch (extent.kind) {
    case 'undated':
      return (
        <>
          <Fact label="started" value="unrecorded" muted />
          <Fact label="ended" value="unrecorded" muted />
        </>
      );
    case 'ended':
      return (
        <>
          <Fact label="started" value={formatStamp(extent.start)} />
          <Fact label="ended" value={formatStamp(extent.end)} />
          <Fact
            label="extent · derived"
            value={formatDurationSeconds(extent.end - extent.start)}
            muted
          />
        </>
      );
    case 'open':
      return (
        <>
          <Fact label="started" value={formatStamp(extent.start)} />
          <Fact label="ended" value="no recorded end" muted />
          <Fact label="last dated message" value={formatStamp(extent.last)} />
          <Fact
            label="observed span · derived"
            value={formatDurationSeconds(extent.last - extent.start)}
            muted
          />
        </>
      );
    case 'open_unobserved':
      return (
        <>
          <Fact label="started" value={formatStamp(extent.start)} />
          <Fact label="ended" value="no recorded end" muted />
          <Fact label="last dated message" value="none after start" muted />
        </>
      );
    default:
      return assertNever(extent);
  }
}

/** The one cross-workspace pivot the shipping product exposes: Loom selects a
 * thread by its provider-qualified id. Global project scope rides along. */
function LoomPivot({ provider, sessionId }: { provider: string; sessionId: string }) {
  const [params] = useSearchParams();
  const search = new URLSearchParams();
  for (const key of ['scope', 'scopeLabel']) {
    const value = params.get(key);
    if (value !== null) search.set(key, value);
  }
  search.set('loomSession', JSON.stringify([provider, sessionId]));
  return (
    <Link
      to={{ pathname: '/loom', search: `?${search.toString()}` }}
      className="td-hit group self-start"
      data-pivot="loom"
    >
      <span className={PAGER_BEZEL}>
        <Waypoints aria-hidden size={11} />
        Open in Loom
      </span>
    </Link>
  );
}

/* ------------------------------------------------------------------------ *
 * Transcript, one server page at a time
 * ------------------------------------------------------------------------ */

export function SessionTranscript({ sessionId }: { sessionId: string }) {
  // The cache token comes from the authority, never a second construction of
  // it here, `scopeKey` is what every scoped read keys by.
  const scopeCacheKey = useScope((state) => scopeKey(state.scope));
  return <SessionTranscriptPage key={`${scopeCacheKey}:${sessionId}`} sessionId={sessionId} />;
}

function SessionTranscriptPage({ sessionId }: { sessionId: string }) {
  /** The current cursor is the top entry; the rest lets Previous replay the
   * exact opaque cursor the server issued for the preceding page. */
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const cursor = cursorStack.at(-1) ?? null;
  /**
   * The page a reader asked for, held until it arrives on screen. It lives up
   * here because a new cursor is a new query: the read boundary swings to its
   * loading state and unmounts the whole page of rows, including the control
   * that was just activated, and a flag inside that subtree would be
   * reinitialised by the remount.
   */
  const [pageRequest, setPageRequest] = useState(0);
  const session = useEnvelope(
    ['lcm', 'session', sessionId, cursor],
    `/api/plugins/hermes-lcm/session/${encodeURIComponent(sessionId)}?limit=${PAGE_SIZE}${
      cursor == null ? '' : `&cursor=${encodeURIComponent(cursor)}`
    }`,
    LcmSessionPayloadV1Schema,
  );

  return (
    <section aria-label="Transcript" className="flex flex-col gap-2">
      <Legend trailing={<EvidenceGradeTag grade="EXACT" sourceClass="TRANSCRIPT" />}>
        transcript
      </Legend>
      <p className="text-3xs leading-snug text-text-muted">
        <span className="td-legend text-text-secondary">reasoning</span>{' '}
        <EvidenceGradeTag grade="UNAVAILABLE" className="align-middle" /> private chain-of-thought
        is not a persisted source class; only stored turns and retained summaries are shown.
      </p>
      <ReadSection
        title="Transcript"
        chrome="centered"
        state={envelopeReadState(session.isPending, session.data, {
          loading: 'reading transcript',
          transport: 'transcript could not be read',
        })}
      >
        {(envelope) => {
          const payload = envelope.payload;
          return payload.exists === false ? (
            <StateChip kind="unknown" detail="the session store holds no transcript under this id" />
          ) : (
            <SessionBody
              payload={payload}
              pageNumber={cursorStack.length + 1}
              onPreviousPage={() => {
                setCursorStack((stack) => stack.slice(0, -1));
                setPageRequest((request) => request + 1);
              }}
              onNextPage={(nextCursor) => {
                setCursorStack((stack) => [...stack, nextCursor]);
                setPageRequest((request) => request + 1);
              }}
              pageRequest={pageRequest}
            />
          );
        }}
      </ReadSection>
    </section>
  );
}

function SessionBody({
  payload,
  pageNumber,
  onPreviousPage,
  onNextPage,
  pageRequest,
}: {
  payload: LcmSessionPayloadV1;
  pageNumber: number;
  onPreviousPage: () => void;
  onNextPage: (cursor: string) => void;
  pageRequest: number;
}) {
  return (
    <div className="flex flex-col gap-4">
      <SessionCounts payload={payload} />
      <CompactionBoundaries payload={payload} />
      <RawMessages
        payload={payload}
        pageNumber={pageNumber}
        onPreviousPage={onPreviousPage}
        onNextPage={onNextPage}
        pageRequest={pageRequest}
      />
      <p className="td-value break-all text-3xs text-text-muted" title={payload.path}>
        {payload.storage_scope} · {payload.path}
      </p>
    </div>
  );
}

/**
 * The session's own totals, which are whole-session figures rather than page
 * figures, that distinction is stated, because the message list below shows
 * one page and the count above it does not.
 *
 * The compaction ratio is the one derived number here and it is labelled as a
 * derivation of the two counts printed beside it. It is withheld entirely when
 * the source-token count is zero: a ratio against a zero denominator is not a
 * number.
 */
function SessionCounts({ payload }: { payload: LcmSessionPayloadV1 }) {
  const { counts } = payload;
  const sourceTokens = counts.source_token_count;
  const summaryTokens = counts.summary_token_count;
  const compaction =
    sourceTokens != null && summaryTokens != null && sourceTokens > 0
      ? summaryTokens / sourceTokens
      : null;
  return (
    <div className="flex flex-col gap-2">
      <Legend>whole session</Legend>
      <div className="grid grid-cols-2 gap-2">
        <div className="td-raised border border-edge-subtle px-2.5 py-2">
          <Readout
            label="messages"
            size="sm"
            value={splitCount(counts.message_count).value}
            unit={splitCount(counts.message_count).unit}
            note="token counts shown per loaded message"
          />
        </div>
        <div className="td-raised border border-edge-subtle px-2.5 py-2">
          <Readout
            label="summary nodes"
            size="sm"
            value={splitCount(counts.summary_node_count).value}
            unit={splitCount(counts.summary_node_count).unit}
            note={
              summaryTokens != null && sourceTokens != null
                ? `${summaryTokens.toLocaleString()} of ${sourceTokens.toLocaleString()} source tokens`
                : 'token counts unavailable'
            }
          />
        </div>
      </div>
      {compaction != null ? (
        <p className="text-3xs leading-snug text-text-muted">
          Summaries hold {(compaction * 100).toFixed(1)}% of the source tokens they replaced , 
          derived from the two counts above, not a stored ratio.
        </p>
      ) : sourceTokens != null && summaryTokens != null ? (
        <p className="text-3xs leading-snug text-text-muted">
          No source tokens are recorded against this session&apos;s summaries, so no compaction
          ratio exists to report.
        </p>
      ) : (
        <p className="text-3xs leading-snug text-text-muted">
          Compaction token counts are unavailable, so no ratio exists to report.
        </p>
      )}
    </div>
  );
}

/** The compactor's cuts. Each node states the depth it sits at, the category
 * and source type it was built from, and the exact token exchange it made. A
 * summary is a persisted derived artifact, EXPLICIT, never the source text. */
function CompactionBoundaries({ payload }: { payload: LcmSessionPayloadV1 }) {
  const nodes = payload.summary_nodes;
  return (
    <div className="flex flex-col gap-1.5">
      <Legend
        trailing={
          <>
            <span className="shrink-0 text-3xs text-text-muted tabular">
              {nodes.length} of {payload.counts.summary_node_count.toLocaleString()}
            </span>
            <EvidenceGradeTag grade="EXPLICIT" sourceClass="RETAINED SUMMARY" />
          </>
        }
      >
        compaction boundaries
      </Legend>
      {nodes.length === 0 ? (
        <StateChip
          kind={payload.counts.summary_node_count === 0 ? 'complete_zero_findings' : 'partial'}
          detail={
            payload.counts.summary_node_count === 0
              ? 'the compactor has not cut this session'
              : 'this page carried no summary nodes'
          }
        />
      ) : (
        // Scrollable regions need keyboard operation (WCAG 2.1.1); every row is
        // read-out, so the list itself takes the tab stop and carries a name.
        <ol
          tabIndex={0}
          aria-label="Compaction boundaries"
          className="flex max-h-64 flex-col overflow-auto border border-edge-subtle"
        >
          {nodes.map((node) => (
            <SummaryNodeRow key={node.node_id} node={node} />
          ))}
        </ol>
      )}
      {payload.has_more_summary_nodes ? (
        <p className="text-3xs text-text-muted">
          The store holds more summary nodes than this page carries.
        </p>
      ) : null}
    </div>
  );
}

function SummaryNodeRow({ node }: { node: LcmSummaryNodeV1 }) {
  const sourceTokens = node.source_token_count;
  const summaryTokens = node.token_count;
  const retained =
    sourceTokens != null && summaryTokens != null && sourceTokens > 0
      ? summaryTokens / sourceTokens
      : null;
  return (
    <li
      className="flex flex-col gap-1 border-b border-edge-subtle px-2 py-1.5 last:border-b-0"
      data-summary-node={node.node_id}
      data-summary-depth={node.depth}
    >
      <span className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <span className="td-legend shrink-0 text-text-secondary">depth {node.depth}</span>
        <span className="min-w-0 truncate text-3xs text-text-primary">{node.category}</span>
        <span className="td-value ml-auto shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {summaryTokens != null ? summaryTokens.toLocaleString() : 'unavailable'} ←{' '}
          {sourceTokens != null ? sourceTokens.toLocaleString() : 'unavailable'} tokens
        </span>
      </span>
      {retained != null ? <Meter fraction={retained} height="row" className="w-full" /> : null}
      <span className="line-clamp-3 text-3xs leading-snug text-text-secondary">{node.summary}</span>
      <span className="text-3xs text-text-muted">
        {node.source_type} · built {formatStamp(node.created_at)}
        {node.latest_at != null ? ` · latest ${formatStamp(node.latest_at)}` : ''}
      </span>
      {/* The producer's own instruction for recovering what this node replaced,
        * rendered verbatim: the browser does not construct an expansion. */}
      <span className="td-value break-all text-3xs text-text-muted">{node.expand_hint}</span>
    </li>
  );
}

/** Token provenance across the loaded page, tallied from each message's own
 * provenance field, a page-level statement, never a session-level one. */
function pageProvenance(messages: readonly LcmMessageV1[]) {
  let counted = 0;
  let unavailable = 0;
  for (const message of messages) {
    if (message.token_count != null && message.token_count_provenance === 'o200k_approximate') {
      counted += 1;
    } else {
      unavailable += 1;
    }
  }
  return { counted, unavailable };
}

/** The raw turns, one server page at a time. */
function RawMessages({
  payload,
  pageNumber,
  onPreviousPage,
  onNextPage,
  pageRequest,
}: {
  payload: LcmSessionPayloadV1;
  pageNumber: number;
  onPreviousPage: () => void;
  onNextPage: (cursor: string) => void;
  pageRequest: number;
}) {
  const { messages, limit } = payload;
  const range = useRef<HTMLParagraphElement>(null);
  const provenance = pageProvenance(messages);

  /**
   * The requested page is on screen, so put focus back if paging lost it.
   * Focus lands on the range line rather than the first row, because the
   * range line is the answer a reader who just paged is holding, which page
   * am I on now, and it renders in every state, including an empty page.
   * Only when focus was actually orphaned.
   */
  useEffect(() => {
    if (pageRequest === 0) return;
    if (document.activeElement === document.body) range.current?.focus();
  }, [pageRequest]);

  return (
    <div className="flex flex-col gap-1.5">
      <Legend>raw messages</Legend>

      {/* Loaded page count, whole-session total, and whether another page
        * exists, all three, because any one alone lets a page read as the
        * transcript. A status region so paging announces where the reader now
        * is; `tabIndex={-1}` so the focus repair can land here without adding
        * a tab stop. */}
      <p ref={range} role="status" tabIndex={-1} className="text-3xs text-text-muted tabular">
        {messages.length} on this page · {payload.counts.message_count.toLocaleString()} in session ·
        page {pageNumber} · page size {limit}
        {payload.next_cursor != null ? ' · more pages follow' : ' · last page'}
      </p>
      {messages.length > 0 ? (
        <p className="text-3xs text-text-muted tabular" data-page-token-provenance>
          token provenance on this page: {provenance.counted} counted (o200k approximate) ·{' '}
          {provenance.unavailable} unavailable
        </p>
      ) : null}

      {messages.length === 0 ? (
        <StateChip
          kind={payload.counts.message_count === 0 ? 'complete_zero_findings' : 'partial'}
          detail={
            payload.counts.message_count === 0
              ? 'the store holds no turns for this session'
              : 'this page carried no turns'
          }
        />
      ) : (
        <ol
          tabIndex={0}
          aria-label="Raw messages"
          className="flex max-h-96 flex-col overflow-auto border border-edge-subtle"
        >
          {messages.map((message) => (
            <MessageRow key={message.message_id} message={message} />
          ))}
        </ol>
      )}

      <div className="flex items-center gap-2">
        <button
          type="button"
          className="td-hit group disabled:opacity-40"
          disabled={pageNumber === 1}
          onClick={onPreviousPage}
        >
          <span className={PAGER_BEZEL}>
            <ChevronLeft aria-hidden size={11} />
            Previous page
          </span>
        </button>
        <button
          type="button"
          className="td-hit group disabled:opacity-40"
          disabled={payload.next_cursor == null}
          onClick={() => {
            if (payload.next_cursor != null) onNextPage(payload.next_cursor);
          }}
        >
          <span className={PAGER_BEZEL}>
            Next page
            <ChevronRight aria-hidden size={11} />
          </span>
        </button>
      </div>
    </div>
  );
}

function MessageRow({ message }: { message: LcmMessageV1 }) {
  const compacted = message.summary_node_ids.length;
  const tokenLabel = tokenCountLabel(message.token_count, message.token_count_provenance);
  return (
    <li
      className="flex flex-col gap-1 border-b border-edge-subtle px-2 py-1.5 last:border-b-0"
      data-message={message.message_id}
      data-message-role={message.role ?? 'unrecorded'}
    >
      <span className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        {message.ordinal != null ? (
          <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
            #{message.ordinal}
          </span>
        ) : null}
        <span className="td-legend shrink-0 text-text-secondary">
          {message.role ?? 'role unrecorded'}
        </span>
        {message.tool_name ? (
          <span className="td-value min-w-0 truncate text-3xs text-text-primary">
            {message.tool_name}
          </span>
        ) : null}
        <span className="ml-auto shrink-0 text-3xs text-text-muted tabular">
          {message.timestamp != null ? formatStamp(message.timestamp) : 'no timestamp'}
        </span>
      </span>
      {message.content == null ? (
        // The turn exists; its body does not. Retention offloaded or dropped
        // it, and an empty line here would read as an empty message.
        <span className="text-3xs italic text-text-muted">
          body not held by the store{message.storage_kind ? ` (${message.storage_kind})` : ''}
        </span>
      ) : (
        <span className="line-clamp-4 whitespace-pre-wrap break-words text-3xs leading-snug text-text-secondary">
          {message.content}
        </span>
      )}
      <span className="flex flex-wrap gap-x-2 text-3xs text-text-muted">
        {message.source ? <span>{message.source}</span> : null}
        {message.storage_kind && message.content != null ? <span>{message.storage_kind}</span> : null}
        <span className="tabular">{tokenLabel ?? 'token count unavailable'}</span>
        {compacted > 0 ? (
          <span>
            in {compacted} {compacted === 1 ? 'summary' : 'summaries'}
          </span>
        ) : null}
        {message.pinned ? <span>pinned</span> : null}
      </span>
    </li>
  );
}

/* ------------------------------------------------------------------------ *
 * Git relations, each under its own daemon source status
 * ------------------------------------------------------------------------ */

function RelationsSection({ identity }: { identity: Identity }) {
  return (
    <section aria-label="Git relations" className="flex flex-col gap-2">
      <Legend>git relations</Legend>
      <RelationsBody identity={identity} />
    </section>
  );
}

function RelationsBody({ identity }: { identity: Identity }) {
  switch (identity.kind) {
    case 'blocked':
      return <StateChip kind={identity.state} detail="relations are read with the index page" />;
    case 'store_unavailable':
      return <StateChip kind="unknown" detail="session store not readable" />;
    case 'ambiguous':
      return (
        <StateChip
          kind="unavailable"
          detail="relations are keyed by provider and id; choose a provider above"
        />
      );
    case 'absent':
      return (
        <StateChip
          kind="unavailable"
          detail={`not loaded, relations are read with index page ${identity.page}`}
        />
      );
    case 'row':
      return (
        <Relations
          row={identity.row}
          relations={relationsFor(identity.payload, identity.row.provider, identity.row.session_id)}
        />
      );
    default:
      return assertNever(identity);
  }
}

function Relations({ row, relations }: { row: LoomSessionRowV1; relations: SessionRelations }) {
  const { commits, editedFiles, branchSpans, commitStatus, fileStatus, branchStatus } = relations;
  return (
    <div className="flex flex-col gap-3">
      <RelationGroup label="→ commits" status={commitStatus}>
        {commits.length > 0 ? (
          <ul className="flex flex-col gap-1.5">
            {commits.map((commit) => {
              const evidence = commitEvidence(commit);
              return (
                <li key={commit.commit_sha} className="flex flex-col gap-0.5" data-commit={commit.commit_sha}>
                  <span className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                    <span className="td-value min-w-0 truncate text-3xs text-text-primary" title={commit.commit_sha}>
                      {commit.commit_sha.slice(0, 12)}
                    </span>
                    {evidence ? (
                      <EvidenceGradeTag grade={evidence.grade} sourceClass={evidence.sourceClass} />
                    ) : (
                      <span className="text-3xs text-text-muted">grade unmapped · {commit.evidence}</span>
                    )}
                  </span>
                  <span className="text-3xs text-text-muted">
                    {commit.relation} · {commit.evidence}
                    {commit.span_overlap_kind ? ` · ${commit.span_overlap_kind}` : ''} ·{' '}
                    {formatStamp(commit.committed_at)}
                  </span>
                  <span className="truncate text-3xs text-text-muted">
                    {commit.branch ?? 'branch unrecorded'}
                    {commit.worktree ? ` · ${commit.worktree}` : ''}
                  </span>
                </li>
              );
            })}
          </ul>
        ) : (
          <SourceAbsence
            status={commitStatus}
            zero="no commit is attributed to this session"
            fallback="commit attribution coverage is unavailable"
          />
        )}
      </RelationGroup>

      <RelationGroup label="→ edited files" status={fileStatus}>
        {editedFiles.length > 0 ? (
          <ul className="flex flex-col gap-1">
            {editedFiles.map((file) => (
              <li key={`${file.path}:${file.change_type ?? ''}`} className="flex gap-2">
                <span className="min-w-0 flex-1 truncate text-3xs text-text-secondary" title={file.path}>
                  {file.path}
                </span>
                <span className="td-value shrink-0 text-3xs text-text-muted">
                  {file.change_type ?? 'change unrecorded'}
                  {file.hunks != null ? ` · ${file.hunks} ${file.hunks === 1 ? 'hunk' : 'hunks'}` : ''}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <StateChip
            kind={row.edited_files_recorded ? 'complete_zero_findings' : 'unknown'}
            detail={
              row.edited_files_recorded
                ? 'recorded edited-files rollup is empty'
                : 'this session has no recorded edited-files rollup'
            }
          />
        )}
      </RelationGroup>

      <RelationGroup label="→ branch & worktree spans" status={branchStatus}>
        {branchSpans.length > 0 ? (
          <ul className="flex flex-col gap-1">
            {branchSpans.map((span) => (
              <li key={`${span.worktree}:${span.first_at}`} className="flex flex-col">
                <span className="flex flex-wrap items-baseline gap-x-2">
                  <span className="truncate text-3xs text-text-secondary">
                    {span.branch ?? 'branch unrecorded'} · {span.worktree}
                  </span>
                  <EvidenceGradeTag grade="EXACT" sourceClass={span.source.toUpperCase()} />
                </span>
                <span className="text-3xs text-text-muted">
                  {formatStamp(span.first_at)} → {formatStamp(span.last_at)} ·{' '}
                  {formatDurationSeconds(span.last_at - span.first_at)} · {span.event_count}{' '}
                  {span.event_count === 1 ? 'event' : 'events'}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <SourceAbsence
            status={branchStatus}
            zero="no branch or worktree span is recorded for this session"
            fallback="branch/worktree span coverage is unavailable"
          />
        )}
      </RelationGroup>
    </div>
  );
}

/** A typed empty relation: a ready source with nothing for this session is a
 * measured zero; any other source state is that state, with its reason. */
function SourceAbsence({
  status,
  zero,
  fallback,
}: {
  status: LoomSourceStatusV1 | null;
  zero: string;
  fallback: string;
}) {
  if (status?.state === 'ready') return <StateChip kind="complete_zero_findings" detail={zero} />;
  return (
    <StateChip
      kind={status?.state ?? 'unknown'}
      detail={status?.reason ?? status?.coverage.reason ?? fallback}
    />
  );
}

function RelationGroup({
  label,
  status,
  children,
}: {
  label: string;
  status: LoomSourceStatusV1 | null;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1" data-relation-group={label}>
      <span className="flex flex-wrap items-baseline gap-x-2">
        <span className="td-legend text-text-secondary">{label}</span>
        {status ? (
          <span className="text-3xs text-text-muted">
            {status.label} · {status.state}
            {status.authority ? ` · ${status.authority}` : ''}
          </span>
        ) : (
          <span className="text-3xs text-text-muted">source status not served</span>
        )}
      </span>
      {children}
    </div>
  );
}
