/**
 * KNOWLEDGE CURATION — the deterministic curator's own reading of the fact
 * store, on two routes:
 *
 *   `/curation/status`  what the similarity-dedup curator has actually done:
 *                       apply-run count and the last run's own summary line.
 *   `/curation/plan`    what it would propose right now: dedup deletes for
 *                       near-duplicate pairs, plus hygiene candidates —
 *                       secret-like, transient, and possible-supersession
 *                       pairs, where a negation/state-change cue suggests a
 *                       newer fact contradicts and supersedes an older one.
 *
 * Every plan entry is a candidate for review (`review_required`), not a
 * decision. This surface proposes nothing of its own and applies nothing; the
 * apply journey stays behind `/curate/apply` with explicit ops. A failed plan
 * computation renders as its error sentence, never as a clean empty plan.
 */
import { z } from 'zod';
import { LegacyBoundary } from '../../ui/ReadSection.tsx';
import { useLegacy } from '../../data/query/useLegacy.ts';

const BASE = '/api/plugins/holographic';

const StatusSchema = z
  .object({
    state: z
      .object({
        paused: z.boolean(),
        run_count: z.number(),
        last_run_at: z.unknown().nullable(),
        last_run_summary: z.string().nullable(),
      })
      .passthrough(),
  })
  .passthrough();

const CandidateSchema = z
  .object({
    fact_id: z.number(),
    reason: z.string(),
    content: z.string().nullable().optional(),
    similarity: z.number().optional(),
    superseded_by: z.number().optional(),
    review_required: z.boolean().optional(),
  })
  .passthrough();

const PlanSchema = z
  .object({
    actions: z.array(CandidateSchema),
    hygiene: z
      .object({
        secret_like: z.array(CandidateSchema),
        transient: z.array(CandidateSchema),
        supersession: z.array(CandidateSchema),
      })
      .passthrough()
      .nullable(),
    total_facts: z.number(),
    error: z.string(),
  })
  .passthrough();

/** How many supersession pairs print in full; the count states the rest. */
const SHOWN = 5;

export function KnowledgeCuration() {
  const status = useLegacy(
    ['memory', 'curation', 'status'],
    `${BASE}/curation/status`,
    StatusSchema,
  );
  // The plan folds fact vectors into similarity pairs; the daemon caches the
  // computation against the store fingerprint, and once per visit is enough.
  const plan = useLegacy(['memory', 'curation', 'plan'], `${BASE}/curation/plan`, PlanSchema, {
    staleTime: 5 * 60_000,
  });
  return (
    <section className="flex flex-col gap-2 border-t border-edge-subtle px-3 py-3" aria-label="Memory curation">
      <h2 className="td-legend">curation</h2>
      <LegacyBoundary title="Curator" pending={status.isPending} result={status.data}>
        {(data) => (
          <p className="text-2xs leading-relaxed text-text-secondary">
            {data.state.run_count === 0
              ? 'the similarity curator has never applied a run against this store'
              : `${data.state.run_count.toLocaleString()} apply ${
                  data.state.run_count === 1 ? 'run' : 'runs'
                } recorded${
                  data.state.last_run_summary ? ` · last: ${data.state.last_run_summary}` : ''
                }`}
            {data.state.paused ? ' · paused' : ''}
          </p>
        )}
      </LegacyBoundary>
      <LegacyBoundary title="Curation plan" pending={plan.isPending} result={plan.data}>
        {(data) => <PlanBody data={data} />}
      </LegacyBoundary>
    </section>
  );
}

function PlanBody({ data }: { data: z.infer<typeof PlanSchema> }) {
  if (data.error !== '') {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        curation plan could not be computed: {data.error}
      </p>
    );
  }
  const hygiene = data.hygiene;
  const supersession = hygiene?.supersession ?? [];
  const flagged =
    data.actions.length +
    (hygiene ? hygiene.secret_like.length + hygiene.transient.length : 0) +
    supersession.length;
  if (flagged === 0) {
    return (
      <p className="text-2xs text-text-muted">
        nothing proposed across {data.total_facts.toLocaleString()} facts — no near-duplicate,
        secret-like, transient, or superseded candidates
      </p>
    );
  }
  const shown = supersession.slice(0, SHOWN);
  return (
    <div className="flex flex-col gap-2">
      <dl className="grid grid-cols-2 gap-x-3 gap-y-0.5 text-2xs">
        <PlanFigure label="dedup deletes" value={data.actions.length} />
        <PlanFigure label="supersession" value={supersession.length} />
        <PlanFigure label="secret-like" value={hygiene?.secret_like.length ?? 0} />
        <PlanFigure label="transient" value={hygiene?.transient.length ?? 0} />
      </dl>
      {shown.length > 0 ? (
        <ul className="flex flex-col gap-1.5" aria-label="Possible supersessions">
          {shown.map((row) => (
            <li key={row.fact_id} className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2">
              <p className="text-3xs text-text-muted">
                fact #{row.fact_id}
                {row.superseded_by != null ? ` · possibly superseded by #${row.superseded_by}` : ''}
                {row.similarity != null ? ` · similarity ${row.similarity.toFixed(4)}` : ''}
              </p>
              {row.content ? (
                <p className="text-2xs leading-relaxed text-text-secondary">{row.content}</p>
              ) : null}
            </li>
          ))}
          {supersession.length > shown.length ? (
            <li className="text-3xs leading-relaxed text-text-muted">
              {(supersession.length - shown.length).toLocaleString()} more supersession
              candidates in the plan
            </li>
          ) : null}
        </ul>
      ) : null}
      <p className="text-3xs leading-relaxed text-text-muted">
        candidates for review, not decisions — applying stays an explicit operation
      </p>
    </div>
  );
}

function PlanFigure({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="td-legend">{label}</dt>
      <dd className="tabular text-2xs">{value.toLocaleString()}</dd>
    </div>
  );
}
