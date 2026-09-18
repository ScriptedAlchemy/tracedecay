import type { WorkTimelineLaneV1 } from '../../contracts/index.ts';
import type { WorkTaskLane } from './workProductView.ts';

/**
 * How the authority's projected lane is printed.
 *
 * The lane is the one typed lifecycle state the Work authority derives for a
 * task (`work_product_projection.rs::lane`), and it is printed as the word the
 * authority used. The swatch beside the word follows the design system's
 * typed-state families, green for served ready/complete, amber for measured
 * activity or attention, gray for disconnected or unknown, hollow for a
 * typed absence, and never carries the state alone.
 */
export interface WorkLaneReading {
  /** Uppercase engraved label, exactly the authority's word. */
  readonly label: string;
  /** Utility class for the swatch fill; `null` draws a hollow dashed swatch. */
  readonly swatch: string | null;
  /** One sentence for the inspector and the accessible name. */
  readonly sentence: string;
}

function projectedLane(lane: WorkTimelineLaneV1): WorkLaneReading {
  switch (lane) {
    case 'ready':
      return {
        label: 'READY',
        swatch: 'bg-state-ready',
        sentence: 'ready: every gating dependency is accepted and nothing is running',
      };
    case 'running':
      return {
        label: 'RUNNING',
        swatch: 'bg-alert',
        sentence: 'running: the runtime projection holds a live attempt',
      };
    case 'review':
      return {
        label: 'REVIEW',
        swatch: 'bg-state-partial',
        sentence: 'review: an attempt terminated and the task is not yet accepted',
      };
    case 'blocked':
      return {
        label: 'BLOCKED',
        swatch: 'bg-state-conflicting',
        sentence: 'blocked: a gating dependency is not accepted or an attempt needs recovery',
      };
    case 'scheduled':
      return {
        label: 'SCHEDULED',
        swatch: 'bg-state-loading',
        sentence: 'scheduled: the declared schedule instant is still ahead',
      };
    case 'todo':
      return {
        label: 'TODO',
        swatch: 'bg-edge-strong',
        sentence: 'todo: acceptance criteria are declared and no proposal is accepted',
      };
    case 'triage':
      return {
        label: 'TRIAGE',
        swatch: 'bg-state-unknown',
        sentence: 'triage: no acceptance criteria are declared yet',
      };
    case 'done':
      return {
        label: 'DONE',
        swatch: 'bg-state-complete-zero',
        sentence: 'done: the task is accepted',
      };
    case 'archived':
      return {
        label: 'ARCHIVED',
        swatch: 'bg-state-cancelled',
        sentence: 'archived: the task was archived',
      };
    case 'cancelled':
      return {
        label: 'CANCELLED',
        swatch: 'bg-state-cancelled',
        sentence: 'cancelled: every accepted attempt was cancelled',
      };
    case 'unavailable':
      return {
        label: 'UNAVAILABLE',
        swatch: null,
        sentence:
          'unavailable: the runtime projection could not be read, so the authority declines to place this task in a lane',
      };
    default: {
      const unhandled: never = lane;
      return unhandled;
    }
  }
}

export function laneReading(lane: WorkTaskLane): WorkLaneReading {
  switch (lane.kind) {
    case 'projected':
      return projectedLane(lane.lane);
    case 'uncarded':
      return {
        label: 'NO CARD',
        swatch: null,
        sentence:
          'no lane: the kanban projection in this graph version carries no card for this task, so no lifecycle state is claimed',
      };
    default: {
      const unhandled: never = lane;
      return unhandled;
    }
  }
}
