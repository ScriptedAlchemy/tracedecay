import type { WorkTaskLane } from './workProductView.ts';

/**
 * The design system's typed-state family for a task's lane, and how a card in
 * that family is drawn. `laneReading` owns the word and the swatch; this owns
 * the card's surface treatment, so the family, not the hue, is what a
 * monochrome print still distinguishes:
 *
 *   ready         served ready/complete   solid edge
 *   activity      measured live attempt   solid edge, amber named by legend
 *   degraded      needs attention         hatched face
 *   disconnected  cancelled/unknown/none  dashed edge, muted ink
 *   loading       scheduled ahead         solid edge, ice swatch
 *   neutral       declared, not started   solid edge
 */
export type LaneFamily = 'ready' | 'activity' | 'degraded' | 'disconnected' | 'loading' | 'neutral';

export interface LaneTreatment {
  readonly family: LaneFamily;
  /** CSS colour the hatch and accents are drawn in, or null for none. */
  readonly tone: string | null;
  readonly dashed: boolean;
  readonly hatched: boolean;
}

export function laneTreatment(lane: WorkTaskLane): LaneTreatment {
  if (lane.kind === 'uncarded') {
    return { family: 'disconnected', tone: null, dashed: true, hatched: false };
  }
  switch (lane.lane) {
    case 'ready':
    case 'done':
      return { family: 'ready', tone: 'var(--raw-state-ready)', dashed: false, hatched: false };
    case 'running':
      return { family: 'activity', tone: 'var(--raw-alert)', dashed: false, hatched: false };
    case 'review':
      return { family: 'degraded', tone: 'var(--raw-state-partial)', dashed: false, hatched: true };
    case 'blocked':
      return { family: 'degraded', tone: 'var(--raw-state-conflicting)', dashed: false, hatched: true };
    case 'cancelled':
    case 'archived':
    case 'triage':
    case 'unavailable':
      return { family: 'disconnected', tone: 'var(--raw-state-cancelled)', dashed: true, hatched: false };
    case 'scheduled':
      return { family: 'loading', tone: 'var(--raw-state-loading)', dashed: false, hatched: false };
    case 'todo':
      return { family: 'neutral', tone: null, dashed: false, hatched: false };
    default: {
      const unhandled: never = lane.lane;
      return unhandled;
    }
  }
}

/** A 135° hairline hatch in the family's tone, for a card face or a swatch. */
export function hatchBackground(tone: string, strength = 24): string {
  return `repeating-linear-gradient(135deg, color-mix(in oklch, ${tone} ${strength}%, transparent) 0 1.5px, transparent 1.5px 6px)`;
}
