export type EvidenceGrade =
  | "exact"
  | "explicit"
  | "inferred"
  | "ambiguous"
  | "stale"
  | "unavailable";

/** Event vocabulary shared by exported records and the labeled design example. */
export type EventKind =
  | "message"
  | "reasoning"
  | "tool"
  | "command"
  | "file-read"
  | "file-edit"
  | "search"
  | "session"
  | "spawn"
  | "pr"
  | "branch"
  | "end"
  | "gap"
  | "task" | "worktree" | "test" | "result" | "handoff" | "rejoin" | "commit" | "summary" | "decision";

export type LoomStateId = "01" | "02" | "03" | "04" | "05" | "06" | "07";

export const LOOM_STATES: { id: LoomStateId; chip: string; title: string; kicker: string }[] = [
  { id: "01", chip: "Follow tail", title: "FOLLOW LOADED TAIL", kicker: "Orient to the loaded page. NOW is the loaded page end only." },
  { id: "02", chip: "Replay", title: "TEMPORAL REPLAY", kicker: "Scrub the loaded page. Unrevealed future stays hidden." },
  { id: "03", chip: "Branching", title: "BRANCHING EXECUTION", kicker: "Spawn, parallel work, evidenced handoff and rejoin." },
  { id: "04", chip: "Dense", title: "DENSE EXECUTION", kicker: "Workstream bundles first. Honest pack identities, spine-only coverage." },
  { id: "05", chip: "Evidence", title: "SELECTED EVENT EVIDENCE", kicker: "Exact hook, transcript, task, code, and causal neighborhood." },
  { id: "06", chip: "Feedback", title: "FEEDBACK CONTINUATION", kicker: "Local TraceDecay feedback. Later work that acts on it." },
  { id: "07", chip: "Gaps", title: "EVIDENCE GAPS", kicker: "Ambiguous, stale, missing, unavailable. No invented private CoT." },
];

export function parseLoomState(raw: string | null): LoomStateId {
  if (raw === "01" || raw === "02" || raw === "03" || raw === "04" || raw === "05" || raw === "06" || raw === "07") {
    return raw;
  }
  return "01";
}
