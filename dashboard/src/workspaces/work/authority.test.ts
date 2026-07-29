import { describe, expect, it } from 'vitest';
import * as contracts from '../../contracts/index.ts';
import { WITHHELD_WORK, withheldPresentation, type WithheldReason } from './authority.ts';

const SURFACES = WITHHELD_WORK.flatMap((group) => group.surfaces);
const EXPORTS = new Set(Object.keys(contracts));

/** `task_activity` names a stream rather than a type; every other row already
 * names its contract the way schemars emits it. */
function pascalCase(name: string): string {
  return name.replace(/(?:^|_)([a-z])/g, (_all, letter: string) => letter.toUpperCase());
}

/** Whether the generated contracts module carries a contract for one row, under
 * any of the shapes codegen emits (bare, versioned, payload-wrapped, and the zod
 * schema beside each). */
function generatedContractFor(requires: string): string | undefined {
  const base = pascalCase(requires);
  const bases = [base, `${base}V1`, `${base}Payload`, `${base}PayloadV1`];
  return bases
    .flatMap((name) => [name, `${name}Schema`])
    .find((candidate) => EXPORTS.has(candidate));
}

describe('the Work authority ledger', () => {
  /**
   * The gate's load-bearing claim, checked against the generated module rather
   * than against a sentence someone remembered to update.
   *
   * When the PR14 backend adds a Work payload to `DashboardContractCatalogV1`
   * and the contracts are regenerated, this test fails — which is the point. A
   * page that says "no read model exists" must not survive the read model
   * arriving.
   */
  it('claims withheld surfaces only while their contracts are genuinely absent', () => {
    const landed = SURFACES.map((surface) => ({
      surface: surface.id,
      contract: generatedContractFor(surface.requires),
    })).filter((entry) => entry.contract !== undefined);

    expect(
      landed,
      'a generated Work contract now exists, so this surface must be wired to it ' +
        'instead of rendering the contract gate',
    ).toEqual([]);
  });

  it('is a non-empty set of uniquely identified surfaces', () => {
    expect(WITHHELD_WORK.length).toBeGreaterThan(0);
    for (const group of WITHHELD_WORK) {
      expect(group.surfaces.length, `${group.id} is empty`).toBeGreaterThan(0);
    }

    const ids = SURFACES.map((surface) => surface.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('states what every row waits on and what it would draw', () => {
    for (const surface of SURFACES) {
      expect(surface.name).not.toBe('');
      expect(surface.draws, `${surface.id} does not say what it would draw`).not.toBe('');
      expect(surface.requires, `${surface.id} names no contract`).not.toBe('');
    }
  });

  it('never presents a withheld surface as available', () => {
    const reasons: readonly WithheldReason[] = [
      'read_model_absent',
      'command_absent',
      'stream_absent',
    ];

    for (const reason of reasons) {
      const presentation = withheldPresentation(reason);
      expect(['unsupported', 'unsupported_schema']).toContain(presentation.state);
      expect(presentation.summary).not.toBe('');
    }

    // Every row's reason is one the presentation switch handles, so no row can
    // reach the surface without a state.
    for (const surface of SURFACES) {
      expect(reasons).toContain(surface.reason);
    }
  });
});
