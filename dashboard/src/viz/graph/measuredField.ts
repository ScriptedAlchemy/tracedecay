import { axisFrame, nodeHullFrame, type FieldFrame, type PreparedField } from './layout.ts';
import type { FieldExtent } from './types.ts';

/**
 * The measured path: a field whose coordinates are the caller's own
 * measurement.
 *
 * There is no layout step here at all, and that is the point — running a force
 * pass over placed coordinates, or re-centering their components onto a ring,
 * would destroy the very measurement the positions were carrying. Nothing on
 * this path loads a layout engine, because nothing on it has anything to lay
 * out.
 */
export function frameMeasuredField(
  prepared: PreparedField,
  extent: FieldExtent | undefined,
): FieldFrame {
  // A measured field is framed by its AXIS, not by its occupants. Framing
  // the occupants would rescale the picture every time a body enters or
  // leaves a region, and would quietly delete an empty region — which on
  // this kind of field is itself a reading.
  if (extent) return axisFrame(extent);
  // Without a stated axis there is nothing to frame but the bodies, so a
  // measured field with no extent falls back to the same hull an emergent one
  // uses. The caller has said where each body is but not what the frame means.
  return nodeHullFrame(prepared.graph, prepared.realNodes);
}
