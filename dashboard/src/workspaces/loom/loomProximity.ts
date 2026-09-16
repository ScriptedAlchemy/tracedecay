import { useSearchParams } from 'react-router';
import type {
  FeedbackProximityEncounterV1,
  FeedbackProximityReadResultV1,
} from '../../contracts/index.ts';
import {
  proximityPage,
  useFeedbackProximity,
} from '../../viz/proximity/index.ts';
import type { WorkResult } from '../work/workApi.ts';

export interface LoomProximityState {
  readonly encounters: readonly FeedbackProximityEncounterV1[];
  readonly result: WorkResult<FeedbackProximityReadResultV1> | undefined;
  readonly selectedEncounterId: string | null;
  readonly selectEncounter: (encounterId: string | null) => void;
}

export function useLoomProximity(): LoomProximityState {
  const [params, setParams] = useSearchParams();
  const proximity = useFeedbackProximity(true);
  const encounters =
    proximity.data?.outcome === 'value'
      ? proximityPage(proximity.data.value)?.encounters ?? []
      : [];
  const selectEncounter = (encounterId: string | null) => {
    const next = new URLSearchParams(params);
    if (encounterId === null) next.delete('loomEncounter');
    else next.set('loomEncounter', encounterId);
    setParams(next, { replace: true });
  };
  return {
    encounters,
    result: proximity.data,
    selectedEncounterId: params.get('loomEncounter'),
    selectEncounter,
  };
}
