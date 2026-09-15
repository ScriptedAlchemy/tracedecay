import sessions from "../../profile-pack/proj_ae394425f7837d4f/sessions.json";
import messages from "../../profile-pack/proj_ae394425f7837d4f/messages-spine.json";
import observations from "../../profile-pack/proj_ae394425f7837d4f/observations-slim.json";
import type { NeuralArbor } from "../brain/particles";

type SessionRow = {
  session_id: string;
  project_path: string;
  is_subagent: number;
};
type MessageRow = { message_id: string; session_id: string };
type ObsRow = { session_id: string; kind: string };

export const TRACEDECAY_WORKTREES = [
  "redesign",
  "ui-concept-first-party",
  "ui-concept-v2-final-followup",
];

function worktreeOf(path: string) {
  if (path === "/Volumes/bigssd/projects/tracedecay/.worktrees/ui-concept-v2-final-followup") return "ui-concept-v2-final-followup";
  if (path === "/Volumes/bigssd/projects/tracedecay/.worktrees/ui-concept-first-party") return "ui-concept-first-party";
  if (path === "/Volumes/bigssd/projects/tracedecay") return "redesign";
  return "unattributed";
}

export const TRACEDECAY_PROJECT_ID = "proj_ae394425f7837d4f";

export function tracedecayArbor(): NeuralArbor {
  const sessionRows = sessions as SessionRow[];
  const sessionIds = sessionRows.map((s) => s.session_id);
  const sessionWorktrees = sessionRows.map((s) => worktreeOf(s.project_path));
  const messagePlacements = (messages as MessageRow[]).map((m) => ({
    sessionId: m.session_id,
    id: m.message_id,
  }));
  const observationPlacements = (observations as ObsRow[]).map((o, i) => ({
    sessionId: o.session_id,
    kind: o.kind,
    i,
  }));
  return {
    worktrees: [...TRACEDECAY_WORKTREES, ...(sessionWorktrees.includes("unattributed") ? ["unattributed"] : [])],
    sessions: sessionIds.length,
    sessionIds,
    sessionWorktrees,
    messages: messagePlacements.length,
    messagePlacements,
    observations: observationPlacements.length,
    observationPlacements,
    hook: true,
  };
}
