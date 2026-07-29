import { StateChip } from '../../ui/StateChip.tsx';
import { Corners, Legend, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import {
  WIRE_AUTHORITY,
  WITHHELD_WORK,
  type WithheldSurface,
  withheldPresentation,
} from './authority.ts';

/**
 * Work — channel thirteen.
 *
 * The workspace is routed, named and reachable, and its data plane is closed.
 * That is the whole design: a Kanban board, a dependency DAG or a run status
 * drawn here today would be this dashboard's own invention, and inventing them
 * is the one thing this surface must never do. So it draws the boundary itself —
 * every projection, command and stream the PR14 cut owes, each carrying the
 * generated contract it waits on and the state it is actually in.
 *
 * No fetch is issued. Without a generated Work contract there is no request or
 * response shape to send, and hand-writing one here would put a second,
 * unreviewed wire format in the dashboard.
 */

const GATE_SENTENCE =
  'No generated Work read model is available in this build. Kanban, DAG, timeline, causal, workload, runtime, and control state are withheld rather than inferred.';

/** What the aside reports, and the exact words for each. Read model, projection,
 * command and stream fail separately, so each is stated separately rather than
 * collapsed into one "unavailable". */
const BOUNDARY: readonly { term: string; reading: string }[] = [
  { term: 'Work read model', reading: 'Not generated' },
  { term: 'Projections', reading: 'Not rendered' },
  { term: 'Commands', reading: 'Not exposed' },
  { term: 'Activity stream', reading: 'Not registered' },
];

/** The reasons present in one group, in row order and without repeats.
 *
 * The chips carry the state; the group says why once. Sixteen rows each
 * repeating "no generated read model" beside its own chip is the same sentence
 * sixteen times, which reads as noise rather than as an inventory. */
function reasonSummaries(surfaces: readonly WithheldSurface[]): string[] {
  const summaries = surfaces.map((surface) => withheldPresentation(surface.reason).summary);
  return [...new Set(summaries)];
}

function WithheldRow({ surface }: { surface: WithheldSurface }) {
  const presentation = withheldPresentation(surface.reason);
  return (
    <tr className="border-t border-edge-subtle align-top" data-work-surface={surface.id}>
      <th scope="row" className="px-2 py-1.5 text-left font-medium text-text-primary">
        {surface.name}
      </th>
      <td className="px-2 py-1.5 text-text-secondary max-md:hidden">{surface.draws}</td>
      <td className="px-2 py-1.5">
        <span className="td-value whitespace-nowrap text-text-secondary">{surface.requires}</span>
      </td>
      {/* From `lg` the state label stays on one line, which keeps the rows one
        * line tall and the ledger scannable. Below it the chip is allowed to
        * wrap, because a nowrap chip is width the 320px reflow budget does not
        * have. */}
      <td className="px-2 py-1.5 lg:whitespace-nowrap">
        <StateChip kind={presentation.state} />
      </td>
    </tr>
  );
}

export function WorkPage() {
  return (
    <div
      // The surface's own reading, for the accessibility and visual gates: this
      // page has one state and it is not a partial render of a wired workspace.
      data-work-authority="uncontracted"
      // The shell owns `main#td-main` and its scroller; a workspace that
      // scrolls does it in a labelled, focusable region of its own, which is
      // the only internal scrolling Plan 11 licenses.
      className="flex h-full min-w-0 flex-col overflow-auto"
      tabIndex={0}
      role="region"
      aria-label="Work content"
    >
      <WorkspaceHeader
        path="work"
        title="Work"
        note="canonical task graph · no generated read model in this build"
        actions={
          <StateChip
            kind="unsupported_schema"
            detail="Work contract absent"
            className="ml-auto shrink-0 max-sm:hidden"
          />
        }
      />

      <div data-work-ledger className="min-w-0 flex-1 p-3 sm:p-4">
        <div className="mx-auto grid min-w-0 max-w-[1600px] gap-3 lg:grid-cols-[minmax(0,1fr)_19rem] lg:gap-4">
          <div className="flex min-w-0 flex-col gap-3">
            <Panel legend="why this channel draws nothing">
              <p className="max-w-3xl text-sm leading-6 text-text-primary">{GATE_SENTENCE}</p>
              <p className="mt-3 max-w-3xl text-xs leading-5 text-text-secondary">
                Work is the canonical task graph and the minimal execution runtime, read through
                the one generated contract module this dashboard validates every response against.
                That module carries no Work payload, so there is no projection to render, no
                command to offer, and no stream to subscribe to — and a lane, an edge or a run
                status assembled here instead would be invented rather than read.
              </p>
              <p className="mt-3 max-w-3xl text-xs leading-5 text-text-secondary">
                The ledger below is the PR14 cut of Work derived from plan scope, each row naming
                the contract it waits on. The only measured fact on this page is this build's own
                contract inventory.
              </p>
            </Panel>

            {WITHHELD_WORK.map((group) => (
              <Panel
                key={group.id}
                legend={group.legend}
                bodyClassName="p-0"
                elevation="well"
                footer={
                  <p className="text-3xs tracking-[0.04em] text-text-muted">
                    Withheld here because there is {reasonSummaries(group.surfaces).join(', and ')}.
                  </p>
                }
              >
                <div
                  // Focusable and named: the ledger outruns 320px and 400% zoom
                  // sideways, and a scroll container a keyboard cannot reach is
                  // content nobody can read.
                  role="region"
                  aria-label={`${group.legend} table`}
                  tabIndex={0}
                  className="min-w-0 overflow-x-auto"
                >
                  <table className="w-full border-collapse text-2xs">
                    <caption className="sr-only">
                      Every {group.legend} the PR14 Work cut owes, with the generated contract it
                      requires and the state it is in. All are withheld in this build.
                    </caption>
                    <thead className="bg-surface-2">
                      <tr className="text-left text-text-secondary">
                        <th scope="col" className="px-2 py-1 font-medium">
                          Surface
                        </th>
                        <th scope="col" className="px-2 py-1 font-medium max-md:hidden">
                          What it would draw
                        </th>
                        <th scope="col" className="px-2 py-1 font-medium">
                          Requires
                        </th>
                        <th scope="col" className="px-2 py-1 font-medium">
                          State
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {group.surfaces.map((surface) => (
                        <WithheldRow key={surface.id} surface={surface} />
                      ))}
                    </tbody>
                  </table>
                </div>
              </Panel>
            ))}
          </div>

          <aside className="flex min-w-0 flex-col gap-3">
            <div className="relative min-w-0 border border-edge-subtle bg-surface-1 p-3 pt-4">
              <Corners />
              <Ticks count={24} />
              <Legend
                trailing={
                  <span className="shrink-0 text-3xs font-semibold uppercase tracking-[0.14em] text-state-unsupported-schema">
                    Withheld
                  </span>
                }
              >
                boundary
              </Legend>

              <dl className="mt-4 space-y-2.5 text-xs">
                {BOUNDARY.map((entry) => (
                  <div
                    key={entry.term}
                    className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] gap-3 border-b border-edge-subtle pb-2.5 last:border-b-0 last:pb-0"
                  >
                    <dt className="min-w-0 truncate text-text-muted">{entry.term}</dt>
                    <dd className="td-value min-w-0 break-words text-right font-medium text-text-primary">
                      {entry.reading}
                    </dd>
                  </div>
                ))}
              </dl>
            </div>

            <Panel legend="what opens this channel">
              <p className="text-xs leading-5 text-text-secondary">
                A Work payload has to enter through{' '}
                <span className="td-value text-text-primary">{WIRE_AUTHORITY}</span> and be
                regenerated into the dashboard's contracts module. The rows above then become
                reads; until they do, this channel stays navigable and closed rather than
                plausible.
              </p>
              <p className="mt-3 text-3xs leading-5 text-text-muted">
                The other twelve channels are unaffected: each keeps its own contract and its own
                state.
              </p>
            </Panel>
          </aside>
        </div>
      </div>
    </div>
  );
}
