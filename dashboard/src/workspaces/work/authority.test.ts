import { describe, expect, it } from 'vitest';
import * as contracts from '../../contracts/index.ts';
import { WITHHELD_WORK, withheldPresentation, type WithheldReason } from './authority.ts';

const SURFACES = WITHHELD_WORK.flatMap((group) => group.surfaces);

/**
 * The generated module's runtime exports, which are its zod schemas: a `type`
 * alias is erased before this can see it, so a landed contract is detected
 * through the schema const codegen always emits beside the type. That is the
 * stronger signal anyway — a schema is what a read would be validated against.
 */
const EXPORTS = new Set(Object.keys(contracts));

/** `task_activity` names a stream rather than a type; every other row already
 * names its contract the way schemars emits it. */
function pascalCase(name: string): string {
  return name.replace(/(?:^|_)([a-z])/g, (_all, letter: string) => letter.toUpperCase());
}

/**
 * Whether the generated contracts module carries a contract for one row.
 *
 * Matched by prefix rather than against a list of expected suffixes. A read
 * model does not reach the dashboard as the domain type's own name: it arrives
 * as whatever wire shape the backend wraps it in — a snapshot, an incremental
 * delta, an envelope payload — each with a `V1` revision and a zod schema
 * beside it. An exact-name check would let `WorkProjectionSnapshotV1` land while
 * this page went on claiming no read model exists, which is the one failure this
 * gate is here to prevent. Prefix matching costs a theoretical false positive
 * and removes a likely false negative, and only the false negative can lie to
 * someone reading the page.
 */
function contractFor(requires: string, exports: ReadonlySet<string>): string | undefined {
  const base = pascalCase(requires);
  return [...exports].find((name) => name.startsWith(base));
}

function generatedContractFor(requires: string): string | undefined {
  return contractFor(requires, EXPORTS);
}

describe('the Work authority ledger', () => {
  /**
   * The gate's load-bearing claim, checked against the generated module rather
   * than against a sentence someone remembered to update.
   *
   * When the backend adds a Work payload to `DashboardContractCatalogV1` and
   * the contracts are regenerated, this test fails — which is the point. A
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

  /**
   * The gate is only worth having if it fails when it should, so both of its
   * error directions are checked against the names codegen actually produces.
   */
  it('detects a landed read model whatever wire shape carries it', () => {
    const landed = new Set([
      'WorkProjectionSnapshotV1',
      'WorkProjectionSnapshotV1Schema',
      'WorkEventDeltaV1',
    ]);

    expect(contractFor('WorkProjection', landed)).toBe('WorkProjectionSnapshotV1');
    expect(contractFor('WorkEvent', landed)).toBe('WorkEventDeltaV1');
  });

  it('does not mistake worktree topology policy for canonical Work', () => {
    // `WorkTopologyPolicyV1` is already generated, and it is branch and worktree
    // placement policy — nothing to do with the task graph. It shares only the
    // first four letters, so it is the nearest thing to a false positive the
    // real contracts contain.
    expect(EXPORTS.has('WorkTopologyPolicyV1Schema')).toBe(true);
    for (const surface of SURFACES) {
      expect(
        generatedContractFor(surface.requires),
        `${surface.id} matched an unrelated contract`,
      ).not.toBe('WorkTopologyPolicyV1');
    }
  });

  /**
   * The activity stream is the one row whose arrival this module cannot see. A
   * stream is registered as a variant of the daemon's event-kind union, and that
   * union is not part of the generated contracts at all, so no export appears
   * when `task_activity` starts flowing. Rather than let the row pass as absent
   * on evidence that could never say otherwise, the absence of the union is
   * asserted directly: when it is added to the catalog, this fails and the
   * stream row gets a real check.
   */
  it('cannot yet verify the activity stream, and says so', () => {
    const eventKinds = [...EXPORTS].filter((name) => name.startsWith('DashboardEventKind'));
    expect(
      eventKinds,
      'the event-kind union now reaches the dashboard, so the stream row must be ' +
        'checked against it instead of assumed absent',
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
