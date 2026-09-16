import type {
  CodeCloneIndexCoverageV1,
  CodeCloneIndexObservationV1,
  CodeCloneIndexStatusV1,
  CodeIndexFreshnessPayloadV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { Field } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import {
  cloneIndexDetail,
  cloneIndexObservation,
  cloneIndexState,
  formatCloneDuration,
} from './cloneIndexModel.ts';
import { formatBytes } from './storageModel.ts';

const countFormat = new Intl.NumberFormat('en-US');

export function CloneIndexStatus({
  result,
  pending,
}: {
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined;
  pending: boolean;
}) {
  if (pending) {
    return (
      <section className="mx-4 mt-3" aria-label="Clone index">
        <p className="text-2xs text-text-muted">reading clone-index coverage…</p>
      </section>
    );
  }
  if (result?.outcome === 'transport') {
    return (
      <section className="mx-4 mt-3" aria-label="Clone index">
        <StateChip kind={result.state} detail={result.detail ?? 'clone-index status unavailable'} />
      </section>
    );
  }
  const worktrees = result?.outcome === 'envelope' ? result.envelope.payload.worktrees : [];
  return (
    <section
      className="mx-4 mt-3 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-3"
      aria-label="Clone index"
    >
      <h2 className="td-legend">Clone index</h2>
      {worktrees.length === 0 ? (
        <p className="mt-2 text-2xs text-text-muted">
          {result?.outcome === 'envelope'
            ? result.envelope.payload.note
            : 'clone-index status is unavailable'}
        </p>
      ) : (
        <div className="mt-2 flex flex-col gap-2">
          {worktrees.map((worktree) => (
            <CloneIndexCard
              key={worktree.worktree_root}
              status={
                worktree.clone_index ?? {
                  state: 'unavailable',
                  reason: 'clone-index status is absent from this freshness response',
                }
              }
              worktree={worktree.worktree_root}
            />
          ))}
        </div>
      )}
    </section>
  );
}

function CloneIndexCard({
  status,
  worktree,
}: {
  status: CodeCloneIndexStatusV1;
  worktree: string;
}) {
  const observation = cloneIndexObservation(status);
  return (
    <article
      className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2.5"
      data-clone-index-state={status.state}
    >
      <div className="flex flex-wrap items-center gap-2">
        <StateChip kind={cloneIndexState(status)} />
        <span className="text-2xs text-text-secondary">{cloneIndexDetail(status)}</span>
      </div>
      <p className="mt-1 truncate font-mono text-3xs text-text-muted" title={worktree}>
        {worktree}
      </p>
      {observation ? <CloneIndexObservation observation={observation} status={status} /> : null}
    </article>
  );
}

function CloneIndexObservation({
  observation,
  status,
}: {
  observation: CodeCloneIndexObservationV1;
  status: CodeCloneIndexStatusV1;
}) {
  const coverage = observation.coverage;
  const resources = observation.resources;
  return (
    <>
      {status.state === 'backfilling' ? (
        <div className="mt-2">
          <p className="text-3xs text-text-muted">
            {figure(coverage.completed_source_pages)} / {figure(coverage.total_source_pages)} sealed
            pages
          </p>
          <progress
            aria-label={`Clone backfill for ${observation.generation_id}`}
            className="mt-1 h-1.5 w-full accent-accent"
            max={Math.max(coverage.total_source_pages, 1)}
            value={coverage.completed_source_pages}
          />
        </div>
      ) : null}
      <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-3xs leading-snug sm:grid-cols-4">
        <Field label="eligible bodies">
          {ratio(coverage.eligible_source_bodies, coverage.source_bodies)}
        </Field>
        <Field label="conservative">
          {ratio(coverage.conservative_normalized_bodies, coverage.eligible_source_bodies)}
        </Field>
        <Field label="rename-normalized">
          {ratio(coverage.rename_normalized_bodies, coverage.eligible_source_bodies)}
        </Field>
        <Field label="prior payload reuse">{figure(coverage.payloads_reused)} occurrences</Field>
        <Field label="unique payloads">{figure(coverage.unique_payloads)}</Field>
        <Field label="exact postings">{figure(coverage.exact_postings)}</Field>
        <Field label="near fingerprints">
          {ratio(coverage.near_fingerprint_bodies, coverage.eligible_source_bodies)} ·{' '}
          {figure(coverage.near_fingerprint_postings)} postings
        </Field>
        <Field label="hot postings skipped">
          {figure(coverage.hot_postings_skipped)} lists ·{' '}
          {figure(coverage.hot_posting_rows_skipped)} rows
        </Field>
        <Field label="coverage exclusions">{coverageExclusionSummary(coverage)}</Field>
        <Field label="rename limitations">{renameLimitationSummary(coverage)}</Field>
      </dl>
      <div className="mt-2 grid gap-2 text-3xs sm:grid-cols-2">
        <section className="rounded-[var(--radius-chip)] bg-surface-1 p-2">
          <h3 className="font-medium text-text-secondary">Candidate and verification budgets</h3>
          <p className="mt-1 text-text-muted">
            {figure(observation.budgets.posting_rows)} posting rows ·{' '}
            {figure(observation.budgets.candidate_bodies)} candidates ·{' '}
            {figure(observation.budgets.verification_bodies)} body comparisons ·{' '}
            {figure(observation.budgets.verification_token_work)} token work
          </p>
          <p className="mt-1 text-text-muted">
            hot posting above {figure(observation.budgets.hot_posting_rows)} rows · minimum{' '}
            {figure(observation.budgets.minimum_body_tokens)} tokens
          </p>
          <p className="mt-1 text-text-muted">
            minimum directional coverage ·{' '}
            {formatMillionths(observation.budgets.minimum_directional_coverage_millionths)}
          </p>
        </section>
        <section className="rounded-[var(--radius-chip)] bg-surface-1 p-2">
          <h3 className="font-medium text-text-secondary">Resources and update</h3>
          <p className="mt-1 text-text-muted">
            {resources.bytes_on_disk == null
              ? 'disk bytes unavailable'
              : `${formatBytes(resources.bytes_on_disk)} on disk`}{' '}
            ·{' '}
            {resources.peak_scratch_memory_bytes == null
              ? 'scratch unavailable'
              : `${formatBytes(resources.peak_scratch_memory_bytes)} peak scratch`}
          </p>
          <p className="mt-1 text-text-muted">
            changed-symbol update {formatCloneDuration(resources.changed_symbol_update_micros)} ·{' '}
            {figure(resources.stale_invalidations)} stale invalidations
          </p>
        </section>
      </div>
      <p className="mt-2 truncate font-mono text-3xs text-text-muted" title={observation.generation_id}>
        {observation.generation_id} · artifact v
        {observation.artifact_format_revision ?? 'unknown'} · normalization{' '}
        {observation.conservative_normalization_revision}/
        {observation.rename_normalization_revision}
      </p>
    </>
  );
}

function figure(value: number | null | undefined): string {
  return value == null ? 'unknown' : countFormat.format(value);
}

function ratio(
  numerator: number | null | undefined,
  denominator: number | null | undefined,
): string {
  return `${figure(numerator)} / ${figure(denominator)}`;
}

function coverageExclusionSummary(coverage: CodeCloneIndexCoverageV1): string {
  const values = [
    ['too small', coverage.excluded_too_small_bodies],
    ['tokenization', coverage.excluded_incomplete_tokenization_bodies],
  ] as const;
  return values.map(([label, value]) => `${figure(value)} ${label}`).join(' · ');
}

function renameLimitationSummary(coverage: CodeCloneIndexCoverageV1): string {
  const values = [
    ['rename partial', coverage.rename_partial_bodies],
    ['rename unsupported', coverage.rename_unsupported_bodies],
  ] as const;
  return values.map(([label, value]) => `${figure(value)} ${label}`).join(' · ');
}

function formatMillionths(value: number): string {
  return `${(value / 10_000).toFixed(1)}%`;
}
