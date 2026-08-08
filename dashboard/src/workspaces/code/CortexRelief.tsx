/**
 * CORTEX — the macro sheet of the structure LENS (plan 11b `:134`, `:214`,
 * `:229` "Synthesis: ONE navigable space. Far = CORTEX. Touch a symbol = TRACE
 * floods. Enter a file = CORE SAMPLE").
 *
 * This is the far end of the same continuum the LENS ruler already carries, not
 * a page of its own: it renders inside the `cortex` position above the
 * connectivity spine, and it takes the workspace's current symbol focus so the
 * reader can see WHERE on the terrain the thing they are tracing lives. Moving
 * the ruler to TRACE or CORE from here changes altitude over the same identity.
 *
 * The division of labour is the plan's honesty boundary (`:196`):
 *
 *   `cortexRelief.ts`   wire measurement → positions, areas, contour counts.
 *                       Every figure printed here comes from there.
 *   `cortexRender.ts`   draws. Decides nothing.
 *   this file           composes the two, and is responsible for one thing of
 *                       its own: saying out loud what the picture is and — the
 *                       harder half — what it is NOT showing.
 *
 * ACCESSIBILITY. The canvas is one `role="img"` with a description carrying the
 * same claims as the caption, and the region table is its exact equivalent:
 * every region in the measurement is in it as text, including the ones the
 * drawing cap folds out, each with the numbers the field encodes as position,
 * area and ring count. That pairing is a property of this composition and of
 * nothing smaller, which is why the two are always rendered together, from the
 * one model, and never conditionally.
 */
import { useMemo, useState } from 'react';

import {
  StructureReadV12Schema as StrataReadSchema,
  type StrataMeasurementV1,
} from '../../contracts/generated.ts';
import { absenceReason, useStructure } from '../../data/query/structure.ts';
import { CenteredState } from '../../ui/ReadSection.tsx';
import { Readout } from '../../ui/instrument.tsx';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { cn } from '../../ui/cn';
import { CortexCanvas } from './CortexCanvas.tsx';
import {
  MAX_DRAWN_REGIONS,
  buildCortexModel,
  cortexAbsences,
  cortexLegendPanels,
  directoryOf,
  type CortexModel,
  type CortexRegion,
} from './cortexRelief.ts';

const BASE = '/api/plugins/graph';

export function CortexRelief({ focusPath }: { focusPath?: string | null }) {
  const strata = useStructure<StrataMeasurementV1>(
    ['graph', 'strata'],
    `${BASE}/strata`,
    StrataReadSchema,
  );

  return (
    <section
      className="flex flex-col border-b border-edge-subtle"
      aria-label="CORTEX module relief"
    >
      <div className="flex flex-wrap items-baseline gap-2 border-b border-edge-subtle px-3 py-2">
        <h2 className="td-title">CORTEX</h2>
        <span aria-hidden className="td-rule" />
        <span className="td-legend normal-case tracking-normal text-text-muted">
          the indexed repository as continuous relief · elevation is dependency depth,
          area is file mass, contour density is internal connectivity
        </span>
      </div>
      {strata.isPending ? (
        <CenteredState title="Reading the dependency strata" kind="loading" />
      ) : strata.data === undefined ? (
        <CenteredState
          title="The strata read recorded no response, so no terrain is drawn"
          kind="unknown"
        />
      ) : strata.data.outcome !== 'measured' ? (
        // No falsified terrain: an unavailable read is an unavailable read, and
        // never an empty-but-successful field of flat ground.
        <CenteredState
          title="The dependency strata could not be measured, so no terrain is drawn"
          kind={strata.data.outcome === 'transport' ? 'unknown' : 'unavailable'}
          detail={absenceReason(strata.data) ?? undefined}
        />
      ) : (
        <ReliefSheet
          measurement={strata.data.measurement}
          focusPath={focusPath ?? null}
        />
      )}
    </section>
  );
}

function ReliefSheet({
  measurement,
  focusPath,
}: {
  measurement: StrataMeasurementV1;
  focusPath: string | null;
}) {
  const model = useMemo(() => buildCortexModel(measurement), [measurement]);
  const [selected, setSelected] = useState<string | null>(null);
  const focusedDirectory = focusPath === null ? null : directoryOf(focusPath);
  const selectedRegion =
    model.regions.find((region) => region.directory === selected) ?? null;

  if (model.regions.length === 0) {
    return (
      <CenteredState
        title="The strata scan clustered no directories in this graph"
        kind="complete_zero_findings"
      />
    );
  }

  return (
    <div className="flex flex-col">
      <ReliefPlate model={model} />
      <figure className="flex flex-col gap-2 border-b border-edge-subtle px-3 pb-2 pt-2">
        <CortexCanvas
          model={model}
          selected={selected}
          focusedDirectory={focusedDirectory}
          onSelect={setSelected}
        />
        <figcaption className="flex flex-col gap-2" data-testid="cortex-key">
          <ReliefLegend model={model} />
          <ReliefAbsences model={model} />
        </figcaption>
      </figure>
      {focusedDirectory !== null ? (
        <p className="border-b border-edge-subtle px-3 py-1.5 text-3xs leading-relaxed text-text-muted">
          the symbol this workspace is focused on lives in{' '}
          <span className="td-value text-text-secondary">{focusedDirectory}</span> —{' '}
          {model.drawnRegions.some((region) => region.directory === focusedDirectory)
            ? 'that region is ringed on the relief above. Move the lens to TRACE to flood it.'
            : 'that directory is not one of the drawn regions, so nothing is ringed above; its row is in the table below if the scan reached it.'}
        </p>
      ) : null}
      {selectedRegion !== null ? <SelectedRegion region={selectedRegion} /> : null}
      <RegionTable model={model} selected={selected} onSelect={setSelected} />
    </div>
  );
}

/** The plate above the field. Every figure is counted from the model, which is
 * the same record the canvas draws, so the two cannot disagree. */
function ReliefPlate({ model }: { model: CortexModel }) {
  const cells: { label: string; value: string; unit?: string; note?: string }[] = [
    {
      label: 'module regions',
      value: model.totalRegions.toLocaleString(),
      unit: 'dirs',
      note: `${model.drawnRegions.length} drawn`,
    },
    {
      label: 'depth strata',
      value: `0 – ${model.maxDepth}`,
      note: `${model.idealDepth} ideal`,
    },
    {
      label: 'contour interval',
      value: '0.50',
      unit: 'edges / file',
      note: 'index every 5th',
    },
    {
      label: 'aggregation',
      value: `${model.drawnRegions.length} ⟵ ${model.drawnFiles.toLocaleString()}`,
      unit: 'files',
      note: `cap ${MAX_DRAWN_REGIONS} regions`,
    },
    {
      label: 'folded out',
      value: model.foldedRegions.toLocaleString(),
      unit: 'regions',
      note:
        model.foldedRegions === 0
          ? 'the whole clustering is drawn'
          : `${model.foldedFiles.toLocaleString()} files, all in the table`,
    },
  ];
  return (
    <div className="flex flex-col" data-testid="cortex-readout">
      <dl className="grid grid-cols-2 border-b border-edge-subtle sm:grid-cols-3 lg:grid-cols-5">
        {cells.map((cell) => (
          <div key={cell.label} className="min-w-0 border-l border-edge-subtle px-3 py-2 first:border-l-0">
            <Readout
              label={cell.label}
              value={cell.value}
              {...(cell.unit ? { unit: cell.unit } : {})}
              {...(cell.note ? { note: cell.note } : {})}
              size="sm"
            />
          </div>
        ))}
      </dl>
      {model.capped ? (
        <p className="border-b border-edge-subtle px-3 py-1.5 text-3xs leading-relaxed text-state-unknown">
          the scan stopped at its budget — {model.scan.max_files.toLocaleString()} files and{' '}
          {model.scan.max_dependency_edges.toLocaleString()} dependency edges in{' '}
          {model.scan.budget_ms} ms — so this terrain is the part of the repository the scan
          reached, and the strata above are a floor rather than a depth
        </p>
      ) : null}
      {model.unplacedRegions > 0 ? (
        <p className="border-b border-edge-subtle px-3 py-1.5 text-3xs leading-relaxed text-state-unknown">
          {model.unplacedRegions}{' '}
          {model.unplacedRegions === 1 ? 'region carries' : 'regions carry'} no measured
          dependency depth for any of their files, so they have no elevation and are not
          placed on the relief. They are in the table below with their stratum printed as
          absent rather than as zero
        </p>
      ) : null}
    </div>
  );
}

function ReliefLegend({ model }: { model: CortexModel }) {
  return (
    <dl className="grid grid-cols-1 gap-x-3 gap-y-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-6">
      {cortexLegendPanels(model).map((panel) => (
        <div key={panel.label} className="flex min-w-0 flex-col gap-1">
          <dt className="flex items-center gap-1.5">
            <span className="td-legend whitespace-normal">{panel.label}</span>
            <span aria-hidden className="td-rule" />
          </dt>
          <dd className="flex min-w-0 flex-col gap-1">
            <span className="td-value text-2xs text-text-secondary">{panel.reading}</span>
            <span className="text-3xs leading-snug text-text-muted">{panel.teach}</span>
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** The channels the reference sheet carries that this read does not back.
 * Printed as its own block rather than omitted, because a reader who knows the
 * form expects heat and rivers and has to be told why there are none. */
function ReliefAbsences({ model }: { model: CortexModel }) {
  return (
    <div className="flex flex-col gap-1 border-t border-edge-subtle pt-2">
      <span className="td-legend text-state-unknown">not on this sheet</span>
      <dl className="grid grid-cols-1 gap-x-3 gap-y-1.5 sm:grid-cols-3">
        {cortexAbsences(model).map((panel) => (
          <div key={panel.label} className="flex min-w-0 flex-col">
            <dt className="td-legend normal-case tracking-normal text-text-muted">
              {panel.label} — <span className="text-state-unknown">{panel.reading}</span>
            </dt>
            <dd className="text-3xs leading-snug text-text-muted">{panel.teach}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

function SelectedRegion({ region }: { region: CortexRegion }) {
  return (
    <div
      data-testid="cortex-selected"
      className="flex flex-wrap items-baseline gap-x-4 gap-y-1 border-b border-edge-subtle bg-surface-1 px-3 py-2"
    >
      <span
        aria-hidden
        className="size-2 shrink-0 rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
        style={kindColorVars(region.directory)}
      />
      <span className="td-value min-w-0 flex-1 truncate text-2xs text-text-primary">
        {region.directory}
      </span>
      <span className="text-3xs text-text-muted">
        stratum{' '}
        <span className="td-value text-text-secondary">
          {region.depth ?? 'absent'}
        </span>
        {region.depthMin !== null && region.depthMax !== null
          ? ` (files span ${region.depthMin}–${region.depthMax})`
          : ''}{' '}
        · <span className="td-value text-text-secondary">{region.fileCount}</span> files ·{' '}
        <span className="td-value text-text-secondary">{region.internalEdges}</span> internal ·{' '}
        <span className="td-value text-text-secondary">{region.density.toFixed(2)}</span> e/file ·{' '}
        <span className="td-value text-text-secondary">{region.contours}</span> contours ·{' '}
        <span className="td-value text-text-secondary">{region.incomingEdges}</span> in /{' '}
        <span className="td-value text-text-secondary">{region.outgoingEdges}</span> out
      </span>
    </div>
  );
}

/** The accessible equivalent of the relief: every region in the measurement,
 * drawn or folded, with the numbers the field encodes. */
function RegionTable({
  model,
  selected,
  onSelect,
}: {
  model: CortexModel;
  selected: string | null;
  onSelect: (directory: string | null) => void;
}) {
  return (
    <div className="max-h-96 overflow-auto">
      <table className="w-full text-left text-2xs">
        <caption className="td-legend border-b border-edge-subtle px-3 py-2 text-left normal-case tracking-normal text-text-muted">
          Every module region the strata scan clustered — {model.totalRegions.toLocaleString()}{' '}
          directories over {model.totalFiles.toLocaleString()} files — including the{' '}
          {model.foldedRegions.toLocaleString()} the drawing cap folds out. Ordered by{' '}
          {model.clusterOrdering}.
        </caption>
        <thead className="sticky top-0 bg-surface-1">
          <tr className="border-b border-edge-subtle">
            <th className="td-legend px-3 py-2">region</th>
            <th className="td-legend px-3 py-2">stratum</th>
            <th className="td-legend px-3 py-2">files</th>
            <th className="td-legend px-3 py-2">internal</th>
            <th className="td-legend px-3 py-2">e / file</th>
            <th className="td-legend px-3 py-2">contours</th>
            <th className="td-legend px-3 py-2">in</th>
            <th className="td-legend px-3 py-2">out</th>
            <th className="td-legend px-3 py-2">on relief</th>
          </tr>
        </thead>
        <tbody>
          {model.regions.map((region) => (
            <tr
              key={region.directory}
              className={cn(
                'border-b border-edge-subtle',
                selected === region.directory && 'bg-surface-2',
              )}
            >
              <td className="max-w-72 px-3 py-1.5">
                <button
                  type="button"
                  onClick={() =>
                    onSelect(selected === region.directory ? null : region.directory)
                  }
                  aria-pressed={selected === region.directory}
                  className="flex w-full min-w-0 items-center gap-1.5 text-left"
                >
                  <span
                    aria-hidden
                    className="size-1.5 shrink-0 rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
                    style={kindColorVars(region.directory)}
                  />
                  <span className="min-w-0 truncate font-mono text-text-primary">
                    {region.directory}
                  </span>
                </button>
              </td>
              <td className="px-3 py-1.5 tabular-nums text-text-secondary">
                {region.depth === null ? (
                  <span className="text-state-unknown">absent</span>
                ) : region.depthMin !== null &&
                  region.depthMax !== null &&
                  region.depthMin !== region.depthMax ? (
                  `${region.depth} (${region.depthMin}–${region.depthMax})`
                ) : (
                  region.depth
                )}
              </td>
              <td className="px-3 py-1.5 tabular-nums text-text-secondary">
                {region.fileCount.toLocaleString()}
              </td>
              <td className="px-3 py-1.5 tabular-nums text-text-secondary">
                {region.internalEdges.toLocaleString()}
              </td>
              <td className="px-3 py-1.5 tabular-nums text-text-secondary">
                {region.density.toFixed(2)}
              </td>
              <td className="px-3 py-1.5 tabular-nums text-text-secondary">
                {region.contours === 0 ? (
                  <span className="text-state-unknown">no relief</span>
                ) : (
                  region.contours
                )}
              </td>
              <td className="px-3 py-1.5 tabular-nums text-text-muted">
                {region.incomingEdges.toLocaleString()}
              </td>
              <td className="px-3 py-1.5 tabular-nums text-text-muted">
                {region.outgoingEdges.toLocaleString()}
              </td>
              <td className="px-3 py-1.5 text-text-muted">
                {region.drawn ? 'drawn' : 'folded'}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
