/**
 * The constellation renderer exploration: three candidate drawings of the
 * same fact scene, selected with `?constellation=`. Without the parameter the
 * Facts camera keeps the polar `FactConstellation`, so the choice is
 * reversible by deleting the losing renderer files and their entry here.
 */
export const CONSTELLATION_VARIANTS = ['cameras', 'field', 'lattice'] as const;
export type ConstellationVariant = (typeof CONSTELLATION_VARIANTS)[number];

export const CONSTELLATION_VARIANT_PARAM = 'constellation';

export function parseConstellationVariant(value: string | null): ConstellationVariant | null {
  return CONSTELLATION_VARIANTS.find((variant) => variant === value) ?? null;
}
