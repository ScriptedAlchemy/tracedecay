import type { ProjectBody } from "../data/fixtures";

/**
 * Presentation offsets inside a measured recency column, not new recency data.
 * Stable identity order prevents a registry reorder from moving project X positions.
 * Keep centers in the central 80% of the band; body/label overlap is separate.
 */
export function recencyPeerOffsets(
  projects: readonly Pick<ProjectBody, "id" | "recency">[],
  columnWidth: number,
): Map<string, number> {
  const buckets = new Map<ProjectBody["recency"], string[]>();
  for (const project of projects) {
    const ids = buckets.get(project.recency);
    if (ids) ids.push(project.id);
    else buckets.set(project.recency, [project.id]);
  }

  const offsets = new Map<string, number>();
  for (const ids of buckets.values()) {
    // Compare code units rather than locale-dependent collation.
    ids.sort((a, b) => a < b ? -1 : a > b ? 1 : 0);
    const spacing = Math.min(112, columnWidth * 0.8 / Math.max(1, ids.length - 1));
    for (let slot = 0; slot < ids.length; slot++) {
      offsets.set(ids[slot], (slot - (ids.length - 1) / 2) * spacing);
    }
  }
  return offsets;
}
