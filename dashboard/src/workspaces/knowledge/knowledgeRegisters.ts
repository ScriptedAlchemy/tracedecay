/**
 * The registers the Knowledge workspace posts to the shell's status strip:
 * the memory overview read, its graph sub-read, and the camera position , 
 * the three the plate names beside the transport cells. Each is the
 * authority's own word plus one qualifier; none is inferred from another,
 * and one camera's answer never stands in for another's. Pure, so the strip's
 * wording can be pinned by a test without mounting the page.
 */
import type {
  DashboardEnvelopeV1,
  MemoryOverviewPayloadV1,
  MemoryReadStatusV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { StatusRegister } from '../../data/shell/statusRegisters.ts';
import { knowledgeViewLabel, type KnowledgeViewKind } from './KnowledgeViews.tsx';

const OWNER = 'knowledge';

/** An envelope read as one register: pending, transport, or the daemon's own
 * domain state with the qualifier the payload supplies. A null payload , 
 * which the envelope ladder reports as a transport outcome, never reaches
 * `ready`. */
function envelopeRegister<T>(
  id: string,
  label: string,
  pending: boolean,
  result: EnvelopeResult<T> | undefined,
  ready: (envelope: DashboardEnvelopeV1<T>) => string | undefined,
): StatusRegister {
  if (pending) return { id, label, value: 'loading', state: 'loading' };
  if (result === undefined) {
    return { id, label, value: 'unknown', state: 'unknown', detail: 'no response recorded' };
  }
  if (result.outcome === 'transport') {
    return { id, label, value: result.state, state: result.state, detail: result.detail };
  }
  return {
    id,
    label,
    value: result.envelope.domain_state,
    state: result.envelope.domain_state,
    detail: ready(result.envelope),
  };
}

/** The memory overview envelope: the read every other Facts register hangs off. */
export function memoryRegister(
  pending: boolean,
  result: EnvelopeResult<MemoryOverviewPayloadV1> | undefined,
): StatusRegister {
  return envelopeRegister(`${OWNER}:memory`, 'Memory', pending, result, (envelope) => {
    const holographic = envelope.payload.holographic;
    if (holographic.error) return holographic.error;
    const facts = holographic.overview?.facts;
    return facts == null ? undefined : `${facts.toLocaleString()} facts`;
  });
}

/** One of the overview's sub-reads, in the daemon's own state and code. The
 * sub-read cannot be more available than the envelope that carries it, so a
 * pending or refused overview is reported here in the same words. */
function subReadRegister(
  id: string,
  label: string,
  pending: boolean,
  overview: EnvelopeResult<MemoryOverviewPayloadV1> | undefined,
  pick: (payload: MemoryOverviewPayloadV1) => MemoryReadStatusV1 | undefined,
  qualify: (payload: MemoryOverviewPayloadV1) => string,
): StatusRegister {
  if (pending || overview === undefined || overview.outcome === 'transport') {
    return envelopeRegister(id, label, pending, overview, () => undefined);
  }
  const payload = overview.envelope.payload;
  return readRegister(id, label, pick(payload), qualify(payload));
}

function readRegister(
  id: string,
  label: string,
  read: MemoryReadStatusV1 | undefined,
  qualifier: string,
): StatusRegister {
  if (!read) return { id, label, value: 'unknown', state: 'unknown', detail: 'sub-read not reported' };
  // The daemon's error sentence outranks the measured qualifier; its code
  // does not, the aperture's coverage footer prints the code beside the
  // reasons, and the strip has room for a word and a count.
  return {
    id,
    label,
    value: read.state,
    state: read.state,
    detail: read.error ?? qualifier,
  };
}

export function graphRegister(
  pending: boolean,
  overview: EnvelopeResult<MemoryOverviewPayloadV1> | undefined,
): StatusRegister {
  return subReadRegister(
    `${OWNER}:graph`,
    'Graph',
    pending,
    overview,
    (payload) => payload.holographic.reads['graph'],
    (payload) => {
      const graph = payload.holographic.graph;
      return `${graph.root_count.toLocaleString()} roots · ${graph.relation_count.toLocaleString()} relations`;
    },
  );
}

/** The camera position: an identity, not a source state. */
export function cameraRegister(view: KnowledgeViewKind): StatusRegister {
  return {
    id: `${OWNER}:camera`,
    label: 'Camera',
    value: knowledgeViewLabel(view),
    state: 'identity',
  };
}
