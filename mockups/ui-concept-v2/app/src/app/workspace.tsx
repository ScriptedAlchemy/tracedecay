import { createContext, useCallback, useContext, useMemo, useSyncExternalStore, type Dispatch, type ReactNode, type SetStateAction } from 'react';

export type DemoMode = 'snapshot' | 'fixture';
type DemoContext = {
  mode: DemoMode;
  setMode: (mode: DemoMode) => void;
  navigate: (surface: string, params?: Record<string, string>) => void;
};

const WorkspaceContext = createContext<DemoContext | null>(null);
const remembered = new Map<string, unknown>();
const listeners = new Map<string, Set<() => void>>();

function matchesInitial(value: unknown, initial: unknown): boolean {
  if (initial === null) return true; // Nullable domain values are validated by their owner.
  if (typeof value !== typeof initial || value === null) return false;
  if (typeof initial === 'number') return Number.isFinite(value);
  if (Array.isArray(initial)) return Array.isArray(value) && (!initial.length || value.every(item => matchesInitial(item, initial[0])));
  if (typeof initial === 'object') return !Array.isArray(value) && Object.entries(initial).every(([key, field]) => Object.hasOwn(value as object, key) && matchesInitial((value as Record<string, unknown>)[key], field));
  return true;
}

function readRemembered<T>(key: string, initial: T): T {
  if (remembered.has(key)) return remembered.get(key) as T;
  try {
    const saved = sessionStorage.getItem(key);
    if (saved !== null) {
      const parsed: unknown = JSON.parse(saved);
      if (matchesInitial(parsed, initial)) {
        remembered.set(key, parsed);
        return parsed as T;
      }
    }
  } catch {
    // Camera/selection storage is optional; source evidence never comes from it.
  }
  remembered.set(key, initial);
  return initial;
}

function remember(key: string, value: unknown) {
  remembered.set(key, value);
  try { sessionStorage.setItem(key, JSON.stringify(value)); } catch {
    // Keep the current page usable in browsers that deny session storage.
  }
  for (const listener of listeners.get(key) ?? []) listener();
}

function currentMode(): DemoMode {
  const query = new URLSearchParams(location.search);
  return query.get('data') === 'fixture' || (!query.has('data') && query.get('loom_source') === 'design') ? 'fixture' : 'snapshot';
}

function currentSurface() {
  return new URLSearchParams(location.search).get('surface') ?? 'brain';
}

export function WorkspaceProvider({ children }: { children: ReactNode }) {
  const mode = currentMode();
  const navigate = useCallback((surface: string, params?: Record<string, string>) => {
    remember(`td:route:${mode}:${currentSurface()}`, location.pathname + location.search);
    const saved = readRemembered<string | null>(`td:route:${mode}:${surface}`, null);
    const url = new URL(typeof saved === 'string' ? saved : location.pathname, location.origin);
    url.searchParams.set('surface', surface);
    url.searchParams.set('data', mode);
    if (params) for (const [key, value] of Object.entries(params)) url.searchParams.set(key, value);
    if (surface === 'loom' && mode === 'fixture') url.searchParams.set('loom_source', 'design');
    location.assign(url.pathname + url.search);
  }, [mode]);
  const setMode = useCallback((next: DemoMode) => {
    if (next === mode) return;
    const url = new URL(location.href);
    // View choices can cross sources; PRs, branches, events and attention
    // identities belong to the source where they were selected.
    const viewKeys = new Set(['surface', 'state', 'view', 'dim', 'lens']);
    for (const key of [...url.searchParams.keys()]) {
      if (!viewKeys.has(key)) url.searchParams.delete(key);
    }
    url.searchParams.set('data', next);
    if (currentSurface() === 'loom') url.searchParams.set('loom_source', next === 'fixture' ? 'design' : 'mac');
    // Sources contain immutable exports selected at module load. Navigation
    // remounts those owners so a fixture cannot retain snapshot event identities.
    location.assign(url.pathname + url.search);
  }, [mode]);
  const value = useMemo(() => ({ mode, setMode, navigate }), [mode, setMode, navigate]);
  return <WorkspaceContext.Provider value={value}>{children}</WorkspaceContext.Provider>;
}

export function useDemo(): DemoContext {
  const context = useContext(WorkspaceContext);
  if (!context) throw new Error('WorkspaceProvider must wrap the dashboard');
  return context;
}

/** Per-source view memory; these values are never data or readiness authority. */
export function useWorkspaceState<T>(key: string, initial: T): [T, Dispatch<SetStateAction<T>>] {
  const { mode } = useDemo();
  const storageKey = `td:view:${mode}:${key}`;
  const subscribe = useCallback((listener: () => void) => {
    const group = listeners.get(storageKey) ?? new Set<() => void>();
    group.add(listener);
    listeners.set(storageKey, group);
    return () => { group.delete(listener); if (!group.size) listeners.delete(storageKey); };
  }, [storageKey]);
  const getSnapshot = useCallback(() => readRemembered(storageKey, initial), [storageKey]);
  const value = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const setValue = useCallback<Dispatch<SetStateAction<T>>>(action => {
    const prior = readRemembered(storageKey, initial);
    const next = typeof action === 'function' ? (action as (value: T) => T)(prior) : action;
    remember(storageKey, next);
  }, [storageKey]);
  return [value, setValue];
}

export type AttentionMark = { seen: boolean; acknowledged: boolean; snoozed: boolean; signature: string };
export const attentionSignature = (item: AttentionItem) => JSON.stringify([
  item.sourceRef, item.status, item.detail, item.observedAt, item.evidence,
  item.severity, item.owner, item.title, item.repository, item.target.surface,
  Object.entries(item.target.params).sort(([a], [b]) => a.localeCompare(b)),
]);
export function useAttentionMarks() { return useWorkspaceState<Record<string, AttentionMark>>('attention.marks', {}); }

export type AttentionItem = {
  id: string;
  title: string;
  detail: string;
  source: 'ci' | 'review' | 'diagnostic' | 'proximity' | 'workflow' | 'work' | 'configuration' | 'coverage';
  severity: 'error' | 'warning' | 'information';
  status: 'active' | 'resolved';
  owner: 'you' | 'agent' | 'external' | 'system' | 'unknown';
  evidence: 'exact' | 'explicit' | 'inferred' | 'unavailable';
  mode: DemoMode;
  observedAt: string | null;
  sourceRef: string;
  repository?: string;
  target: { surface: string; params: Record<string, string> };
};
