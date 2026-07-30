import { describe, expect, it } from 'vitest';
import { WITHHELD_WORK, withheldPresentation, type WithheldReason } from './authority.ts';
import { resolveWorkStates, resolveWorkWire, wireStateFor } from './workContracts.ts';

const SURFACES = WITHHELD_WORK.flatMap((group) => group.surfaces);

/** A surface by id, so a test can name the row it is about. */
function surface(id: string) {
  const found = SURFACES.find((candidate) => candidate.id === id);
  if (found === undefined) throw new Error(`no such Work surface: ${id}`);
  return found;
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
    const landed = resolveWorkStates()
      .filter((state) => state.kind === 'landed')
      .map((state) => ({ surface: state.surface.id, contract: state.contract }));

    expect(
      landed,
      'a generated Work contract now exists, so this surface must be wired to it ' +
        'instead of rendering the contract gate',
    ).toEqual([]);
    expect(resolveWorkWire().kind).toBe('closed');
  });

  /**
   * The gate is only worth having if it fails when it should, so both of its
   * error directions are checked against the names codegen actually produces.
   */
  it('detects a landed read model whatever wire shape carries it', () => {
    const announced = new Set(['WorkProjectionSnapshotV1Schema', 'WorkEventDeltaV1Schema']);
    expect(wireStateFor(surface('kanban'), announced)).toMatchObject({
      kind: 'landed',
      contract: 'WorkProjectionSnapshotV1Schema',
    });
    expect(wireStateFor(surface('history'), announced)).toMatchObject({
      kind: 'landed',
      contract: 'WorkEventDeltaV1Schema',
    });
  });

  /**
   * The failure this gate came closest to shipping with.
   *
   * The rows are labelled with design names — `WorkProjection`, `WorkEvent`,
   * `ExecutionAdmission` — and the committed application authority does not use
   * any of them: it serves `WorkSnapshotV1`, `WorkDeltaV1` and
   * `AdmitExecutionCommand`. A gate keyed off the labels would have watched for
   * names nothing will ever emit and left this page claiming absence over live
   * contracts, so the names actually implemented are asserted here by hand.
   */
  it('detects the names the committed application authority actually uses', () => {
    const implemented = new Set([
      'WorkSnapshotV1Schema',
      'WorkDeltaV1Schema',
      'CreateWorkCommandSchema',
      'ReviewProposalCommandSchema',
      'AdmitExecutionCommandSchema',
      'AttachRuntimeEvidenceCommandSchema',
    ]);

    for (const id of ['kanban', 'timeline', 'graph-change', 'proposal-review', 'admission', 'acceptance']) {
      expect(wireStateFor(surface(id), implemented).kind, `${id} missed its landed contract`).toBe(
        'landed',
      );
    }

    expect(resolveWorkWire(resolveWorkStates(implemented)).kind).toBe('opening');
  });

  it('does not mistake worktree topology policy for canonical Work', () => {
    // `WorkTopologyPolicyV1` and the `Worktree*` family are already generated,
    // and they are branch and worktree placement policy — nothing to do with the
    // task graph. Sharing four letters with `Work`, they are the nearest thing to
    // a false positive the real contracts contain, and the previous run of this
    // suite proves they do not trip the gate.
    const decoys = new Set([
      'WorkTopologyPolicyV1Schema',
      'WorktreeRetentionPolicyV1Schema',
      'WorktreeIdSchema',
    ]);
    for (const candidate of SURFACES) {
      expect(
        wireStateFor(candidate, decoys).kind,
        `${candidate.id} matched an unrelated contract`,
      ).toBe('withheld');
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
  it('watches the event union for the activity stream', () => {
    expect(surface('task-activity').watches).toEqual(['DashboardEventKind']);
    expect(
      wireStateFor(surface('task-activity'), new Set(['DashboardEventKindSchema'])).kind,
      'the event union reaching the dashboard must flip the stream row',
    ).toBe('landed');
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
    for (const candidate of SURFACES) {
      expect(candidate.name).not.toBe('');
      expect(candidate.draws, `${candidate.id} does not say what it would draw`).not.toBe('');
      expect(candidate.requires, `${candidate.id} names no contract`).not.toBe('');
      expect(candidate.watches.length, `${candidate.id} watches no contract name`).toBeGreaterThan(
        0,
      );
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
    for (const candidate of SURFACES) {
      expect(reasons).toContain(candidate.reason);
    }
  });
});
