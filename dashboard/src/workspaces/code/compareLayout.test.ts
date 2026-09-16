import { describe, expect, it } from 'vitest';

import type { RevisionPairUnionLayoutV1 } from '../../contracts/generated.ts';
import {
  EMPTY_COMPARE_SELECTION,
  branchFromReference,
  compareUnionLayoutUrl,
  countChanges,
  groupSymbolsByFile,
  isCompareSelectionComplete,
  readCompareSelection,
  writeCompareSelection,
} from './compareLayout.ts';

const SELECTION = {
  base: { branch: 'main', revision: '1'.repeat(40) },
  head: { branch: 'feature/x', revision: '2'.repeat(40) },
  file: 'src/',
  kind: 'function',
};

describe('Compare selection in the URL', () => {
  it('round-trips all four identity fields and both filters', () => {
    const params = writeCompareSelection(new URLSearchParams('view=compare&symbol=sym-0'), SELECTION);
    expect(params.get('view')).toBe('compare');
    expect(params.get('symbol')).toBe('sym-0');
    expect(readCompareSelection(params)).toEqual(SELECTION);
  });

  it('removes a cleared field instead of writing an empty value', () => {
    const params = writeCompareSelection(
      writeCompareSelection(new URLSearchParams(), SELECTION),
      { ...SELECTION, file: '', kind: '' },
    );
    expect(params.has('compare_file')).toBe(false);
    expect(params.has('compare_kind')).toBe(false);
    expect(readCompareSelection(new URLSearchParams())).toEqual(EMPTY_COMPARE_SELECTION);
  });

  it('is complete only when both branches and both revisions are named', () => {
    expect(isCompareSelectionComplete(SELECTION)).toBe(true);
    expect(isCompareSelectionComplete({ ...SELECTION, file: '', kind: '' })).toBe(true);
    expect(
      isCompareSelectionComplete({ ...SELECTION, head: { branch: 'feature/x', revision: '' } }),
    ).toBe(false);
    expect(isCompareSelectionComplete(EMPTY_COMPARE_SELECTION)).toBe(false);
  });

  it('addresses the route with the branch names and exact revisions, filters only when set', () => {
    const url = new URL(compareUnionLayoutUrl(SELECTION), 'http://d');
    expect(url.pathname).toBe('/api/plugins/graph/compare/union-layout');
    expect(url.searchParams.get('base')).toBe('main');
    expect(url.searchParams.get('base_revision')).toBe('1'.repeat(40));
    expect(url.searchParams.get('head')).toBe('feature/x');
    expect(url.searchParams.get('head_revision')).toBe('2'.repeat(40));
    expect(url.searchParams.get('file')).toBe('src/');
    expect(url.searchParams.get('kind')).toBe('function');

    const bare = new URL(compareUnionLayoutUrl({ ...SELECTION, file: '', kind: '' }), 'http://d');
    expect(bare.searchParams.has('file')).toBe(false);
    expect(bare.searchParams.has('kind')).toBe(false);
  });

  it('turns a local branch reference into the branch a person types, and nothing else', () => {
    expect(branchFromReference('refs/heads/feature/x')).toBe('feature/x');
    expect(branchFromReference('refs/tags/v1')).toBe('refs/tags/v1');
  });
});

function layout(): RevisionPairUnionLayoutV1 {
  const revision = (side: 'base' | 'head') => ({
    reference: `refs/heads/${side}`,
    revision: (side === 'base' ? '1' : '2').repeat(40),
    tree: (side === 'base' ? '3' : '4').repeat(40),
    generation: `generation.${side}`,
  });
  const file = (i: number) => ({
    file_occurrence_id: `file.${i}`,
    path: `src/${i}.rs`,
    content_digest: `sha256:${String(i).repeat(64).slice(0, 64)}`,
    disposition: 'present' as const,
    symbol_identities: [`symbol-${i}`],
  });
  const symbol = (i: number, content: string) => ({
    symbol_occurrence_id: `sym-${i}`,
    file_identity: `file-identity-${i}`,
    file_occurrence_id: `file.${i}`,
    qualified_name: `crate::s${i}`,
    name: `s${i}`,
    kind: 'function',
    file: `src/${i}.rs`,
    content_digest: content,
  });
  return {
    base: revision('base'),
    head: revision('head'),
    files: [
      { file_identity: 'file-identity-0', change: 'unchanged', base: file(0), head: file(0) },
      { file_identity: 'file-identity-1', change: 'removed', base: file(1), head: null },
    ],
    symbols: [
      { symbol_identity: 'symbol-0', change: 'unchanged', base: symbol(0, 'a'), head: symbol(0, 'a') },
      { symbol_identity: 'symbol-1', change: 'removed', base: symbol(1, 'b'), head: null },
    ],
  };
}

describe('Compare union layout readings', () => {
  it('counts each change class of a region list', () => {
    expect(countChanges(layout().files)).toEqual({ unchanged: 1, changed: 0, added: 0, removed: 1 });
    expect(countChanges(layout().symbols)).toEqual({ unchanged: 1, changed: 0, added: 0, removed: 1 });
  });

  it('groups symbols under their file in the daemon order', () => {
    const groups = groupSymbolsByFile(layout());
    expect(groups.map((group) => group.file.file_identity)).toEqual([
      'file-identity-0',
      'file-identity-1',
    ]);
    expect(groups[0]!.file.change).toBe('unchanged');
    expect(groups[0]!.symbols.map((symbol) => symbol.symbol_identity)).toEqual(['symbol-0']);
    expect(groups[1]!.symbols.map((symbol) => symbol.symbol_identity)).toEqual(['symbol-1']);
  });
});
