import { StateChip } from '../../ui/StateChip.tsx';
import { Corners, Legend, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import { WIRE_AUTHORITY, WITHHELD_WORK, type WithheldSurface } from './authority.ts';
import { WorkTaskActivity } from './WorkTaskActivity.tsx';
import {
  type WorkWire,
  type WorkWireState,
  isAbsentReading,
  resolveWorkStates,
  resolveWorkWire,
  wireReading,
  wireStateFor,
} from './workContracts.ts';

/**
 * Work — channel thirteen.
 *
 * The workspace is routed, named and reachable, and this build carries no
 * contract able to fill it. That is the whole design: a Kanban board, a
 * dependency DAG or a run status drawn without one would be this dashboard's own
 * invention, and inventing them is the one thing this surface must never do. So
 * it draws the boundary instead — every projection, command and stream Work is
 * made of, each naming the generated contract it waits on beside the state this
 * build measures it in.
 *
 * No fetch is issued. Without a generated Work contract there is no request or
 * response shape to send, and hand-writing one here would put a second,
 * unreviewed wire format in the dashboard.
 */

/**
 * This build's own contract inventory, measured once at import.
 *
 * The generated exports of a loaded build are fixed, so the reading is taken
 * here and every row reads from it. Nothing on this page asserts absence: the
 * chips, the gate sentence and the boundary panel are all derived from this.
 */
const WIRE_STATES: ReadonlyMap<string, WorkWireState> = new Map(
  resolveWorkStates().map((state) => [state.surface.id, state]),
);
const WIRE = resolveWorkWire([...WIRE_STATES.values()]);

const GATE_SENTENCE =
  'No generated Work read model is available in this build. Kanban, DAG, timeline, causal, workload, runtime, and control state are withheld rather than inferred.';

/** What the panel says about the channel as a whole, from the measured wire
 * rather than from a fixed sentence. A landed contract has to change this
 * paragraph, or the page would keep describing a closed channel over one that
 * had started to open. */
function gateReading(wire: WorkWire): string {
  switch (wire.kind) {
    case 'closed':
      return GATE_SENTENCE;
    case 'opening':
      return (
        `${wire.landed.length} generated Work contract${wire.landed.length === 1 ? '' : 's'} ` +
        'now exist in this build and no surface here reads them yet. The rows below name which. ' +
        'Nothing is inferred from them in the meantime.'
      );
    default: {
      const unhandled: never = wire;
      return unhandled;
    }
  }
}

function stateOf(surface: WithheldSurface): WorkWireState {
  return WIRE_STATES.get(surface.id) ?? wireStateFor(surface);
}

/** One group's reading for the aside, from the measured wire.
 *
 * Derived rather than fixed: three of these four terms are claims about what
 * exists, and a panel that keeps reporting "Not generated" over a generated
 * contract is the exact failure this channel is built to avoid.
 *
 * Counted by `isAbsentReading` rather than by landed contracts, and given both
 * readings rather than only the absent one. A group is not closed merely because
 * nothing has been generated for it — the streams group holds one row this build
 * genuinely subscribes to, and a count of landed contracts reported it as "Not
 * registered" beside a chip that said it was live. Each term names its own verb
 * because the four pieces are present in different senses: a read model is
 * generated, a command is exposed, a stream is subscribed to. */
function boundaryReading(groupId: string, absent: string, present: string): string {
  const group = WITHHELD_WORK.find((candidate) => candidate.id === groupId);
  if (group === undefined) return 'Group unknown';
  const reaching = group.surfaces.filter((surface) => !isAbsentReading(stateOf(surface))).length;
  if (reaching === 0) return absent;
  if (reaching === group.surfaces.length) return present;
  return `${reaching} of ${group.surfaces.length} · ${present}`;
}

/** What the aside reports, and the exact words for each. Read model, projection,
 * command and stream fail separately, so each is stated separately rather than
 * collapsed into one "unavailable". `Projections` is the one fixed reading,
 * because it describes this page rather than the backend: nothing here renders
 * one until a row is wired, landed contract or not. */
const BOUNDARY: readonly { term: string; reading: string }[] = [
  {
    term: 'Work read model',
    reading: boundaryReading('projections', 'Not generated', 'Generated, not read'),
  },
  { term: 'Projections', reading: 'Not rendered' },
  { term: 'Commands', reading: boundaryReading('commands', 'Not exposed', 'Exposed, not called') },
  {
    term: 'Activity stream',
    reading: boundaryReading('streams', 'Not registered', 'Subscribed, not read'),
  },
];

/** The distinct summaries a set of rows carries, in row order. */
function summaries(surfaces: readonly WithheldSurface[]): string {
  return [...new Set(surfaces.map((surface) => wireReading(stateOf(surface)).summary))].join(
    ', and ',
  );
}

/** What one group says about itself, once.
 *
 * The chips carry each row's state; the group states the reasons behind them.
 * Sixteen rows each repeating "no generated read model" beside its own chip is
 * the same sentence sixteen times, which reads as noise rather than as an
 * inventory. Derived, so a group that is no longer uniformly withheld cannot go
 * on saying that it is.
 *
 * Split on `isAbsentReading` rather than on `state.kind`, for two reasons that
 * both showed up in the streams group. It holds one row whose contract has not
 * landed and which is nonetheless a live subscription, so keying on the wire
 * state introduced it as "Withheld" — the plain opposite of what its own chip
 * says. And the absent summaries are the only ones that read as nouns after
 * "there is": `runtime_not_mounted` summarises as "subscribed, with no projection
 * to refetch", which that frame turned into "there is subscribed, with no
 * projection to refetch". A present row needs its own clause, not a slot in that
 * list. */
function groupFooter(surfaces: readonly WithheldSurface[]): string {
  const absent = surfaces.filter((surface) => isAbsentReading(stateOf(surface)));
  const reaching = surfaces.filter((surface) => !isAbsentReading(stateOf(surface)));

  if (reaching.length === 0) return `Withheld here because there is ${summaries(absent)}.`;
  if (absent.length === 0) return `None withheld here: ${summaries(reaching)}.`;
  return (
    `${absent.length} withheld because there is ${summaries(absent)}. ` +
    `${reaching.length} not withheld: ${summaries(reaching)}.`
  );
}

/** The row header a surface's cells are titled by, including the one on the
 * narrow-width detail row below it. */
function headerId(surface: WithheldSurface): string {
  return `work-surface-${surface.id}`;
}

function SurfaceRow({ surface }: { surface: WithheldSurface }) {
  const state = stateOf(surface);
  const presentation = wireReading(state);
  return (
    <>
      <tr className="border-t border-edge-subtle align-top" data-work-surface={surface.id}>
        {/* The name and nothing else. A row header is announced again at every
          * cell a reader visits, so the description that used to sit here opened
          * all four of this row's announcements with the same long sentence. It
          * still has to be readable at narrow widths — losing it there is what
          * `96ef71510` fixed — so it moved to the detail row below rather than
          * out of the table, and `pb-0` there lets that row supply the bottom
          * padding so the sentence still sits directly under the name. */}
        <th
          scope="row"
          id={headerId(surface)}
          className="px-2 py-1.5 text-left font-medium text-text-primary max-md:pb-0"
        >
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
          {/* The live row reads its detail from the stream itself. Keyed on the
            * reason rather than on the row's id, because what makes it live is
            * having a mounted subscription and nothing to apply it to. */}
          {surface.reason === 'runtime_not_mounted' ? (
            <WorkTaskActivity kind={presentation.state} />
          ) : (
            <StateChip
              kind={presentation.state}
              detail={state.kind === 'landed' ? state.contract : undefined}
            />
          )}
        </td>
      </tr>
      {/* What the row would draw, at the widths its own column is not drawn at.
        *
        * A continuation row rather than a `sr-only` cell in the hidden column:
        * `sr-only` positions absolutely, which blockifies a `td` out of the table
        * grid and leaves whether it is still a cell of this row up to the
        * browser. This stays in normal flow, spans the three columns that are
        * drawn below `md`, and is titled by the surface's own header through
        * `headers` — so the sentence is associated with its row explicitly
        * instead of by being buried in the row's name. */}
      <tr className="md:hidden" data-work-surface-detail={surface.id}>
        <td
          colSpan={3}
          headers={headerId(surface)}
          className="px-2 pb-1.5 text-text-secondary"
        >
          {surface.draws}
        </td>
      </tr>
    </>
  );
}

export function WorkPage() {
  return (
    <div
      // The surface's own reading, for the accessibility and visual gates. It is
      // measured, not declared: when a Work contract lands, this flips and the
      // gate that pins it to `uncontracted` fails until the rows are wired.
      data-work-authority={WIRE.kind === 'closed' ? 'uncontracted' : 'partially-contracted'}
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
        note={
          // Yielded below `sm` so the state chip beside it has the width to stay.
          // The header is one line and only one of the two fits there: the note
          // would truncate to a few characters anyway, and both halves of what it
          // says — the canonical task graph, and the missing read model — are
          // stated in full in the panel immediately below. The state is not.
          <span className="max-sm:hidden">
            {WIRE.kind === 'closed'
              ? 'canonical task graph · no generated read model in this build'
              : 'canonical task graph · generated contracts landed, not read here yet'}
          </span>
        }
        actions={
          // The same state twice, and exactly one of them displayed: the header
          // is one 36px line, and the detail makes the chip wide enough to wrap
          // onto a second inside it below `sm`. Dropping the whole chip there was
          // the wrong half to lose — it left the narrowest viewport, the one that
          // can least afford to read a paragraph to find out where it stands,
          // with the page's state stated only in prose. So the detail is what
          // goes, and the state itself stays scannable at 320px.
          <>
            <StateChip
              kind={WIRE.kind === 'closed' ? 'unsupported_schema' : 'partial'}
              className="ml-auto shrink-0 sm:hidden"
            />
            <StateChip
              kind={WIRE.kind === 'closed' ? 'unsupported_schema' : 'partial'}
              detail={WIRE.kind === 'closed' ? 'Work contract absent' : 'Work contract unwired'}
              className="ml-auto shrink-0 max-sm:hidden"
            />
          </>
        }
      />

      <div data-work-ledger className="min-w-0 flex-1 p-3 sm:p-4">
        <div className="mx-auto grid min-w-0 max-w-[1600px] gap-3 lg:grid-cols-[minmax(0,1fr)_19rem] lg:gap-4">
          <div className="flex min-w-0 flex-col gap-3">
            <Panel
              legend={
                WIRE.kind === 'closed'
                  ? 'why this channel draws nothing'
                  : 'what landed, and why this channel still draws nothing'
              }
            >
              <p className="max-w-3xl text-sm leading-6 text-text-primary">{gateReading(WIRE)}</p>
              <p className="mt-3 max-w-3xl text-xs leading-5 text-text-secondary">
                Work is the canonical task graph and the minimal execution runtime, read through
                the one generated contract module this dashboard validates every response against.
                {WIRE.kind === 'closed'
                  ? ' That module carries no Work payload, so there is no projection to render, no command to offer, and no stream to subscribe to'
                  : ' No surface here reads what that module now carries, so there is still no projection rendered, no command offered and no stream subscribed to'}{' '}
                — and a lane, an edge or a run status assembled here instead would be invented
                rather than read.
              </p>
              <p className="mt-3 max-w-3xl text-xs leading-5 text-text-secondary">
                The ledger below is everything Work is made of, each row naming the contract it
                waits on and the state this build measures it in. That inventory — taken from the
                generated module itself — is the only fact on this page.
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
                    {groupFooter(group.surfaces)}
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
                      Every one of the {group.legend}, with the generated contract it requires and
                      the state this build measures it in.
                    </caption>
                    <thead className="bg-surface-2">
                      <tr className="text-left text-text-secondary">
                        <th scope="col" className="px-2 py-1 font-medium">
                          Surface
                        </th>
                        {/* Hidden with its column below `md`, where each row
                          * states the same sentence on its own detail row
                          * instead. */}
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
                        <SurfaceRow key={surface.id} surface={surface} />
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
                  <span
                    className={`shrink-0 text-3xs font-semibold uppercase tracking-[0.14em] ${
                      WIRE.kind === 'closed'
                        ? 'text-state-unsupported-schema'
                        : 'text-state-partial'
                    }`}
                  >
                    {WIRE.kind === 'closed' ? 'Withheld' : 'Unwired'}
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
