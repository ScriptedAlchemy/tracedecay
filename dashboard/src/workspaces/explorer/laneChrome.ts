/** Presentation constants shared by every Explorer view: the glyph and the
 * specification behind a lane id, resolved once so no view re-scans `LANES`. */
import { Boxes, Lightbulb, MessagesSquare, Sparkles, type LucideIcon } from 'lucide-react';
import { LANES, type LaneId, type LaneSpec } from './model.ts';

export const LANE_ICON: Record<LaneId, LucideIcon> = {
  code: Boxes,
  sessions: MessagesSquare,
  knowledge: Lightbulb,
  semantic: Sparkles,
};

function laneSpec(id: LaneId): LaneSpec {
  const spec = LANES.find((lane) => lane.id === id);
  if (!spec) throw new Error(`Missing lane specification: ${id}`);
  return spec;
}

export const LANE_BY_ID: Record<LaneId, LaneSpec> = {
  code: laneSpec('code'),
  sessions: laneSpec('sessions'),
  knowledge: laneSpec('knowledge'),
  semantic: laneSpec('semantic'),
};
