import type { DomainStateKind } from '../../ui/StateChip.tsx';
import * as contracts from '../../contracts/index.ts';
import { WITHHELD_WORK, type WithheldSurface, withheldPresentation } from './authority.ts';

/**
 * Whether this build carries a contract for each Work surface, measured.
 *
 * The ledger next door states what Work is made of. This module answers the only
 * question about it that can be checked at runtime: has any of it arrived? The
 * page reads its states from here rather than from the ledger's prose, so
 * "withheld" stops being a sentence someone has to remember to delete and
 * becomes a reading of the generated contracts module.
 *
 * It is also the activation seam. No Work payload shape is written here, because
 * writing one would be a second unreviewed wire format — the shapes will arrive
 * as generated zod schemas and be inferred from them. What is written here is
 * the set of *names* to watch and the branch each row takes when one appears, so
 * turning a row into a read is a local change in one component rather than a
 * hunt through the workspace.
 */

/**
 * The generated module's runtime export names.
 *
 * Its zod schemas are values, so they are visible here; `type` aliases are
 * erased and are not. That is the stronger signal regardless — a schema is what
 * a response would be validated against, and codegen emits one beside every
 * type. Computed once per build, since it cannot change while the page is open.
 */
const GENERATED_EXPORTS: ReadonlySet<string> = new Set(Object.keys(contracts));

export type WorkWireState =
  | { readonly kind: 'withheld'; readonly surface: WithheldSurface }
  /** A generated contract now exists for this row and nothing here reads it yet.
   * Distinct from `withheld` on purpose: "absent" and "present but unwired" are
   * different facts, and reporting the second as the first would make this page
   * lie about a contract that had already landed. */
  | { readonly kind: 'landed'; readonly surface: WithheldSurface; readonly contract: string };

/** Matched by prefix so `WorkSnapshot`, `WorkSnapshotV1` and
 * `WorkSnapshotV1Schema` all count as the same arrival. */
function contractFor(surface: WithheldSurface, exports: ReadonlySet<string>): string | undefined {
  for (const watched of surface.watches) {
    for (const name of exports) {
      if (name.startsWith(watched)) return name;
    }
  }
  return undefined;
}

export function wireStateFor(
  surface: WithheldSurface,
  exports: ReadonlySet<string> = GENERATED_EXPORTS,
): WorkWireState {
  const contract = contractFor(surface, exports);
  return contract === undefined ? { kind: 'withheld', surface } : { kind: 'landed', surface, contract };
}

export interface WireReading {
  readonly state: DomainStateKind;
  readonly summary: string;
}

export function wireReading(state: WorkWireState): WireReading {
  switch (state.kind) {
    case 'withheld':
      return withheldPresentation(state.surface.reason);
    case 'landed':
      return { state: 'partial', summary: `${state.contract} landed, not read here yet` };
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

export type WorkWire =
  /** No Work contract exists in this build. Every row is withheld and the
   * channel's closed reading is the whole truth about it. */
  | { readonly kind: 'closed' }
  /** At least one contract has arrived. The page must name what landed rather
   * than keep presenting a uniformly closed channel. */
  | { readonly kind: 'opening'; readonly landed: readonly WorkWireState[] };

/** Every row's state, resolved in one pass. The page holds the result rather
 * than re-resolving per render: the generated exports of a loaded build cannot
 * change, so this is measured once and read many times. */
export function resolveWorkStates(
  exports: ReadonlySet<string> = GENERATED_EXPORTS,
): readonly WorkWireState[] {
  return WITHHELD_WORK.flatMap((group) => group.surfaces).map((surface) =>
    wireStateFor(surface, exports),
  );
}

export function resolveWorkWire(
  states: readonly WorkWireState[] = resolveWorkStates(),
): WorkWire {
  const landed = states.filter((state) => state.kind === 'landed');
  return landed.length === 0 ? { kind: 'closed' } : { kind: 'opening', landed };
}
