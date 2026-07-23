import { StateChip } from '../../ui/StateChip';

const COPY: Record<string, string> = {
  brain: 'Whole-system and scoped summaries, health, activity, and freshness.',
  explorer: 'Pivotable search across messages, sessions, facts, code, and time.',
  loom: 'Temporal and causal traces linking prompts, tools, code, and outcomes.',
  sessions: 'Transcript search, LCM summaries, and raw-message drill-down.',
  agents: 'Agent trees, status, handoffs, tool activity, and failure context.',
  code: 'Symbol search, references, diagnostics, and graph freshness.',
  knowledge: 'Facts, evidence, contradictions, supersession, and curation.',
  delivery: 'Changes, commits, branches, worktrees, PRs, CI, and releases.',
  automations: 'Schedules, run history, artifacts, approvals, and skills.',
  observatory: 'Hook hints, event flow, latency, daemon and storage health.',
  costs: 'Provider and model usage, tokens, latency, and estimated cost.',
  settings: 'Effective layered configuration and validated changes.',
};

/** Designed pending state for a workspace whose slice has not shipped yet.
 * Truthful (loading-nothing is not pretended), scoped, and quiet. */
export function WorkspacePlaceholder({ workspace }: { workspace: string }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-8">
      <h1 className="text-lg font-semibold capitalize tracking-tight">{workspace}</h1>
      <p className="max-w-md text-center text-sm text-text-muted">
        {COPY[workspace] ?? 'Workspace'}
      </p>
      <StateChip kind="unknown" detail="workspace slice not yet implemented" />
    </div>
  );
}
