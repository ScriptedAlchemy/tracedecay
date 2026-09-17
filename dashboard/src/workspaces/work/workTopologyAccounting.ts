import type {
  ExecutionTopologyMetricsV1,
  ExecutionTopologyViewV1,
  WorkAttemptListV1,
  WorkAttemptTopologyBindingV1,
} from '../../contracts/index.ts';
import type { WorkResult } from './workApi.ts';
import { workAttemptReading } from './workAttemptModel.ts';
import type { WorkGraphReading } from './workGraphModel.ts';
import { attemptCensus } from './workAccountingCensus.ts';
import {
  blockedTimeCard,
  concurrencyCard,
  conflictCard,
  duplicateEffectCard,
  rerunCard,
} from './workAccountingCards.ts';
import {
  githubStackCapabilityCard,
  integrationOutcomesCard,
} from './workAccountingMetrics.ts';
import {
  unavailableCard,
  type WorkAccountingCard,
  type WorkAttemptPageV1,
  type WorkTopologyAccountingReading,
} from './workAccountingModel.ts';

/**
 * Plan 26's execution-topology accounting, over the reads this build has.
 *
 * Plan 26 (`docs/plans/tracedecay-v2/26-observability-accounting-and-usage.md`,
 * the `execution-topology` mandate) asks one product view for twelve
 * dimensions, and attaches a contract to every card that carries one:
 *
 *   > Every card exposes support, eligible denominator, censoring/unknowns,
 *   > interval coverage, horizon, descriptor revision, and safe anchors;
 *   > unsupported or under-floor metrics render unavailable rather than zero.
 *
 * That contract is the whole point of this module, and it is why the cards are
 * built here rather than assembled in JSX. A card is a DATA STRUCTURE with
 * seven provenance facets, each of them a `WorkChannel` in its own right, so a
 * card whose reading is unavailable can still state a real eligible
 * denominator, and a card whose reading is real can still state that its
 * censoring is unknown. The view renders whatever the structure holds and has
 * no way to print a figure the structure did not prove.
 *
 * WHERE THE NUMBERS COME FROM, AND WHERE THEY DO NOT
 *
 * `ExecutionTopologyMetricsV1` is published at `operation.work.topology_metrics`
 * (`/api/work/topology-metrics`), and this ledger consumes its integration and
 * stack families through `workAccountingMetrics.ts`: the
 * `work_merge_attempts_total` kind × outcome cells and the
 * `github_stack_capability` reading, each cell decoded verbatim with the
 * projector's own typed absences. The remaining event-fed dimensions render
 * as typed absences naming the event kind a reviewer can grep for
 * (`EXECUTION_TOPOLOGY_EVENT_KINDS_V1`): this lens does not decode their
 * descriptors, and an absence stated is not a zero shown.
 *
 * Three further dimensions have a real, mounted source on the attempt and
 * graph reads, and those are the cards this module adds to the landed lens:
 *
 *   concurrency        `operation.work.views` →
 *                      `WorkWorkloadProjectionV1.requested_concurrency` and
 *                      `.actual_concurrency`, both nullable because the
 *                      authority withholds them unless the runtime projection
 *                      covered every attempt. Two of the mandate's five rungs;
 *                      accepted, admitted and useful width have no field and
 *                      say so.
 *   runtime reruns     `operation.work.list_attempts` → each attempt's
 *                      `WorkRecoveryStateV1`, bucketed by its
 *                      `WorkRestartReasonV1`. This is the RUNTIME family only:
 *                      test and CI reruns are a different population with no
 *                      read model, and the three are never summed.
 *   duplicate effects  the same page's `WorkEffectStateV1` census. The
 *                      adjudication itself — was one effect committed twice —
 *                      is unavailable, but `compound_non_repeatable` is a real
 *                      ELIGIBLE DENOMINATOR for it, so that card carries a
 *                      denominator under an unavailable reading. That
 *                      asymmetry is the contract working, not a gap in it.
 *
 * WHAT IS DELIBERATELY NOT DONE HERE
 *
 * No formula. Plan 26 prohibits dashboard-side business metrics outright
 * ("Backend and UI adapters render application read models; dashboard formulas
 * are prohibited"), so nothing below divides, rates, or scores anything. Every
 * figure is a count off a decoded contract field, and a ratio the plan asks
 * for is absent rather than computed here.
 *
 * No adjacent measure standing in for a missing one. Blocked EFFORT is
 * readable and blocked TIME is not; the blocked-time card prints the effort
 * figure under its own label as a distinct measure and leaves the mandated one
 * absent. `WorkFallbackTopology` is the provider-executable fallback and not
 * the GitHub generic fallback, so it is named in that card's absence and never
 * counted into it.
 *
 * No collapsed matrix. The mechanical and semantic conflict matrices keep
 * every cell separate and there is no accuracy scalar anywhere, because a
 * single number cannot be un-collapsed once a reader has seen it.
 */

// --- The reading -------------------------------------------------------------

/**
 * The twelve accounting cards, from the mounted Work reads.
 *
 * Takes the raw attempt result rather than a derived reading because the
 * effect and recovery censuses walk the attempts' execution envelopes and
 * recovery records, which the derived attempt page deliberately does not
 * restate. The reading state is still computed once, by `workAttemptReading`,
 * so a refusal is reported in exactly the words every other Work projection
 * reports it in. The canonical topology read supplies the population binding;
 * no attempt census is emitted after that authority identifies a different
 * generation.
 */
export function workTopologyAccounting(
  result: WorkResult<WorkAttemptListV1> | undefined,
  graph: WorkGraphReading,
  topology?: WorkResult<ExecutionTopologyViewV1> | undefined,
  metrics?: WorkResult<ExecutionTopologyMetricsV1> | undefined,
): WorkTopologyAccountingReading {
  const canonicalTopology = topologyBinding(topology);
  const boundAttempts = attemptsBoundToTopology(result, canonicalTopology);
  const reading = workAttemptReading(boundAttempts);
  const page =
    boundAttempts !== undefined &&
    boundAttempts.outcome === 'value' &&
    boundAttempts.value.state === 'listed'
      ? boundAttempts.value
      : null;
  const census = page === null ? null : attemptCensus(page.attempts);
  const boundGraph = graphBoundToTopology(graph, canonicalTopology ?? page?.topology ?? null);

  const cards: WorkAccountingCard[] = [
    concurrencyCard(boundGraph),
    unavailableCard(
      'duplicate_work',
      'independently adjudicated duplicate work',
      [
        { key: 'adjudicated_pairs', label: 'Adjudicated duplicate pairs', measure: 'adjudicated duplicate pairs' },
        { key: 'adjudicator_independence', label: 'Independent adjudications', measure: 'the independent adjudication count' },
      ],
      'Two attempts landing on one task is a retry chain and not duplicate work; the retry weave counts those and is not borrowed here.',
    ),
    conflictCard(),
    unavailableCard('ready_to_integrated_latency', 'ready-to-integrated latency', [
      { key: 'latency_distribution', label: 'Latency distribution', measure: 'the ready-to-integrated latency distribution' },
    ], 'No read in this build carries a duration at all: an attempt records the instant it finished and never the instant it started.'),
    integrationOutcomesCard(metrics),
    unavailableCard('stale_stack_age', 'stale-stack age', [
      { key: 'stack_age', label: 'Stack age distribution', measure: 'the stale-stack age distribution' },
    ]),
    githubStackCapabilityCard(metrics),
    blockedTimeCard(boundGraph),
    rerunCard(reading, page, census),
    duplicateEffectCard(reading, page, census),
    unavailableCard('operational_leaks', 'operational leaks', [
      { key: 'leaks', label: 'Observed leaks', measure: 'observed operational leaks' },
      { key: 'leak_recovery', label: 'Leak recovery', measure: 'the leak recovery disposition' },
    ]),
    unavailableCard('delivery_fanout', 'delivery fanout', [
      { key: 'surfaces', label: 'Delivery surfaces per unit of work', measure: 'the delivery fanout distribution' },
    ]),
  ];

  return {
    cards,
    measured: cards.filter((card) => card.reading.available).length,
  };
}

/**
 * A graph runtime and an attempt page can only describe one execution
 * population when they name the same topology generation. The endpoint reads
 * independently, so an older graph is not a harmless background refresh: its
 * runtime figures are unbound from this page and must stay unavailable.
 */
function topologyBinding(
  topology: WorkResult<ExecutionTopologyViewV1> | undefined,
): WorkAttemptTopologyBindingV1 | null {
  if (topology === undefined || topology.outcome !== 'value' || topology.value.state !== 'view') {
    return null;
  }
  return topology.value.topology;
}

/**
 * The attempt list is a separately refreshed execution population. Once the
 * canonical topology page is available, its generation is the authority that
 * lets this ledger join recovery and effect evidence to the placement lanes.
 * A mismatched attempt page is therefore a typed refusal rather than a source
 * of stale counts.
 */
function attemptsBoundToTopology(
  result: WorkResult<WorkAttemptListV1> | undefined,
  topology: WorkAttemptTopologyBindingV1 | null,
): WorkResult<WorkAttemptListV1> | undefined {
  if (
    topology === null ||
    result === undefined ||
    result.outcome !== 'value' ||
    result.value.state !== 'listed' ||
    result.value.topology.generation === topology.generation
  ) {
    return result;
  }

  return {
    outcome: 'refused',
    state: 'conflicting',
    detail:
      `the attempt page is pinned to topology generation ${result.value.topology.generation}, but the canonical ` +
      `topology page is pinned to ${topology.generation}; their execution populations are unbound, so attempt-derived accounting figures are not rendered`,
  };
}

function graphBoundToTopology(
  graph: WorkGraphReading,
  topology: WorkAttemptTopologyBindingV1 | null,
): WorkGraphReading {
  if (topology === null || graph.state !== 'read' || graph.page.entry === null) return graph;

  const graphGeneration = graph.page.entry.runtime.generation_id;
  if (graphGeneration === topology.generation) return graph;

  return {
    state: 'refused',
    chip: 'conflicting',
    detail:
      `the graph runtime is pinned to topology generation ${graphGeneration}, but the canonical ` +
      `topology page is pinned to ${topology.generation}; their execution populations are unbound, so graph-derived measurements are not rendered`,
  };
}
