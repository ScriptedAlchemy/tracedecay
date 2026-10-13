import { useMemo } from 'react';
import type {
  ExecutionTopologyMetricsV1,
  ExecutionTopologyViewV1,
  WorkAttemptListV1,
} from '../../../contracts/generated.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { Meter, Panel } from '../../../ui/instrument.tsx';
import type { WorkResult } from '../workApi.ts';
import type { WorkChannel } from '../workChannel.ts';
import type { WorkGraphReading } from '../workGraphModel.ts';
import {
  METRICS_GAP_REASON,
  WORK_ACCOUNTING_DIMENSIONS,
  accountingEventKind,
  isUndecoded,
  type WorkAccountingCard,
  type WorkAccountingFigure,
  type WorkAccountingMatrix,
  type WorkAccountingProvenance,
  type WorkAccountingRow,
} from '../workAccountingModel.ts';
import { workTopologyAccounting } from '../workTopologyAccounting.ts';
import { ChannelAbsence, ViewCaption } from './WorkViewChannel.tsx';

/**
 * Plan 26's execution-topology accounting, drawn on the landed topology lens.
 *
 * One ledger, twelve cards, and the same seven-facet footer under every one of
 * them. `workTopologyAccounting.ts` explains which cards have a mounted source
 * and why the rest stay stated absences; this file's whole job is to render
 * what that structure holds and to be incapable of rendering anything else.
 *
 * Two rendering rules carry the plan's invariant, and both are structural
 * rather than stylistic:
 *
 *   A figure is drawn only from `channel.available`. There is no fallback
 *   branch, no `?? 0`, and no default. An absent channel renders its state chip
 *   and its sentence through `ChannelAbsence`, which is the same mark the four
 *   Work projections use for the same purpose, so an absence on this ledger
 *   reads as the absence a reader has already learned elsewhere on the page.
 *
 *   A meter is drawn only beside a figure that exists. A zero-length bar under
 *   an unavailable row is exactly the falsified zero the plan forbids, and the
 *   cheapest way not to draw one is to have no code path that could.
 *
 * The confusion matrices are real tables with row and column headers rather
 * than a grid of divs, because a matrix whose cells are all unavailable is
 * still a matrix and a screen reader has to be able to say which cell it is
 * in. No cell is summed, and no scalar is derived from any of them.
 */

export function WorkTopologyAccounting({
  attemptList,
  topology,
  graph,
  metrics,
}: {
  attemptList: WorkResult<WorkAttemptListV1> | undefined;
  topology?: WorkResult<ExecutionTopologyViewV1> | undefined;
  graph: WorkGraphReading;
  metrics?: WorkResult<ExecutionTopologyMetricsV1> | undefined;
}) {
  const reading = useMemo(
    () => workTopologyAccounting(attemptList, graph, topology, metrics),
    [attemptList, graph, topology, metrics],
  );
  return (
    <Panel legend="Execution-topology accounting" elevation="well">
      <div className="flex min-w-0 flex-col gap-3">
        <ViewCaption
          population={`${reading.measured} of ${reading.cards.length} dimensions measured · ${reading.uncovered.length} not decoded by this ledger`}
          note="Plan 26's execution-topology mandate, dimension by dimension. Every card states its support, eligible denominator, censoring, interval coverage, horizon, revision pin, and safe anchors. Measures this ledger does not decode sit behind each card's disclosure, and dimensions it decodes nothing for are listed once under coverage."
        />

        {reading.stated.length === 0 ? (
          <p className="text-2xs leading-snug text-text-muted" data-work-accounting-stated="none">
            No dimension carries a reading or a stated reason here; all {reading.uncovered.length}{' '}
            are listed under coverage.
          </p>
        ) : (
          <ol
            className="flex min-w-0 flex-col gap-2"
            data-work-accounting-cards={reading.stated.length}
            data-work-accounting-dimensions={WORK_ACCOUNTING_DIMENSIONS.length}
          >
            {reading.stated.map((card) => (
              <li key={card.dimension} className="min-w-0">
                <AccountingCard card={card} />
              </li>
            ))}
          </ol>
        )}

        <CoverageDisclosure cards={reading.uncovered} />
      </div>
    </Panel>
  );
}

// Not `flex`: a `<summary>` draws its disclosure marker only while it is
// `display: list-item`, and that marker is the signal that the row opens.
const SUMMARY_CLASS =
  'min-h-[44px] cursor-pointer content-center px-2.5 text-2xs text-text-secondary hover:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent';

function CoverageDisclosure({ cards }: { cards: readonly WorkAccountingCard[] }) {
  if (cards.length === 0) return null;
  return (
    <details
      className="min-w-0 border border-edge-subtle bg-surface-1"
      data-work-accounting-coverage={cards.length}
    >
      <summary className={SUMMARY_CLASS}>
        Coverage · {cards.length} dimensions this ledger does not decode
      </summary>
      <div className="flex min-w-0 flex-col gap-2 px-2.5 pb-2.5">
        <p className="text-3xs leading-snug text-text-muted">
          Each dimension below belongs to an event family Plan 26 projects through
          ExecutionTopologyMetricsV1; {METRICS_GAP_REASON}. Its headline, rows, and all seven
          provenance facets are unsupported here.
        </p>
        <ul aria-label="Dimensions this ledger does not decode" className="flex min-w-0 flex-col gap-2">
          {cards.map((card) => (
            <li
              key={card.dimension}
              className="flex min-w-0 flex-col gap-0.5 border-l border-edge-subtle pl-2"
              data-work-accounting={card.dimension}
              data-work-accounting-reading="absent"
            >
              <h4 className="td-legend truncate text-text-secondary">{card.title}</h4>
              <p className="text-3xs leading-snug text-text-muted">Plan 26 asks for: {card.mandate}</p>
              <p className="text-3xs leading-snug text-text-muted">
                Event family: <code>{accountingEventKind(card.dimension)}</code>
              </p>
              <p className="text-3xs leading-snug text-text-secondary">
                Not decoded: {undecodedMeasures(card).join(' · ')}
              </p>
              {card.caveat === undefined ? null : (
                <p className="text-3xs leading-snug text-text-muted">{card.caveat}</p>
              )}
            </li>
          ))}
        </ul>
      </div>
    </details>
  );
}

function undecodedMeasures(card: WorkAccountingCard): string[] {
  const measures = [card.title.toLowerCase(), ...card.rows.map((row) => row.label.toLowerCase())];
  const matrices = card.matrices ?? [];
  if (matrices.length > 0) {
    const cells = matrices.reduce((total, matrix) => total + matrix.cells.length, 0);
    measures.push(
      `${matrices.map((matrix) => matrix.kind).join(' and ')} predicted × observed matrices, ${cells} cells`,
    );
  }
  return measures;
}

interface UndecodedSlot {
  readonly key: string;
  readonly label: string;
  readonly detail: string;
  readonly marker: Readonly<Record<string, string>>;
}

function UndecodedSlots({ card, slots }: { card: WorkAccountingCard; slots: readonly UndecodedSlot[] }) {
  if (slots.length === 0) return null;
  return (
    <details
      className="min-w-0 border border-edge-subtle bg-surface-1"
      data-work-accounting-undecoded={card.dimension}
    >
      <summary className={SUMMARY_CLASS}>
        Not decoded here · {slots.length} {slots.length === 1 ? 'measure' : 'measures'}
      </summary>
      <ul
        aria-label={`Measures ${card.title} does not decode`}
        className="flex min-w-0 flex-col gap-1.5 px-2.5 pb-2.5"
      >
        {slots.map((slot) => (
          <li
            key={slot.key}
            className="flex min-w-0 flex-col gap-0.5 border-l border-edge-subtle pl-2"
            {...slot.marker}
          >
            <span className="text-2xs text-text-secondary">{slot.label}</span>
            <p className="text-3xs leading-snug text-text-muted">{slot.detail}</p>
          </li>
        ))}
      </ul>
    </details>
  );
}

function AccountingCard({ card }: { card: WorkAccountingCard }) {
  // The ceiling every measurable row in this card is drawn against. Taken from
  // the card's own readable figures, so a card with one readable row draws that
  // row full-length rather than borrowing a scale from a card beside it.
  const ceiling = card.rows.reduce(
    (widest, row) => (row.channel.available ? Math.max(widest, row.channel.value.value) : widest),
    0,
  );
  const rows = card.rows.filter((row) => !isUndecoded(row.channel));
  const facets = facetSlots(card.provenance);
  const undecoded: UndecodedSlot[] = [
    ...card.rows.flatMap(({ key, label, channel }) =>
      isUndecoded(channel)
        ? [
            {
              key: `row:${key}`,
              label,
              detail: channel.detail,
              marker: { 'data-work-accounting-row': key, 'data-work-accounting-absent': channel.state },
            },
          ]
        : [],
    ),
    ...facets.flatMap(({ name, label, channel }) =>
      isUndecoded(channel)
        ? [
            {
              key: `facet:${name}`,
              label,
              detail: channel.detail,
              marker: { 'data-work-accounting-facet': name, 'data-work-accounting-facet-state': 'absent' },
            },
          ]
        : [],
    ),
  ];

  return (
    <section
      aria-label={card.title}
      className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-1 p-2.5"
      data-work-accounting={card.dimension}
      data-work-accounting-reading={card.reading.available ? 'available' : 'absent'}
    >
      <div className="flex min-w-0 flex-col gap-0.5">
        <h4 className="td-legend truncate text-text-secondary">{card.title}</h4>
        <p className="text-3xs leading-snug text-text-muted">Plan 26 asks for: {card.mandate}</p>
      </div>

      {card.reading.available ? (
        <p
          className="text-2xs leading-snug text-text-primary"
          data-work-accounting-headline={card.dimension}
        >
          {card.reading.value}
        </p>
      ) : (
        <ChannelAbsence measure={card.title.toLowerCase()} channel={card.reading} />
      )}

      {card.contradictions.map((contradiction) => (
        <div
          key={contradiction.key}
          className="flex min-w-0 flex-col gap-1"
          data-work-accounting-contradiction={contradiction.key}
        >
          <StateChip kind={contradiction.state} detail="the record contradicts itself" />
          <p className="text-3xs leading-snug text-text-muted">{contradiction.detail}</p>
        </div>
      ))}

      {rows.length > 0 ? (
        <ul className="flex min-w-0 flex-col gap-1.5">
          {rows.map((row) => (
            <li key={row.key} className="min-w-0">
              <AccountingRow row={row} ceiling={ceiling} />
            </li>
          ))}
        </ul>
      ) : null}

      {card.matrices?.map((matrix) => <ConfusionMatrix key={matrix.kind} matrix={matrix} />)}

      <ProvenanceFooter
        dimension={card.dimension}
        facets={facets.filter((facet) => !isUndecoded(facet.channel))}
      />
      <UndecodedSlots card={card} slots={undecoded} />
    </section>
  );
}

function figureText(figure: WorkAccountingFigure): string {
  return `${figure.value} ${figure.unit}`;
}

function AccountingRow({ row, ceiling }: { row: WorkAccountingRow; ceiling: number }) {
  return (
    <div
      className="flex min-w-0 flex-col gap-1 border-l border-edge-subtle pl-2"
      data-work-accounting-row={row.key}
      {...(row.channel.available
        ? { 'data-work-accounting-value': String(row.channel.value.value) }
        : { 'data-work-accounting-absent': row.channel.state })}
    >
      <div className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <span className="min-w-0 flex-1 truncate text-2xs text-text-secondary">{row.label}</span>
        {row.channel.available ? (
          <span className="td-value shrink-0 text-2xs text-text-primary" data-cell="numeric">
            {figureText(row.channel.value)}
          </span>
        ) : null}
      </div>

      {row.channel.available ? (
        <>
          {/* Drawn only beside a figure that exists, and only when the card has
            * a non-zero ceiling to measure against. A bar under an absent row
            * would be a zero the read never produced. */}
          {ceiling > 0 ? <Meter fraction={row.channel.value.value / ceiling} height="row" /> : null}
          {row.channel.value.note === undefined ? null : (
            <p className="text-3xs leading-snug text-text-muted">{row.channel.value.note}</p>
          )}
        </>
      ) : (
        <ChannelAbsence measure={row.label.toLowerCase()} channel={row.channel} />
      )}
    </div>
  );
}

const CELL_LABEL: Record<string, string> = {
  conflict: 'conflict',
  no_conflict: 'no conflict',
  abstained: 'abstained',
  unknown: 'unknown',
};

/**
 * One confusion matrix, cell by cell.
 *
 * Rendered as a table so every cell has a row header and a column header a
 * screen reader can announce; the visual grid is not the only place the cell's
 * identity lives. Each cell renders its own state, there is no aggregate row,
 * no aggregate column, and no derived scalar, because the plan keeps these
 * cells separate and the moment one number summarised them the separation
 * would be gone.
 */
function ConfusionMatrix({ matrix }: { matrix: WorkAccountingMatrix }) {
  const predicted = [...new Set(matrix.cells.map((cell) => cell.predicted))];
  const observed = [...new Set(matrix.cells.map((cell) => cell.observed))];
  const at = (p: string, o: string) =>
    matrix.cells.find((cell) => cell.predicted === p && cell.observed === o);

  return (
    <div className="min-w-0 overflow-x-auto" data-work-accounting-matrix={matrix.kind}>
      <table className="w-full min-w-0 border-collapse text-3xs">
        <caption className="td-legend py-1 text-left text-text-secondary">
          {matrix.kind} conflict prediction · predicted down, observed across · every cell separate
        </caption>
        <thead>
          <tr>
            <th scope="col" className="border border-edge-subtle p-1 text-left text-text-muted">
              predicted \ observed
            </th>
            {observed.map((column) => (
              <th
                key={column}
                scope="col"
                className="border border-edge-subtle p-1 text-left text-text-muted"
              >
                {CELL_LABEL[column] ?? column}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {predicted.map((row) => (
            <tr key={row}>
              <th scope="row" className="border border-edge-subtle p-1 text-left text-text-muted">
                {CELL_LABEL[row] ?? row}
              </th>
              {observed.map((column) => {
                const cell = at(row, column);
                return (
                  <td
                    key={column}
                    className="border border-edge-subtle p-1 align-top"
                    data-work-accounting-cell={`${row}:${column}`}
                  >
                    {cell === undefined ? (
                      <StateChip kind="unknown" detail="cell not carried" />
                    ) : cell.channel.available ? (
                      <span className="td-value" data-cell="numeric">
                        {figureText(cell.channel.value)}
                      </span>
                    ) : (
                      <StateChip kind={cell.channel.state} detail="not measured" />
                    )}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

interface FacetSlot {
  readonly name: string;
  readonly label: string;
  readonly channel: WorkChannel<string>;
}

function facet<T>(
  name: string,
  label: string,
  channel: WorkChannel<T>,
  format: (value: T) => string,
): FacetSlot {
  return {
    name,
    label,
    channel: channel.available ? { available: true, value: format(channel.value) } : channel,
  };
}

function facetSlots(provenance: WorkAccountingProvenance): FacetSlot[] {
  return [
    facet('support', 'Support', provenance.support, figureText),
    facet('eligible', 'Eligible denominator', provenance.eligible, figureText),
    facet(
      'censoring',
      'Censoring / unknowns',
      provenance.censoring,
      (value) => `${value.censored} censored · ${value.unknown} unknown`,
    ),
    facet('intervalCoverage', 'Interval coverage', provenance.intervalCoverage, (value) => value),
    facet('horizon', 'Horizon', provenance.horizon, (value) => value),
    facet('descriptorRevision', 'Descriptor revision', provenance.descriptorRevision, (value) =>
      value.kind === 'metric_descriptor'
        ? value.value
        : `source read pin, ${value.value} (not a metric descriptor revision)`,
    ),
    facet('anchors', 'Safe anchors', provenance.anchors, (anchors) =>
      anchors.length === 0
        ? 'the read carried none'
        : anchors.map((anchor) => `${anchor.kind} ${anchor.id}`).join(' · '),
    ),
  ];
}

function ProvenanceFooter({
  dimension,
  facets,
}: {
  dimension: string;
  facets: readonly FacetSlot[];
}) {
  if (facets.length === 0) return null;
  return (
    <dl
      className="grid min-w-0 grid-cols-1 gap-x-3 gap-y-1.5 border-t border-edge-subtle pt-2 sm:grid-cols-2 lg:grid-cols-4"
      aria-label={`Provenance for ${dimension}`}
      data-work-accounting-provenance={dimension}
    >
      {facets.map(({ name, label, channel }) => (
        <div
          key={name}
          className="flex min-w-0 flex-col gap-0.5"
          data-work-accounting-facet={name}
          data-work-accounting-facet-state={channel.available ? 'available' : 'absent'}
        >
          <dt className="td-legend truncate text-text-muted">{label}</dt>
          <dd className="min-w-0 text-3xs leading-snug text-text-secondary">
            {channel.available ? (
              channel.value
            ) : (
              <span className="flex min-w-0 flex-col gap-0.5">
                <StateChip kind={channel.state} detail={label.toLowerCase()} />
                <span className="text-text-muted">{channel.detail}</span>
              </span>
            )}
          </dd>
        </div>
      ))}
    </dl>
  );
}
