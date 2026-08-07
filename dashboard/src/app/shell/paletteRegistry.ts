import { useEffect } from 'react';
import { create } from 'zustand';

import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { InspectorEntry } from './inspectorStack.ts';

interface PaletteEntryBase {
  readonly id: string;
  readonly label: string;
  readonly hint: string;
  readonly keywords?: readonly string[];
  readonly state?: DomainStateKind;
  readonly scopeLabel?: string;
}

export type PaletteEntry =
  | (PaletteEntryBase & {
      readonly kind: 'navigate';
      readonly to: string;
    })
  | (PaletteEntryBase & {
      readonly kind: 'inspect';
      readonly inspector: InspectorEntry;
      readonly to?: string;
    })
  | (PaletteEntryBase & {
      readonly kind: 'legal_action';
      readonly reference: Readonly<Record<string, unknown>>;
      readonly invoke: (reference: Readonly<Record<string, unknown>>) => void;
    });

interface PaletteRegistryState {
  readonly providers: Readonly<Record<string, readonly PaletteEntry[]>>;
  register: (providerId: string, entries: readonly PaletteEntry[]) => void;
  unregister: (providerId: string) => void;
  clear: () => void;
}

export const usePaletteRegistry = create<PaletteRegistryState>((set) => ({
  providers: {},
  register: (providerId, entries) =>
    set((state) => ({
      providers: { ...state.providers, [providerId]: entries },
    })),
  unregister: (providerId) =>
    set((state) => {
      if (!(providerId in state.providers)) return state;
      const providers = { ...state.providers };
      delete providers[providerId];
      return { providers };
    }),
  clear: () => set({ providers: {} }),
}));

/** Registers entries only while their owning real-data adapter is mounted.
 * Removing the adapter withdraws its entries; the shell never keeps an entity
 * or legal action after the source that supplied it is gone. */
export function usePaletteEntries(
  providerId: string,
  entries: readonly PaletteEntry[],
): void {
  const register = usePaletteRegistry((state) => state.register);
  const unregister = usePaletteRegistry((state) => state.unregister);
  useEffect(() => {
    register(providerId, entries);
    return () => unregister(providerId);
  }, [entries, providerId, register, unregister]);
}
