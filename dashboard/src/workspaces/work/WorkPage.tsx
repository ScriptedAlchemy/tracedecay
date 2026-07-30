import { StateChip } from '../../ui/StateChip.tsx';
import { Corners, Legend, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import { WorkBoard, useSelectedTask } from './WorkBoard.tsx';
import { WorkCommands, WorkCreate } from './WorkCommands.tsx';
import { WorkTaskActivity } from './WorkTaskActivity.tsx';
import { resumeCursor, useWorkDelta, useWorkSnapshot } from './workQueries.ts';

/**
 * Work — channel thirteen.
 *
 * This page reads. Backend `3f43664cb` mounted the nine canonical Work routes
 * and contracted their payloads, so the board below is the daemon's own
 * `WorkProjectionSnapshotV1` rather than the boundary this workspace used to
 * draw in its place.
 *
 * What has not changed is the rule the boundary existed to keep. Every value
 * here came off a generated contract; nothing is inferred, and a route that
 * refuses is reported as the refusal it was. The residual panel at the foot of
 * the page is what remains genuinely unavailable — it shrank rather than
 * disappeared, and each line says why.
 */

/**
 * The runtime-attempt operations the dashboard deliberately does not expose.
 *
 * Named from `WORK_ATTEMPT_OPERATION_IDS_V1`
 * (`crates/tracedecay-application/src/work_catalog.rs`), which declares exactly
 * these eight. `work_api.rs` asserts that none of them is mounted on the
 * dashboard, so they are absent by the backend's own rule rather than by
 * omission here.
 */
const WITHHELD_ATTEMPT_OPERATIONS = [
  'acquire lease',
  'renew lease',
  'start',
  'publish progress',
  'publish artifact',
  'cancel',
  'recover',
  'terminalize',
] as const;

export function WorkPage() {
  const [selected, setSelected] = useSelectedTask();
  const snapshot = useWorkSnapshot();
  const result = snapshot.data;
  const value = result?.outcome === 'value' ? result.value : undefined;
  // Only asked for when the snapshot says it was capped or partial, so a
  // complete board issues no continuation request at all.
  const delta = useWorkDelta(value === undefined ? undefined : resumeCursor(value.coverage));

  const selectedProjection = value?.projections.find(
    (projection) => projection.task_id === selected,
  );

  return (
    <div
      className="min-w-0"
      data-work-authority={value === undefined ? 'unread' : 'read'}
      data-testid="work-page"
    >
      <WorkspaceHeader
        path="work"
        title="Work"
        note="canonical task graph · nine mounted routes"
        actions={<WorkTaskActivity kind="partial" />}
      />

      <div
        role="region"
        aria-label="Work content"
        tabIndex={0}
        className="relative min-w-0 overflow-x-auto p-3"
      >
        <Corners />
        <Ticks />

        <div className="flex min-w-0 flex-col gap-3">
          {snapshot.isPending ? (
            <Panel legend="Work read model">
              <StateChip kind="loading" detail="reading the snapshot" />
            </Panel>
          ) : null}

          {result?.outcome === 'refused' ? (
            <Panel legend="Work read model">
              {/* The daemon's own reason, in the taxonomy's vocabulary. An
                * unavailable runtime and an empty board are different things and
                * must never render alike. */}
              <StateChip kind={result.state} detail={result.detail} />
              <p className="mt-1 text-3xs text-text-muted">
                No board is drawn. This build reads the Work routes and does not
                infer their contents when they refuse.
              </p>
            </Panel>
          ) : null}

          {value === undefined ? null : (
            <>
              <WorkBoard snapshot={value} selected={selected} onSelect={setSelected} />

              {delta.data?.outcome === 'refused' ? (
                <Panel legend="Continuation">
                  <StateChip kind={delta.data.state} detail={delta.data.detail} />
                </Panel>
              ) : null}
              {delta.data?.outcome === 'value' ? (
                <Panel legend="Continuation">
                  <StateChip
                    kind="partial"
                    detail={`${delta.data.value.changed.length} changed, ${delta.data.value.removed.length} removed, through sequence ${delta.data.value.to_sequence}`}
                  />
                </Panel>
              ) : null}

              <div className="grid min-w-0 gap-3 lg:grid-cols-2">
                {selectedProjection === undefined ? (
                  <Panel legend="Commands">
                    <p className="text-2xs text-text-muted">
                      Select a task to see the commands its recorded state allows.
                    </p>
                  </Panel>
                ) : (
                  <WorkCommands projection={selectedProjection} snapshot={value} />
                )}
                <WorkCreate />
              </div>
            </>
          )}

          <aside className="min-w-0" aria-label="Work boundary">
            <div className="rounded-sm border border-edge bg-surface-2 p-2">
              <Legend
                trailing={<StateChip kind="unsupported" detail="runtime attempts" />}
              >
                What this channel still does not open
              </Legend>
              <p className="mt-1 text-3xs text-text-muted">
                The dashboard mounts the two read routes and the seven commands.
                It mounts none of the {WITHHELD_ATTEMPT_OPERATIONS.length}{' '}
                runtime-attempt operations —{' '}
                {WITHHELD_ATTEMPT_OPERATIONS.join(', ')} — because{' '}
                <code className="font-mono">work_api.rs</code> asserts the dashboard
                must not expose them. Proposal review, proposal acceptance and
                evidence attachment are mounted but undrawn: no generated contract
                in this build lists the pending proposals or the runs they would
                name.
              </p>
            </div>
          </aside>
        </div>
      </div>
    </div>
  );
}
