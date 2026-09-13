import { describe, expect, it } from 'vitest';
import {
  fieldMatches,
  matchedFieldNames,
  matchWindow,
  queryTerms,
  segmentMatches,
} from './terms.ts';

describe('queryTerms', () => {
  it('keeps quoted phrases, deduplicates case-insensitively, and drops single characters', () => {
    expect(queryTerms(`graph "search result" GRAPH x 'exact path'`)).toEqual([
      'search result',
      'exact path',
      'graph',
    ]);
  });
});

describe('segmentMatches', () => {
  it('matches metacharacters literally and preserves the unmatched text', () => {
    expect(segmentMatches('Call foo() before foo.', ['foo()'])).toEqual([
      { text: 'Call ', hit: false },
      { text: 'foo()', hit: true },
      { text: ' before foo.', hit: false },
    ]);
  });
});

describe('field matching', () => {
  it('reports only named payload fields that visibly contain a term', () => {
    const row = {
      name: 'search_payload',
      file_path: 'src/dashboard/graph_service.rs',
      hidden: 'search',
    };
    expect(fieldMatches(row.name, ['search'])).toBe(true);
    expect(matchedFieldNames(row, ['name', 'file_path'], ['search'])).toEqual(['name']);
  });
});

describe('matchWindow', () => {
  it('normalizes whitespace and moves the window to the earliest visible match', () => {
    const text = `prefix ${'x'.repeat(100)}\nGraph result suffix`;
    const window = matchWindow(text, ['graph'], 40);
    expect(window.startsWith('…')).toBe(true);
    expect(window).toContain('Graph result suffix');
    expect(window).not.toContain('\n');
  });
});
