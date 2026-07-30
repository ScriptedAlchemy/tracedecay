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
  it('detects a landed read model under every suffix codegen produces', () => {
    // Codegen emits an interface, a `V<n>` revision and a zod const for one
    // type. Those are the same arrival, so each has to flip the row on its own.
    for (const name of [
      'WorkProjectionSnapshot',
      'WorkProjectionSnapshotV1',
      'WorkProjectionSnapshotSchema',
      'WorkProjectionSnapshotV1Schema',
    ]) {
      expect(
        wireStateFor(surface('kanban'), new Set([name])),
        `${name} did not read as an arrival`,
      ).toMatchObject({ kind: 'landed', contract: name });
    }
  });

  /**
   * The arrival this page must not invent.
   *
   * The domain mints its identifiers as schemars string-id newtypes sitting
   * beside the payloads that carry them — `WorkCommandId`,
   * `WorkCancellationRequestId`, `WorkProviderRouteId`
   * (`crates/tracedecay-domain/src/research/id.rs`). Any Work read model
   * embedding one generates the identifier's schema, and an unanchored prefix
   * match would read that as the payload landing: the page would announce
   * contracts that had not arrived and count them in its headline. A request
   * DTO is the same mistake in milder form — it is what calls the read, not the
   * read.
   */
  it('does not mistake an identifier or a request for the payload', () => {
    const nearMisses = new Set([
      'WorkCommandIdSchema',
      'WorkCancellationRequestIdSchema',
      'WorkProviderRouteIdSchema',
      'WorkLeaseIdSchema',
      'WorkArtifactIdSchema',
      'WorkProjectionSnapshotRequestV1Schema',
      'WorkProjectionDeltaRequestV1Schema',
    ]);

    for (const candidate of SURFACES) {
      expect(
        wireStateFor(candidate, nearMisses).kind,
        `${candidate.id} read an identifier or a request as its contract`,
      ).toBe('withheld');
    }
    expect(resolveWorkWire(resolveWorkStates(nearMisses)).kind).toBe('closed');
  });

  /**
   * The failure this gate came closest to shipping with.
   *
   * The rows are labelled with design names — `WorkProjection`, `WorkEvent` —
   * and no crate spells its wire that way, so a gate keyed off the labels would
   * watch for names nothing can emit and leave this page claiming absence over
   * live contracts. The write names actually implemented are asserted here by
   * hand, against `crates/tracedecay-application/src/work.rs`.
   */
  it('detects the command names the application authority actually uses', () => {
    const implemented = new Set([
      'CreateWorkCommandSchema',
      'ReplanDependenciesCommandSchema',
      'ReviewProposalCommandSchema',
      'AcceptProposalCommandSchema',
      'AdmitExecutionCommandSchema',
      'AttachRuntimeEvidenceCommandSchema',
      'AcceptTaskCommandSchema',
    ]);

    for (const id of ['graph-change', 'proposal-review', 'admission', 'acceptance']) {
      expect(wireStateFor(surface(id), implemented).kind, `${id} missed its landed contract`).toBe(
        'landed',
      );
    }

    expect(resolveWorkWire(resolveWorkStates(implemented)).kind).toBe('opening');
  });

  /**
   * A watch on a type no crate defines is indistinguishable from no watch at
   * all, and it reads as coverage.
   *
   * `1fc31a865` deleted the competing `WorkSnapshotV1` and `WorkDeltaV1` wires,
   * and this ledger went on watching both, so two read rows were waiting on a
   * name that could never arrive. The others here were never defined anywhere:
   * they are design labels that had been copied into the watch list.
   */
  it('watches no contract name the workspace does not define', () => {
    const undefinedNames = [
      'WorkSnapshot',
      'WorkDelta',
      'WorkEventDelta',
      'WorkCommand',
      'WorkProposal',
      'ExecutionAdmission',
      'RunControl',
    ];

    for (const candidate of SURFACES) {
      for (const watched of candidate.watches) {
        expect(
          undefinedNames,
          `${candidate.id} watches ${watched}, which no crate emits`,
        ).not.toContain(watched);
      }
    }
  });

  /**
   * The canonical contracts, by their exact committed names.
   *
   * `crates/tracedecay-domain/src/work_read.rs` serves `WorkProjectionSnapshotV1`
   * and `WorkProjectionDeltaV1`; `work_runtime.rs` serves `WorkAttemptV1` and its
   * leaves. None of these is spelled the way this ledger labels its rows, and the
   * delta in particular is neither `WorkDeltaV1` nor `WorkEventDeltaV1` — the two
   * names previously watched for it. Every row is asserted to fire, because a row
   * that sleeps through its own contract arriving is this page's worst failure.
   */
  it('detects the canonical read and runtime contract names', () => {
    const canonical = new Set([
      'WorkProjectionSnapshotV1Schema',
      'WorkProjectionDeltaV1Schema',
      'WorkProjectionCoverageV1Schema',
      'WorkProjectionResumeCursorV1Schema',
      'WorkAttemptV1Schema',
      'WorkAttemptProgressV1Schema',
      'WorkProviderRouteV1Schema',
      'WorkArtifactRefV1Schema',
      'WorkCancellationRequestV1Schema',
      'WorkTerminalEvidenceV1Schema',
      'WorkLeaseFenceV1Schema',
      'DashboardEventKindSchema',
    ]);

    const asleep = resolveWorkStates(canonical)
      .filter((state) => state.kind === 'withheld')
      .map((state) => state.surface.id);

    // The five command rows are application-crate writes and are not part of
    // these two commits, so they are expected to stay withheld.
    expect(asleep.sort()).toEqual(
      ['admission', 'graph-change', 'proposal-review'].sort(),
    );
    expect(resolveWorkWire(resolveWorkStates(canonical)).kind).toBe('opening');
  });

  /** Each projection row must resolve to the contract that would actually serve
   * it, not to whichever Work name landed first. */
  it('resolves each row to its own canonical contract', () => {
    const canonical = new Set([
      'WorkProjectionSnapshotV1Schema',
      'WorkProjectionDeltaV1Schema',
      'WorkAttemptV1Schema',
    ]);

    expect(wireStateFor(surface('kanban'), canonical)).toMatchObject({
      contract: 'WorkProjectionSnapshotV1Schema',
    });
    expect(wireStateFor(surface('timeline'), canonical)).toMatchObject({
      contract: 'WorkProjectionDeltaV1Schema',
    });
    expect(wireStateFor(surface('executor'), canonical)).toMatchObject({
      contract: 'WorkAttemptV1Schema',
    });
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
    expect(surface('task-activity').watches).toContain('DashboardEventKind');
    // The payload such a stream would carry, so the row also fires if progress
    // reaches the dashboard before the union naming its channel does.
    expect(surface('task-activity').watches).toContain('WorkAttemptProgress');
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

  it('never presents a surface as available', () => {
    /** The absences. Each must read as unsupported: there is nothing behind it
     * at all, so no amount of it can have been read. */
    const absences: readonly WithheldReason[] = [
      'read_model_absent',
      'command_absent',
      'stream_absent',
    ];
    /** The one reason that is not an absence, and must not be reported as one. */
    const mounted: readonly WithheldReason[] = ['runtime_not_mounted'];

    for (const reason of absences) {
      const presentation = withheldPresentation(reason);
      expect(['unsupported', 'unsupported_schema']).toContain(presentation.state);
      expect(presentation.summary).not.toBe('');
    }

    for (const reason of mounted) {
      const presentation = withheldPresentation(reason);
      expect(presentation.state).toBe('partial');
      expect(presentation.summary).not.toBe('');
    }

    // Whichever it is, it is never a state that claims the data arrived.
    for (const reason of [...absences, ...mounted]) {
      expect(['ready', 'complete_zero_findings']).not.toContain(
        withheldPresentation(reason).state,
      );
    }

    // Every row's reason is one the presentation switch handles, so no row can
    // reach the surface without a state.
    for (const candidate of SURFACES) {
      expect([...absences, ...mounted]).toContain(candidate.reason);
    }
  });
});
