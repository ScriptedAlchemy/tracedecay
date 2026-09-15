export const CODE_VIEWS = [
  'atlas',
  'topology',
  'trace',
  'shared-code',
  'compare',
] as const;

export type CodeView = (typeof CODE_VIEWS)[number];
export type CodeFocusState = 'absent' | 'loading' | 'available' | 'unavailable';

export const CODE_VIEW_DEFINITIONS = {
  atlas: {
    label: 'Atlas',
    note: 'fixed repository geometry across structural lenses',
  },
  topology: {
    label: 'Topology',
    note: 'modules and symbols across exact and inferred relations',
  },
  trace: {
    label: 'Trace',
    note: 'callers and inputs through the selected symbol to callees and effects',
  },
  'shared-code': {
    label: 'Shared Code',
    note: 'exact and verified near-clone families',
  },
  compare: {
    label: 'Compare',
    note: 'two revisions in one stable union layout',
  },
} as const satisfies Record<CodeView, { label: string; note: string }>;

export interface CodeLocation {
  readonly view: CodeView;
  readonly focusId: string | null;
}

export interface CodeViewBlocker {
  readonly kind: 'loading' | 'unavailable';
  readonly title: string;
  readonly detail: string;
}

const VIEW_PARAM = 'view';
const FOCUS_PARAM = 'symbol';

export function readCodeLocation(params: URLSearchParams): CodeLocation {
  const focusId = params.get(FOCUS_PARAM);
  const requested = params.get(VIEW_PARAM);
  switch (requested) {
    case 'atlas':
    case 'trace':
    case 'shared-code':
    case 'compare':
      return { view: requested, focusId };
    default:
      return { view: 'topology', focusId };
  }
}

export function writeCodeLocation(
  current: URLSearchParams,
  location: CodeLocation,
): URLSearchParams {
  const next = new URLSearchParams(current);
  if (location.view === 'topology') next.delete(VIEW_PARAM);
  else next.set(VIEW_PARAM, location.view);
  if (location.focusId === null) next.delete(FOCUS_PARAM);
  else next.set(FOCUS_PARAM, location.focusId);
  return next;
}

export function codeViewBlocker(
  view: CodeView,
  focus: CodeFocusState,
): CodeViewBlocker | null {
  switch (view) {
    case 'atlas':
      return {
        kind: 'unavailable',
        title: 'Atlas is unavailable',
        detail: 'The fixed structural treemap projection is not mounted.',
      };
    case 'topology':
      return null;
    case 'trace': {
      switch (focus) {
        case 'available':
          return null;
        case 'loading':
          return {
            kind: 'loading',
            title: 'Resolving the selected symbol',
            detail: 'Trace will open after the graph resolves the URL identity.',
          };
        case 'unavailable':
          return {
            kind: 'unavailable',
            title: 'The selected symbol is unavailable',
            detail: 'The current graph generation did not resolve the URL identity.',
          };
        case 'absent':
          return {
            kind: 'unavailable',
            title: 'Trace needs a selected symbol',
            detail: 'Select a symbol in Topology, then return to Trace.',
          };
        default: {
          const unhandled: never = focus;
          return unhandled;
        }
      }
    }
    case 'shared-code':
      return {
        kind: 'unavailable',
        title: 'Shared Code is unavailable',
        detail: 'Exact and near-clone family projections are not mounted.',
      };
    case 'compare':
      return {
        kind: 'unavailable',
        title: 'Compare is unavailable',
        detail: 'Revision-pair identity and union-layout projections are not mounted.',
      };
    default: {
      const unhandled: never = view;
      return unhandled;
    }
  }
}
