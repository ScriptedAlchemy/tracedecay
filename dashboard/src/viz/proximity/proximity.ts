import type {
  FeedbackProximityEncounterV1,
  FeedbackProximityReadPageV1,
  FeedbackProximityReadResultV1,
  FeedbackProximityRelationV1,
} from '../../contracts/index.ts';

export type ProximityTone = 'candidate' | 'overlap' | 'conflict';

export function proximityTone(relation: FeedbackProximityRelationV1): ProximityTone {
  switch (relation.relation_kind) {
    case 'code_neighborhood_candidate':
    case 'shared_code_candidate':
      return 'candidate';
    case 'overlapping_edit':
      return 'overlap';
    case 'confirmed_conflict':
      return 'conflict';
    default: {
      const unhandled: never = relation;
      return unhandled;
    }
  }
}

export function proximityColor(relation: FeedbackProximityRelationV1): string {
  const tone = proximityTone(relation);
  switch (tone) {
    case 'candidate':
      return '#ffc04d';
    case 'overlap':
      return '#ff8a70';
    case 'conflict':
      return '#ff4d4f';
    default: {
      const unhandled: never = tone;
      return unhandled;
    }
  }
}

export function proximityLabel(relation: FeedbackProximityRelationV1): string {
  switch (relation.relation_kind) {
    case 'code_neighborhood_candidate':
    case 'shared_code_candidate':
      return relation.warning_class.replaceAll('_', ' ');
    case 'overlapping_edit':
      return relation.warning_class.replaceAll('_', ' ');
    case 'confirmed_conflict':
      return 'confirmed content conflict';
    default: {
      const unhandled: never = relation;
      return unhandled;
    }
  }
}

export function proximityPage(
  result: FeedbackProximityReadResultV1,
): FeedbackProximityReadPageV1 | null {
  switch (result.state) {
    case 'complete':
    case 'complete_zero':
    case 'partial':
    case 'stale':
      return result.page;
    case 'denied':
    case 'unavailable':
      return null;
    default: {
      const unhandled: never = result;
      return unhandled;
    }
  }
}

export function proximityThreadId(
  encounter: FeedbackProximityEncounterV1,
  participant: number,
): string | null {
  const source = encounter.participants[participant]?.source;
  return source === undefined ? null : JSON.stringify([source.provider, source.session_id]);
}

export function encounterAt(
  encounters: readonly FeedbackProximityEncounterV1[],
  encounterId: string | null,
): FeedbackProximityEncounterV1 | null {
  return encounters.find((encounter) => encounter.encounter_id === encounterId) ?? null;
}
