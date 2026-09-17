/**
 * Workspace-owned registers on the shell's bottom status strip.
 *
 * The strip's first cells — Link, Feed, Source, Query — are the shell's own
 * transport facts. A workspace may publish the states of the authorities it
 * is reading beside them so the strip and the aperture report the same words:
 * the Code workspace posts its graph read, index freshness and pinned
 * selection here. Registers are published for as long as the workspace is
 * mounted and withdrawn when it unmounts, so a stale authority from a route
 * the reader has left can never keep reporting.
 *
 * Every register carries a domain state and the word the workspace uses for
 * it; the strip never derives either. The state drives the swatch and the
 * label carries the meaning, so colour is never the only channel.
 */
import { useEffect, useMemo } from 'react';
import { create } from 'zustand';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

export interface StatusRegister {
  /** Stable per-workspace id; the strip keys on it. */
  readonly id: string;
  /** The register's engraved name, e.g. `Graph`. */
  readonly label: string;
  /** The reading printed after the swatch, in the workspace's own words. */
  readonly value: string;
  /** Taxonomy state behind the reading, or `identity` for a register that
   * names a selection rather than reporting a source. Drives the swatch only. */
  readonly state: DomainStateKind | 'identity';
  /** The reason or qualifier the word alone cannot carry. */
  readonly detail?: string | undefined;
}

interface StatusRegistersState {
  /** Registers by owner; one owner per mounted workspace. */
  readonly owners: ReadonlyMap<string, readonly StatusRegister[]>;
  publish: (owner: string, registers: readonly StatusRegister[]) => void;
  withdraw: (owner: string) => void;
}

export const useStatusRegistersStore = create<StatusRegistersState>((set) => ({
  owners: new Map(),
  publish: (owner, registers) =>
    set((current) => {
      const next = new Map(current.owners);
      next.set(owner, registers);
      return { owners: next };
    }),
  withdraw: (owner) =>
    set((current) => {
      if (!current.owners.has(owner)) return current;
      const next = new Map(current.owners);
      next.delete(owner);
      return { owners: next };
    }),
}));

/** Every published register, in owner insertion order. */
export function useStatusRegisters(): readonly StatusRegister[] {
  // Select the map itself, which is a stable reference between publishes, and
  // derive the list from it: a selector that built a fresh array on every call
  // would hand `useSyncExternalStore` a new snapshot per render.
  const owners = useStatusRegistersStore((state) => state.owners);
  return useMemo(() => flatten(owners), [owners]);
}

function flatten(owners: ReadonlyMap<string, readonly StatusRegister[]>): readonly StatusRegister[] {
  const out: StatusRegister[] = [];
  for (const registers of owners.values()) out.push(...registers);
  return out;
}

/**
 * Publish a workspace's registers for as long as it is mounted.
 *
 * Re-publishes whenever the registers' content changes, keyed on a stable
 * serialisation so a parent re-render with equal readings does not churn the
 * store, and withdraws on unmount.
 */
export function usePublishStatusRegisters(owner: string, registers: readonly StatusRegister[]) {
  const publish = useStatusRegistersStore((state) => state.publish);
  const withdraw = useStatusRegistersStore((state) => state.withdraw);
  const signature = JSON.stringify(registers);
  useEffect(() => {
    publish(owner, JSON.parse(signature) as StatusRegister[]);
  }, [owner, publish, signature]);
  useEffect(() => () => withdraw(owner), [owner, withdraw]);
}
