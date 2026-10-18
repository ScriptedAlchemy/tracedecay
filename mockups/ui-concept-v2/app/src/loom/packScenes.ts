import { PACK, shortId, type PackLoomEvent } from "../data/pack";
import type { EvidenceGrade, EventKind } from "./types";

export const GRADE_LABEL: Record<EvidenceGrade, string> = {
  exact: "EXACT", explicit: "EXPLICIT", inferred: "INFERRED",
  ambiguous: "AMBIGUOUS", stale: "STALE", unavailable: "UNAVAILABLE",
};

export const KIND_LABEL: Record<EventKind, string> = {
  message: "MESSAGE", reasoning: "REASONING", tool: "TOOL CALL", command: "COMMAND",
  "file-read": "FILE READ", "file-edit": "FILE EDIT", search: "SEARCH",
  session: "SESSION", spawn: "SPAWN", pr: "PR MARKER", branch: "BRANCH MARKER",
  end: "RECORD END", gap: "GAP",
  task: "TASK", worktree: "WORKTREE", test: "TEST", result: "RESULT", handoff: "HANDOFF", rejoin: "REJOIN", commit: "COMMIT", summary: "SUMMARY", decision: "DECISION",
};

export const SPINE_SESSION_ID = "d04908bf-d62f-4043-8a21-0f63d6569ebf";
export const BRANCH_PARENT_ID = "f6d565e5-7710-4921-9873-25f797f17554";
export const GHOST_PARENT_ID = "77dd8b64-a4d8-4114-9591-357a09f5a4cc";

function sessionEvents(sessionId: string): PackLoomEvent[] {
  return PACK.loomEvents
    .filter((e) => e.sessionId === sessionId && e.ts != null)
    .sort((a, b) => (a.ts! - b.ts!) || ((a.ordinal ?? 0) - (b.ordinal ?? 0)));
}

export function eventKind(e: PackLoomEvent): EventKind {
  if (Object.hasOwn(KIND_LABEL, e.kind)) return e.kind as EventKind;
  if (e.kind === "git_pull_request") return "pr";
  if (e.kind === "git_branch") return "branch";
  if (e.kind === "reasoning_visible") return "reasoning";
  if (e.kind === "tool_invocation") {
    if (e.tool === "Bash" || e.tool === "Shell") return "command";
    if (e.tool === "Read") return "file-read";
    if (e.tool === "Glob" || e.tool === "Grep" || e.tool === "ToolSearch") return "search";
    return "tool";
  }
  return "message";
}

export function eventGrade(e: PackLoomEvent): EvidenceGrade {
  // reasoning_visible rows are persisted visible statements, not raw fact rows.
  return e.kind === "reasoning_visible" ? "explicit" : "exact";
}

export const SELECTED_MARKER = sessionEvents(SPINE_SESSION_ID).filter(e => e.kind === "git_pull_request")[0];

export const GAP_LEDGER = [
  {
    id: "parent", issue: "Parent session not in snapshot", detail: "4 subagent sessions reference an absent parent row",
    affected: "08-17 06:47 → 06:53", span: "4 sessions", branch: shortId(GHOST_PARENT_ID, 12),
    exists: "parent_id fields (exact). 4 child rows.", grade: "unavailable" as EvidenceGrade,
  },
  {
    id: "bodies", issue: "Message bodies not ingested", detail: "Bodies, FTS, observation payloads not copied",
    affected: "all 71 sessions", span: "pack-wide", branch: "every lane",
    exists: "Spine events, kinds, roles, tool counts.", grade: "unavailable" as EvidenceGrade,
  },
  {
    id: "index", issue: "Code index not sealed", detail: "retrieval_anchors = 0 · facts table absent",
    affected: "index snapshot", span: "stale", branch: "tracedecay.db",
    exists: "graph_verified_heads_v1 counts exist.", grade: "stale" as EvidenceGrade,
  },
  {
    id: "pr", issue: "PR markers without provider record", detail: "6 markers; no PR number, body, or inbox",
    affected: "08-27 22:16 → 22:33", span: "6 markers", branch: shortId(SPINE_SESSION_ID, 8),
    exists: "Exact transcript markers with timestamps.", grade: "unavailable" as EvidenceGrade,
  },
];
