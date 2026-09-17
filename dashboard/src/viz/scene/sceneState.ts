/**
 * The per-body state channels the scene runtime samples every frame, and the
 * decision of whether another frame is owed — kept pure so the two product
 * rules the renderer must never break can be asserted without a GPU:
 * inspection never writes the heat channel, and reduced motion never asks for
 * an animation frame.
 */

export interface BodyStateInput {
  /** Admitted heat from the activation field, 0..1. The ONLY heat source. */
  readonly heat: number;
  /** Recency, 0..1. */
  readonly vitality: number;
  /** Whether this body is the inspected one or joined to it by a drawn path.
   * `null` when nothing is inspected. */
  readonly inFocusNeighborhood: boolean | null;
  /** Eased inspection strength, 0..1. */
  readonly focusT: number;
  /** Whether this body is the inspected one. */
  readonly shown: boolean;
  /** Whether a repository emphasis is active and this body is outside it.
   * `null` when no emphasis is active. */
  readonly outsideEmphasis: boolean | null;
}

export interface BodyState {
  readonly heat: number;
  readonly dim: number;
  readonly raise: number;
  readonly vitality: number;
}

/** Unrelated bodies dim only enough to establish focus; they stay legible. */
export const FOCUS_DIM = 0.62;
/** Bodies outside a focused repository recede to context. */
export const EMPHASIS_DIM = 0.78;

export function bodyState(input: BodyStateInput): BodyState {
  const focusDim = input.inFocusNeighborhood === false ? input.focusT * FOCUS_DIM : 0;
  const emphasisDim = input.outsideEmphasis === true ? EMPHASIS_DIM : 0;
  return {
    heat: clamp(input.heat),
    dim: clamp(Math.max(focusDim, emphasisDim)),
    raise: input.shown ? clamp(input.focusT) : 0,
    vitality: clamp(input.vitality),
  };
}

/** Quantise a channel to the byte the state texture carries. */
export function channelByte(value: number): number {
  return Math.round(clamp(value) * 255);
}

export interface FramePolicyInput {
  readonly warm: boolean;
  readonly focusSettled: boolean;
  readonly cameraMoving: boolean;
  readonly reduced: boolean;
}

/** Whether the loop asks for another animation frame. Reduced motion never
 * does: it composes static frames on demand instead. */
export function wantsNextFrame(input: FramePolicyInput): boolean {
  if (input.reduced) return false;
  return input.warm || !input.focusSettled || input.cameraMoving;
}

function clamp(value: number): number {
  return Math.max(0, Math.min(1, value));
}
