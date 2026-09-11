import type { AttentionItem } from "../app/workspace";

/** Authored fixture evidence only; snapshot mode deliberately contributes none. */
export const AUTOMATIONS_ATTENTION: AttentionItem[] = [
  {
    id: "automation:run-142205:integrity",
    title: "Embeddings artifact needs integrity review",
    detail: "vectors-20250509T142205.zst is present, but its recorded checksum mismatches the authored manifest.",
    source: "workflow",
    severity: "warning",
    status: "active",
    owner: "you",
    evidence: "explicit",
    mode: "fixture",
    observedAt: "2025-05-09 14:22:07 UTC",
    sourceRef: "run-142205 / artifact vectors-20250509T142205.zst",
    repository: "tracedecay",
    target: { surface: "automations", params: { state: "run-142205" } },
  },
];
