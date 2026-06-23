import type { MemoryCurateAction } from "../types";

export function describe(a: MemoryCurateAction): string {
  switch (a.op) {
    case "merge":
      return `Merge #${a.loser} → #${a.winner}${a.similarity != null ? ` (sim ${a.similarity})` : ""}`;
    case "entity_merge": {
      const loser = a.loser_entity ?? a.loser ?? a.entity_id;
      const winner = a.winner_entity ?? a.winner ?? a.keep;
      const from = a.loser_name ? `${a.loser_name} (#${loser})` : `#${loser}`;
      const winnerName = a.winner_name ?? a.name;
      const to = winnerName ? `${winnerName} (#${winner})` : `#${winner}`;
      return `Merge entity ${from} → ${to}`;
    }
    case "entity_prune": {
      const entityId = a.entity_id ?? a.loser ?? a.fact_id;
      return a.name ? `Prune junk entity ${a.name} (#${entityId})` : `Prune junk entity #${entityId}`;
    }
    case "entity_classify": {
      const entityId = a.entity_id ?? a.fact_id;
      return a.name
        ? `Classify entity ${a.name} (#${entityId}) → ${a.entity_type}`
        : `Classify entity #${entityId} → ${a.entity_type}`;
    }
    case "delete":
      return a.duplicate_of != null
        ? `Delete #${a.fact_id} (duplicate of #${a.duplicate_of})`
        : `Delete #${a.fact_id}`;
    case "retag":
      return `Retag #${a.fact_id}`;
    case "recategorize":
      return `Recategorize #${a.fact_id} → ${a.category}`;
    case "reflect": {
      const supersedes = a.supersedes ?? [];
      return supersedes.length
        ? `Reflect (replaces ${supersedes.map((s) => `#${s}`).join(", ")})`
        : "Reflect";
    }
    default:
      return a.op;
  }
}

export function splitTags(s?: string): string[] {
  return (s || "")
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
}

export function diffTags(oldStr?: string, newStr?: string) {
  const oldTags = splitTags(oldStr);
  const newTags = splitTags(newStr);
  const oldSet = new Set(oldTags);
  const newSet = new Set(newTags);
  return {
    oldTags,
    newTags,
    kept: oldTags.filter((t) => newSet.has(t)),
    removed: oldTags.filter((t) => !newSet.has(t)),
    added: newTags.filter((t) => !oldSet.has(t)),
  };
}

export function isBookkeepingTag(tag: string): boolean {
  return tag.startsWith("cat:") || tag.startsWith("target:");
}

export function formatHistoryTime(ts?: string | null): string {
  if (!ts) return "";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return String(ts);
  return date.toLocaleString([], {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatOplogTime(ts: number): string {
  if (!ts) return "";
  return formatHistoryTime(new Date(ts * 1000).toISOString());
}
