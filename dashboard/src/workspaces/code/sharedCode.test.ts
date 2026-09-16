import { describe, expect, it } from 'vitest';

import {
  SHARED_CODE_MATCH_CLASSES,
  describeOccurrence,
  readSharedCodeCoverage,
  sharedFamilyUrl,
  shortDigest,
} from './sharedCode.ts';

describe('shared-code family reads', () => {
  it('addresses the route by occurrence, class, limit, and cursor', () => {
    const url = new URL(sharedFamilyUrl('sym-9', 'rename_normalized_exact', null), 'http://d');
    expect(url.pathname).toBe('/api/plugins/graph/shared-code/family');
    expect(url.searchParams.get('symbol_occurrence_id')).toBe('sym-9');
    expect(url.searchParams.get('match_class')).toBe('rename_normalized_exact');
    expect(url.searchParams.get('limit')).toBe('100');
    expect(url.searchParams.has('cursor')).toBe(false);

    const paged = new URL(
      sharedFamilyUrl('sym-9', 'conservative_exact', 'cursor.page-2', 25),
      'http://d',
    );
    expect(paged.searchParams.get('cursor')).toBe('cursor.page-2');
    expect(paged.searchParams.get('limit')).toBe('25');
  });

  it('draws exactly the two served exact classes, each with its own stitch', () => {
    expect(SHARED_CODE_MATCH_CLASSES.map((definition) => definition.matchClass)).toEqual([
      'conservative_exact',
      'rename_normalized_exact',
    ]);
    expect(new Set(SHARED_CODE_MATCH_CLASSES.map((definition) => definition.stitch)).size).toBe(
      2,
    );
  });
});

describe('shared-code coverage wording', () => {
  it('passes complete and partial through as the daemon stated them', () => {
    expect(readSharedCodeCoverage({ status: 'complete' })).toEqual({ kind: 'complete' });
    expect(readSharedCodeCoverage({ status: 'partial' })).toMatchObject({ kind: 'partial' });
  });

  it('words an exclusion as an exclusion, naming the minimum, never as zero copies', () => {
    const tooSmall = readSharedCodeCoverage({ status: 'excluded_too_small', minimum_tokens: 30 });
    expect(tooSmall.kind).toBe('excluded');
    if (tooSmall.kind !== 'excluded') throw new Error('unreachable');
    expect(tooSmall.sentence).toMatch(/30-token minimum/);
    expect(tooSmall.sentence).toMatch(/not a finding of zero copies/);

    const incomplete = readSharedCodeCoverage({ status: 'excluded_incomplete_tokenization' });
    expect(incomplete.kind).toBe('excluded');
    if (incomplete.kind !== 'excluded') throw new Error('unreachable');
    expect(incomplete.sentence).toMatch(/not a finding of zero copies/);
  });
});

describe('shared-code identities', () => {
  it('shortens a digest for the page and keeps its algorithm prefix out of the way', () => {
    expect(shortDigest(`sha256:${'ab'.repeat(32)}`)).toBe('abababababab');
    expect(shortDigest('short')).toBe('short');
  });

  it('states an occurrence as its path and exact byte span, never a line number', () => {
    expect(
      describeOccurrence({
        symbol_occurrence_id: 'sym-1',
        project_id: 'p',
        repository_id: 'r',
        worktree_id: null,
        source_generation: 'g',
        snapshot_digest: 's',
        path: 'src/lib.rs',
        body_span: { start_byte: 1200, end_byte: 1942 },
      }),
    ).toBe('src/lib.rs · bytes 1,200–1,942');
  });
});
