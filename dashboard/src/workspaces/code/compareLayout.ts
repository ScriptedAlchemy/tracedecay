import type {
  RevisionPairChangeV1,
  RevisionPairFileRegionV1,
  RevisionPairSymbolRegionV1,
  RevisionPairUnionLayoutV1,
} from '../../contracts/generated.ts';

/**
 * COMPARE — two explicit local revisions in one union layout, served by
 * `GET /api/plugins/graph/compare/union-layout` (code_read_api.rs).
 *
 * The selection is four exact values: a base branch with the commit it is
 * expected to point at, and the same for head. The daemon resolves each branch
 * and refuses with `selected_revision_changed` when the reference has moved,
 * so a pasted link never silently compares a different commit. All four live in
 * the URL so the comparison is restorable and shareable.
 */
export interface CompareRevisionSelection {
  readonly branch: string;
  readonly revision: string;
}

export interface CompareSelection {
  readonly base: CompareRevisionSelection;
  readonly head: CompareRevisionSelection;
  /** Lens filters, passed through to the route's `file` / `kind` parameters. */
  readonly file: string;
  readonly kind: string;
}

const PARAMS = {
  base: 'base',
  baseRevision: 'base_revision',
  head: 'head',
  headRevision: 'head_revision',
  file: 'compare_file',
  kind: 'compare_kind',
} as const;

export const EMPTY_COMPARE_SELECTION: CompareSelection = {
  base: { branch: '', revision: '' },
  head: { branch: '', revision: '' },
  file: '',
  kind: '',
};

export function readCompareSelection(params: URLSearchParams): CompareSelection {
  return {
    base: {
      branch: params.get(PARAMS.base) ?? '',
      revision: params.get(PARAMS.baseRevision) ?? '',
    },
    head: {
      branch: params.get(PARAMS.head) ?? '',
      revision: params.get(PARAMS.headRevision) ?? '',
    },
    file: params.get(PARAMS.file) ?? '',
    kind: params.get(PARAMS.kind) ?? '',
  };
}

export function writeCompareSelection(
  current: URLSearchParams,
  selection: CompareSelection,
): URLSearchParams {
  const next = new URLSearchParams(current);
  const set = (key: string, value: string) => {
    if (value === '') next.delete(key);
    else next.set(key, value);
  };
  set(PARAMS.base, selection.base.branch);
  set(PARAMS.baseRevision, selection.base.revision);
  set(PARAMS.head, selection.head.branch);
  set(PARAMS.headRevision, selection.head.revision);
  set(PARAMS.file, selection.file);
  set(PARAMS.kind, selection.kind);
  return next;
}

/** Every one of the four identity fields is present. Filters are optional. */
export function isCompareSelectionComplete(selection: CompareSelection): boolean {
  return (
    selection.base.branch !== '' &&
    selection.base.revision !== '' &&
    selection.head.branch !== '' &&
    selection.head.revision !== ''
  );
}

export const COMPARE_UNION_LAYOUT_ROUTE = '/api/plugins/graph/compare/union-layout';

export function compareUnionLayoutUrl(selection: CompareSelection): string {
  const params = new URLSearchParams();
  params.set('base', selection.base.branch);
  params.set('base_revision', selection.base.revision);
  params.set('head', selection.head.branch);
  params.set('head_revision', selection.head.revision);
  if (selection.file !== '') params.set('file', selection.file);
  if (selection.kind !== '') params.set('kind', selection.kind);
  return `${COMPARE_UNION_LAYOUT_ROUTE}?${params.toString()}`;
}

/** A `refs/heads/<branch>` reference as the branch a person typed. Anything
 * that is not a local branch ref is returned unchanged. */
export function branchFromReference(reference: string): string {
  return reference.startsWith('refs/heads/') ? reference.slice('refs/heads/'.length) : reference;
}

/** The union layout's change vocabulary, with the geometry rule each one
 * follows in the rendering: identity order is the daemon's and is never
 * re-sorted here, so old landmarks stay where they were. */
export const COMPARE_CHANGE_RULES: Readonly<
  Record<RevisionPairChangeV1, { readonly label: string; readonly rule: string }>
> = {
  unchanged: { label: 'unchanged', rule: 'present in both revisions; the same identity' },
  changed: { label: 'changed', rule: 'present in both revisions; content differs' },
  added: { label: 'added', rule: 'head only; appears in identity order without moving neighbours' },
  removed: { label: 'removed', rule: 'base only; keeps its outlined former space' },
};

export interface CompareChangeCounts {
  readonly unchanged: number;
  readonly changed: number;
  readonly added: number;
  readonly removed: number;
}

export function countChanges(
  regions: ReadonlyArray<{ readonly change: RevisionPairChangeV1 }>,
): CompareChangeCounts {
  const counts = { unchanged: 0, changed: 0, added: 0, removed: 0 };
  for (const region of regions) counts[region.change] += 1;
  return counts;
}

/** Symbol regions grouped under the file identity they belong to, in the
 * daemon's file order. The producer (`code_reads.rs::revision_file_regions`)
 * refuses with a typed internal error if any symbol's file identity is not in
 * the file list, so every served symbol has a file region here. */
export function groupSymbolsByFile(
  layout: RevisionPairUnionLayoutV1,
): ReadonlyArray<{
  readonly file: RevisionPairFileRegionV1;
  readonly symbols: ReadonlyArray<RevisionPairSymbolRegionV1>;
}> {
  const byFile = new Map<string, RevisionPairSymbolRegionV1[]>();
  for (const symbol of layout.symbols) {
    const identity = (symbol.head ?? symbol.base)?.file_identity ?? symbol.symbol_identity;
    const bucket = byFile.get(identity);
    if (bucket) bucket.push(symbol);
    else byFile.set(identity, [symbol]);
  }
  return layout.files.map((file) => ({
    file,
    symbols: byFile.get(file.file_identity) ?? [],
  }));
}
