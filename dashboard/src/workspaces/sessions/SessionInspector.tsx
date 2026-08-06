/**
 * SESSION DRILL-DOWN — `GET /api/plugins/hermes-lcm/session/{session_id}`.
 *
 * The transcript itself, not a rollup of it. Loom's thread chain answers "what
 * shape did this session have"; this answers "what was actually said, in what
 * order, and where did the compactor cut".
 *
 * Three things this surface exists to keep separate, which a summary view
 * cannot:
 *
 *   raw messages       the stored turns, in the store's own order, with the
 *                      provider, tool, storage kind and token estimate the
 *                      store recorded for each.
 *   summary nodes      the LCM compaction boundaries. Each one names the span
 *                      of source tokens it replaced and the token count it
 *                      replaced them with, so a compacted region is visible as
 *                      a boundary rather than silently absent from the
 *                      transcript.
 *   the page           `limit` and the server's opaque continuation cursor
 *                      bound one transcript page. What is on screen is never
 *                      presented as the whole session when another cursor is
 *                      available.
 *
 * A message whose `content` is null is not an empty message: the store holds
 * the turn but not its body (offloaded or dropped by retention). That is said
 * outright rather than rendered as a blank line.
 */
import { useEffect, useRef, useState } from 'react';
import { ChevronLeft, ChevronRight } from 'lucide-react';
import {
  LcmSessionPayloadV1Schema,
  type LcmMessageV1,
  type LcmSessionPayloadV1,
  type LcmSummaryNodeV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { useScope } from '../../data/scope/store.ts';
import { InspectorPanel } from '../../ui/archetypes/ExplorerSplit.tsx';
import { ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip';
import { Legend, Meter, Readout } from '../../ui/instrument.tsx';
import { formatStamp, splitCount } from '../../ui/format.ts';

/** One page of transcript. The plan's server-page default; the route caps at
 * 1000 and this stays well inside it so an inspector open never pulls a whole
 * corpus into the browser. */
const PAGE_SIZE = 100;

/** The pager's visible bezel. It stays 24px tall — it annotates a message
 * range rather than heading the panel — and `.td-hit` on the button around it
 * supplies the 44px target. */
const PAGER_BEZEL =
  'inline-flex items-center gap-1 border border-edge-subtle bg-surface-2 px-2 py-1 text-3xs text-text-secondary group-hover:text-text-primary';

export function SessionInspector({
  sessionId,
  onClose,
}: {
  sessionId: string;
  onClose: () => void;
}) {
  const scopeKey = useScope((state) =>
    state.scope.kind === 'all' ? 'all' : `project:${state.scope.projectId}`,
  );
  return (
    <SessionInspectorPage
      key={`${scopeKey}:${sessionId}`}
      sessionId={sessionId}
      onClose={onClose}
    />
  );
}

function SessionInspectorPage({
  sessionId,
  onClose,
}: {
  sessionId: string;
  onClose: () => void;
}) {
  /** The current cursor is the top entry; the rest lets Previous replay the
   * exact opaque cursor the server issued for the preceding page. */
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const cursor = cursorStack.at(-1) ?? null;
  /**
   * The page a reader asked for, held until it arrives on screen.
   *
   * It lives up here because the transcript below does not survive the trip: a
   * new cursor is a new query, so the read boundary swings to its
   * loading state and unmounts the whole page of rows — including the control
   * that was just activated. Focus goes to the document, and a keyboard user is
   * returned to the top of the app with no indication that anything moved. A
   * flag inside the unmounted subtree would be reinitialised by the remount and
   * could not repair it.
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
    <InspectorPanel title="Session transcript" onClose={onClose}>
      <div className="flex flex-col gap-3">
        <p className="td-value break-all text-3xs text-text-muted">{sessionId}</p>
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
            return (
            payload.exists === false ? (
              <StateChip
                kind="unknown"
                detail="the session store holds no transcript under this id"
              />
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
            )
            );
          }}
        </ReadSection>
      </div>
    </InspectorPanel>
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
 * The session's own totals, which are whole-session figures rather than
 * page figures — that distinction is stated, because the message list below
 * shows one page and the count above it does not.
 *
 * The compaction ratio is the one derived number here and it is labelled as a
 * derivation of the two counts printed beside it. It is withheld entirely when
 * the source-token count is zero, because a ratio against a zero denominator is
 * not a small number, it is not a number.
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
            note={
              counts.token_estimate_total != null
                ? `~${counts.token_estimate_total.toLocaleString()} est. tokens`
                : 'token estimate unavailable'
            }
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
          Summaries hold {(compaction * 100).toFixed(1)}% of the source tokens they replaced —
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
 * and source type it was built from, and the exact token exchange it made. */
function CompactionBoundaries({ payload }: { payload: LcmSessionPayloadV1 }) {
  const nodes = payload.summary_nodes;
  return (
    <div className="flex flex-col gap-1.5">
      <Legend
        trailing={
          <span className="shrink-0 text-3xs text-text-muted tabular">
            {nodes.length} of {payload.counts.summary_node_count.toLocaleString()}
          </span>
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
        // Scrollable regions need keyboard operation (WCAG 2.1.1). Every row
        // here is read-out — there is nothing inside to tab to — so the list
        // itself takes the tab stop, and it is named because a tab stop that
        // announces nothing tells a keyboard user only that they have arrived
        // somewhere.
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
      {retained != null ? (
        <Meter fraction={retained} height="row" className="w-full" />
      ) : null}
      <span className="line-clamp-3 text-3xs leading-snug text-text-secondary">
        {node.summary}
      </span>
      <span className="text-3xs text-text-muted">
        {node.source_type} · built {formatStamp(node.created_at)}
        {node.latest_at != null ? ` · latest ${formatStamp(node.latest_at)}` : ''}
      </span>
      {/* The producer's own instruction for recovering what this node replaced.
        * Rendered verbatim: the browser does not construct an expansion. */}
      <span className="td-value break-all text-3xs text-text-muted">{node.expand_hint}</span>
    </li>
  );
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

  /**
   * The requested page is on screen, so put focus back if paging lost it.
   *
   * Focus lands on the range line rather than the first row, because the range
   * line is the answer to the question a reader who just paged is holding —
   * which page am I on now — and it is the one element here that renders in
   * every state, including a page that contains no turns.
   *
   * Only when focus was actually orphaned. A reader who paged with the mouse
   * and is now looking somewhere else keeps what they had.
   */
  useEffect(() => {
    if (pageRequest === 0) return;
    if (document.activeElement === document.body) range.current?.focus();
  }, [pageRequest]);

  return (
    <div className="flex flex-col gap-1.5">
      <Legend>raw messages</Legend>

      {/* Loaded page count, whole-session total, and whether another page
        * exists — all three, because any one of them alone lets a page read as
        * the transcript.
        *
        * A status region, so paging announces where the reader now is instead
        * of silently replacing the rows under them; `tabIndex={-1}` so the
        * focus repair above can land here without adding a tab stop. */}
      <p
        ref={range}
        role="status"
        tabIndex={-1}
        className="text-3xs text-text-muted tabular"
      >
        {messages.length} on this page · {payload.counts.message_count.toLocaleString()} in session ·
        page {pageNumber} · page size {limit}
        {payload.next_cursor != null ? ' · more pages follow' : ' · last page'}
      </p>

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
        // Keyboard-operable for the same reason as the boundary list above: the
        // rows are read-out, so the scroll container itself is the tab stop.
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
  return (
    <li
      className="flex flex-col gap-1 border-b border-edge-subtle px-2 py-1.5 last:border-b-0"
      data-message={message.message_id}
      data-message-role={message.role ?? 'unrecorded'}
    >
      <span className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        {message.ordinal != null ? (
          <span
            className="td-value shrink-0 text-3xs text-text-muted"
            data-cell="numeric"
          >
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
        {message.storage_kind && message.content != null ? (
          <span>{message.storage_kind}</span>
        ) : null}
        {message.token_estimate != null ? (
          <span className="tabular">~{message.token_estimate.toLocaleString()} tokens</span>
        ) : null}
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
