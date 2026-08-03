/**
 * INDEX FRESHNESS — `GET /api/code-index/freshness`.
 *
 * The branch-aware answer to "is the graph beside this panel current, and
 * current *for what*". Every symbol, edge and trace on this page was read from
 * one sealed code-index generation, and that generation was sealed against one
 * exact source reference — so a spine drawn from a generation sealed on
 * `refs/heads/main` while the checkout sits on a feature branch is stale in a
 * way no node count reveals.
 *
 * The route is honest by construction and this surface keeps it that way:
 *
 *   unsupported  the dashboard is not attached to a daemon-owned scheduler
 *                registry at all. There is no generation to report, and the
 *                panel says so instead of drawing a "fresh" badge.
 *   unknown      a registry is attached but has no mounted scheduler for this
 *                project, or has one that has not sealed a generation.
 *   loading      a mount exists and is indexing — a generation is coming.
 *   partial      a mount and a generation exist but the scheduler's own
 *                coverage of them is incomplete.
 *   ready        a complete, fresh generation with complete coverage.
 *
 * Those five are the server's states, not this component's: the envelope's
 * `domain_state` is rendered directly, and nothing here infers freshness from
 * the payload fields.
 */
import { useQuery } from '@tanstack/react-query';
import {
  CodeIndexFreshnessPayloadV1Schema,
  type CodeIndexFreshnessPayloadV1,
  type CodeIndexWorktreeFreshnessV1,
} from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { authorizationState } from '../../ui/EnvelopeTruth.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { elideStart, formatMicrosUtc } from '../../ui/format.ts';

export function IndexFreshness() {
  const scope = useScope((s) => s.scope);
  const freshness = useQuery({
    queryKey: ['code-index', 'freshness', scopeKey(scope)],
    queryFn: () =>
      fetchEnvelope(scopedUrl(scope, '/api/code-index/freshness'), CodeIndexFreshnessPayloadV1Schema),
    refetchInterval: 30_000,
  });

  return (
    <section className="flex flex-col gap-1.5" aria-label="Code index freshness">
      <div className="flex items-center gap-1.5">
        <h3 className="td-legend">index freshness</h3>
        <span aria-hidden className="td-rule" />
      </div>
      {freshness.isPending ? (
        <p className="text-2xs text-state-loading">reading scheduler state…</p>
      ) : freshness.data === undefined ? (
        <p className="text-2xs text-state-unknown">no response recorded</p>
      ) : (
        <FreshnessReading result={freshness.data} />
      )}
    </section>
  );
}

function FreshnessReading({ result }: { result: EnvelopeResult<CodeIndexFreshnessPayloadV1> }) {
  if (result.outcome === 'transport') {
    return (
      <div className="flex flex-col gap-1.5">
        <StateChip kind={result.state} detail={result.detail ?? 'daemon unreachable'} />
      </div>
    );
  }
  const { envelope } = result;
  const { worktrees, note } = envelope.payload;
  // Authorization is an independent axis from the read's own state: a mount can
  // be `ready` and separately `redacted` for the identity asking. Folding them
  // together loses which one the reader is actually blocked by.
  const authorization = authorizationState(envelope.authorization);
  return (
    <div className="flex flex-col gap-2" data-index-freshness={envelope.domain_state}>
      <StateChip kind={envelope.domain_state} />
      {authorization ? (
        <StateChip kind={authorization} detail="read authorization" />
      ) : null}
      {worktrees.map((worktree) => (
        <WorktreeReading key={worktree.worktree_root} worktree={worktree} />
      ))}
      {/* The route's own sentence for why the list is the length it is. It is
        * the only thing distinguishing "no scheduler is attached" from "a
        * scheduler is attached and this project is not mounted", and both
        * arrive as an empty array. */}
      <p className="text-3xs leading-snug text-text-muted">{note}</p>
    </div>
  );
}

/**
 * One mounted worktree.
 *
 * The source reference leads because it is the branch-aware part: it names what
 * the sealed generation is a picture *of*. Everything below it is identity and
 * timing for that same generation.
 */
function WorktreeReading({ worktree }: { worktree: CodeIndexWorktreeFreshnessV1 }) {
  return (
    <div
      className="td-raised flex flex-col gap-1.5 border border-edge-subtle px-2.5 py-2"
      data-worktree-staleness={worktree.staleness_state ?? 'unreported'}
    >
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="td-legend">source reference</span>
        <span
          className="td-value min-w-0 truncate text-2xs text-text-primary"
          title={worktree.source_reference ?? undefined}
        >
          {worktree.source_reference ?? 'not reported by the scheduler'}
        </span>
      </div>
      <dl className="flex flex-col gap-1 text-3xs leading-snug">
        <Row label="staleness">{worktree.staleness_state ?? 'not reported'}</Row>
        <Row label="coverage">{worktree.coverage}</Row>
        <Row label="generation" mono>
          {worktree.latest_generation_id ?? 'no sealed generation yet'}
        </Row>
        {worktree.snapshot_content_identity ? (
          <Row label="snapshot" mono>
            {worktree.snapshot_content_identity}
          </Row>
        ) : null}
        <Row label="sealed">{formatMicros(worktree.sealed_at_micros)}</Row>
        <Row label="reconciled">{formatMicros(worktree.last_reconcile_micros)}</Row>
        {/* A pending-hint count of zero is a real reading — the scheduler
          * counted and found none — so unlike the identity fields above it is
          * printed whenever the server sent a number, and omitted only when it
          * sent none. */}
        {worktree.hook_hint_count != null ? (
          <Row label="pending hints">{worktree.hook_hint_count.toLocaleString()}</Row>
        ) : null}
        {worktree.repository_id ? (
          <Row label="repository" mono>
            {worktree.repository_id}
          </Row>
        ) : null}
      </dl>
      <p
        className="td-value truncate text-3xs text-text-muted"
        title={worktree.worktree_root}
      >
        {elideStart(worktree.worktree_root, 40)}
      </p>
    </div>
  );
}

function Row({
  label,
  children,
  mono,
}: {
  label: string;
  children: string;
  mono?: boolean;
}) {
  return (
    <div className="flex min-w-0 items-baseline gap-1.5">
      <dt className="shrink-0 uppercase tracking-[0.08em] text-text-muted">{label}</dt>
      <dd
        className={`min-w-0 flex-1 truncate text-right text-text-secondary${mono ? ' td-value' : ''}`}
        title={children}
      >
        {children}
      </dd>
    </div>
  );
}

/** A stamp the scheduler did not report stays unreported. An absent time is not
 * the epoch, and printing `1970-01-01` for one would read as a real, very old
 * observation. */
function formatMicros(micros: number | null): string {
  return formatMicrosUtc(micros, { nullAs: 'not reported' });
}
