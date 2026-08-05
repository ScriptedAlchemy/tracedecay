import { create } from 'zustand';
import { z } from 'zod';

const InspectorReferenceSchema = z.object({
  kind: z.string().min(1),
  id: z.string().min(1),
});

const InspectorScopeSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('all') }),
  z.object({ kind: z.literal('project'), project_id: z.string().min(1) }),
]);

const InspectorEntrySchema = z.object({
  scope: InspectorScopeSchema,
  entity: InspectorReferenceSchema,
  evidence: InspectorReferenceSchema.optional(),
});

export type InspectorEntry = z.infer<typeof InspectorEntrySchema>;

const MAX_INSPECTOR_ENTRIES = 4;

export function serializeInspectorEntry(entry: InspectorEntry): string {
  return JSON.stringify(entry);
}

export function parseInspectorEntry(value: string): InspectorEntry | null {
  let decoded: unknown;
  try {
    decoded = JSON.parse(value);
  } catch {
    return null;
  }
  const parsed = InspectorEntrySchema.safeParse(decoded);
  return parsed.success ? parsed.data : null;
}

export function inspectorEntryKey(entry: InspectorEntry): string {
  return serializeInspectorEntry(entry);
}

function bounded(entries: readonly InspectorEntry[]): readonly InspectorEntry[] {
  const byIdentity = new Map<string, InspectorEntry>();
  for (const entry of entries) {
    const key = inspectorEntryKey(entry);
    byIdentity.delete(key);
    byIdentity.set(key, entry);
  }
  return [...byIdentity.values()].slice(-MAX_INSPECTOR_ENTRIES);
}

interface InspectorStackState {
  readonly entries: readonly InspectorEntry[];
  readonly activeKey: string | null;
  replace: (entries: readonly InspectorEntry[], activeKey?: string | null) => void;
  open: (entry: InspectorEntry) => void;
  activate: (key: string) => void;
  close: (key: string) => void;
  move: (key: string, offset: -1 | 1) => void;
}

export const useInspectorStack = create<InspectorStackState>((set) => ({
  entries: [],
  activeKey: null,
  replace: (entries, requestedActive = null) =>
    set(() => {
      const next = bounded(entries);
      const keys = next.map(inspectorEntryKey);
      const activeKey =
        requestedActive !== null && keys.includes(requestedActive)
          ? requestedActive
          : (keys.at(-1) ?? null);
      return { entries: next, activeKey };
    }),
  open: (entry) =>
    set((state) => {
      const key = inspectorEntryKey(entry);
      return {
        entries: bounded([...state.entries.filter((item) => inspectorEntryKey(item) !== key), entry]),
        activeKey: key,
      };
    }),
  activate: (key) =>
    set((state) =>
      state.entries.some((entry) => inspectorEntryKey(entry) === key)
        ? { activeKey: key }
        : state,
    ),
  close: (key) =>
    set((state) => {
      const index = state.entries.findIndex((entry) => inspectorEntryKey(entry) === key);
      if (index < 0) return state;
      const entries = state.entries.filter((entry) => inspectorEntryKey(entry) !== key);
      if (state.activeKey !== key) return { entries };
      const fallback = entries[Math.min(index, entries.length - 1)];
      return {
        entries,
        activeKey: fallback === undefined ? null : inspectorEntryKey(fallback),
      };
    }),
  move: (key, offset) =>
    set((state) => {
      const from = state.entries.findIndex((entry) => inspectorEntryKey(entry) === key);
      const to = Math.max(0, Math.min(state.entries.length - 1, from + offset));
      if (from < 0 || from === to) return state;
      const entries = [...state.entries];
      const [entry] = entries.splice(from, 1);
      if (entry === undefined) return state;
      entries.splice(to, 0, entry);
      return { entries };
    }),
}));
