// Shared primitives for the holographic-memory dashboard surface.

// Legible numeric badges: the shared Badge's compressed display font + wide
// tracking is hard to read for small numbers.
export const NUM_BADGE = "text-xs font-mono-ui tracking-normal text-foreground";

export function truncate(value: string, max = 120): string {
  return value.length > max ? `${value.slice(0, max - 1)}…` : value;
}
