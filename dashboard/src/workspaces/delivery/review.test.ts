import { describe, expect, it } from 'vitest';
import { HEAD_ALPHA, INBOX, OVERVIEW_ALPHA } from '../../test/deliveryFixtures.ts';
import {
  buildCheckRows,
  buildReviewLanes,
  compareHref,
  providerIdentity,
} from './review.ts';

const HEAD = { branch: 'refs/heads/feature/delivery', revision: HEAD_ALPHA };

function reviewItems() {
  return OVERVIEW_ALPHA.review_comments.state === 'ready'
    ? OVERVIEW_ALPHA.review_comments.value.items
    : [];
}

function checks() {
  return OVERVIEW_ALPHA.ci_checks.state === 'ready' ? OVERVIEW_ALPHA.ci_checks.value.items : [];
}

describe('compareHref', () => {
  it('prefills only the head half and the file; base stays for the reader', () => {
    const href = compareHref({ ...HEAD, file: 'src/ingest/retry.ts' });
    const params = new URLSearchParams(href.slice('/code?'.length));
    expect(href.startsWith('/code?')).toBe(true);
    expect(params.get('view')).toBe('compare');
    expect(params.get('head')).toBe('feature/delivery');
    expect(params.get('head_revision')).toBe(HEAD_ALPHA);
    expect(params.get('compare_file')).toBe('src/ingest/retry.ts');
    expect(params.get('base')).toBeNull();
    expect(params.get('base_revision')).toBeNull();
  });
});

describe('buildReviewLanes', () => {
  it('counts lifecycles from the latest observation and groups threads by path', () => {
    const lanes = buildReviewLanes(reviewItems(), HEAD, null);
    expect(lanes.counts).toEqual({ current: 1, outdated: 1, resolved: 0, edited: 0, deleted: 0 });
    expect(lanes.files.map((file) => file.path)).toEqual([
      'src/ingest/config.ts',
      'src/ingest/retry.ts',
    ]);
    expect(lanes.threads.every((thread) => thread.grade === 'exact')).toBe(true);
    expect(lanes.unobserved).toBe(0);
  });

  it('filters by lifecycle without changing the counts', () => {
    const lanes = buildReviewLanes(reviewItems(), HEAD, 'current');
    expect(lanes.counts.outdated).toBe(1);
    expect(lanes.threads.map((thread) => thread.id)).toEqual(['review.R1']);
  });

  it('keeps an item with no observation out of every lifecycle count', () => {
    const lanes = buildReviewLanes(
      [{ id: 'review.R9', label: 'R9', provider: 'github', pull_request_id: '42', comment_id: 'R9', observations: [] }],
      HEAD,
      null,
    );
    expect(lanes.unobserved).toBe(1);
    expect(Object.values(lanes.counts).every((count) => count === 0)).toBe(true);
  });
});

describe('buildCheckRows', () => {
  it('maps conclusions to typed states and pivots only annotated checks to Compare', () => {
    const rows = buildCheckRows(checks(), HEAD);
    expect(rows.map((row) => [row.check.id, row.status])).toEqual([
      ['check.integration', 'error'],
      ['check.unit', 'ready'],
    ]);
    expect(rows[0]!.compareHref).toContain('compare_file=src%2Fingest%2Fretry.ts');
    expect(rows[1]!.compareHref).toBeNull();
  });
});

describe('providerIdentity', () => {
  it('reads base, head and merge base from the last complete pull_request read only', () => {
    const identity = providerIdentity(INBOX.pull_requests[0]!.pull_request);
    expect(identity.head).toBe(HEAD_ALPHA);
    expect(identity.base).toBe('c'.repeat(40));
    expect(identity.mergeBase).toBe('d'.repeat(40));
    expect(identity.outcome).toBe('complete');
    expect(providerIdentity(INBOX.pull_requests[1]!.pull_request)).toEqual({
      base: null,
      head: null,
      mergeBase: null,
      fetchedAtMicros: null,
      outcome: null,
    });
    expect(providerIdentity(null).head).toBeNull();
  });
});
