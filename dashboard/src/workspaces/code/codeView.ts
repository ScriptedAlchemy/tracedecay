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
    status: 'pending',
  },
  topology: {
    label: 'Topology',
    note: 'modules and symbols across exact and inferred relations',
    status: 'mounted',
  },
  trace: {
    label: 'Trace',
    note: 'callers and inputs through the selected symbol to callees and effects',
    status: 'mounted',
  },
  'shared-code': {
    label: 'Shared Code',
    note: 'verified exact and rename-normalized copies of the selected body',
    status: 'mounted',
  },
  compare: {
    label: 'Compare',
    note: 'two exact revisions in one identity-stable union layout',
    status: 'mounted',
  },
} as const satisfies Record<
  CodeView,
  { label: string; note: string; status: 'mounted' | 'pending' }
>;

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
const LEGACY_VIEW_PARAM = 'structureLens';
const LEGACY_FOCUS_PARAM = 'structureFocus';

export function readCodeLocation(params: URLSearchParams): CodeLocation {
  const focusId = params.get(FOCUS_PARAM) ?? params.get(LEGACY_FOCUS_PARAM);
  const requested = params.get(VIEW_PARAM);
  switch (requested) {
    case 'atlas':
    case 'trace':
    case 'shared-code':
    case 'compare':
      return { view: requested, focusId };
    default:
      if (requested !== null) return { view: 'topology', focusId };
  }
  switch (params.get(LEGACY_VIEW_PARAM)) {
    case 'trace':
    case 'core':
      return focusId === null
        ? { view: 'topology', focusId }
        : { view: 'trace', focusId };
    default:
      return { view: 'topology', focusId };
  }
}

export function writeCodeLocation(
  current: URLSearchParams,
  location: CodeLocation,
): URLSearchParams {
  const next = new URLSearchParams(current);
  next.delete(LEGACY_VIEW_PARAM);
  next.delete(LEGACY_FOCUS_PARAM);
  if (location.view === 'topology') next.delete(VIEW_PARAM);
  else next.set(VIEW_PARAM, location.view);
  if (location.focusId === null) next.delete(FOCUS_PARAM);
  else next.set(FOCUS_PARAM, location.focusId);
  return next;
}

/** Whether a view's body may open: `null` when it may, otherwise the state a
 * reader is told instead. Trace and Shared Code are both readings *of one
 * selected symbol occurrence*, so they share the focus gate; Compare carries
 * its own revision selection and Topology needs nothing. */
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
    case 'compare':
      return null;
    case 'trace':
    case 'shared-code':
      return focusBlocker(CODE_VIEW_DEFINITIONS[view].label, focus);
    default: {
      const unhandled: never = view;
      return unhandled;
    }
  }
}

/** Is the selected view one that needs a symbol occurrence before it opens? */
export function codeViewNeedsFocus(view: CodeView): boolean {
  return view === 'trace' || view === 'shared-code';
}

function focusBlocker(label: string, focus: CodeFocusState): CodeViewBlocker | null {
  switch (focus) {
    case 'available':
      return null;
    case 'loading':
      return {
        kind: 'loading',
        title: 'Resolving the selected symbol',
        detail: `${label} will open after the graph resolves the URL identity.`,
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
        title: `${label} needs a selected symbol`,
        detail: `Select a symbol in Topology, then return to ${label}.`,
      };
    default: {
      const unhandled: never = focus;
      return unhandled;
    }
  }
}
