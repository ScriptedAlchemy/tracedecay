import { Archive, CheckCircle2 } from "lucide-react";
import type { ReactNode } from "react";

import { Button } from "../sdk";
import { Spinner } from "../Spinner";
import {
  factProposalDetail,
  factProposalSummary,
  formatUnixTime,
} from "./historyFormat";
import { StateBadge } from "./ManagedSkillsSection";
import type { FactProposalRecord } from "../types";

type FactProposalAction = "apply" | "reject";

function ProposalActionButton({
  action,
  label,
  icon,
  proposalId,
  actioning,
  outlined = false,
  onAction,
}: {
  action: FactProposalAction;
  label: string;
  icon: ReactNode;
  proposalId: string;
  actioning: string | null;
  outlined?: boolean;
  onAction: (action: FactProposalAction, proposalId: string) => void;
}) {
  const loading = actioning?.endsWith(`:${action}`);

  return (
    <Button
      size="xs"
      outlined={outlined}
      disabled={Boolean(actioning)}
      onClick={() => onAction(action, proposalId)}
      className="gap-1.5"
    >
      {loading ? <Spinner /> : icon}
      {label}
    </Button>
  );
}

function proposalUpdatedAt(proposal: FactProposalRecord): number {
  return Number(proposal.updated_at) || 0;
}

export function FactProposalsSection({
  proposals,
  loading,
  error,
  actioning,
  onRefresh,
  onAction,
}: {
  proposals: FactProposalRecord[];
  loading: boolean;
  error: string;
  actioning: string | null;
  onRefresh: () => void;
  onAction: (action: FactProposalAction, proposalId: string) => void;
}) {
  const isPending = (proposal: FactProposalRecord) =>
    proposal.state === "pending_approval";
  const sorted = [...proposals].sort(
    (a, b) => proposalUpdatedAt(b) - proposalUpdatedAt(a),
  );
  const pendingProposals = sorted.filter(isPending);
  const resolvedProposals = sorted.filter((proposal) => !isPending(proposal));
  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
            <span>Fact proposals</span>
            {pendingProposals.length ? (
              <span>{` · ${pendingProposals.length} pending`}</span>
            ) : null}
          </div>
          <div className="mt-0.5 text-[11px] text-text-tertiary">
            Session-reflection facts staged for dashboard approval.
          </div>
        </div>
        <Button
          size="xs"
          ghost
          disabled={loading}
          onClick={onRefresh}
          className="shrink-0 gap-2"
        >
          {loading ? <Spinner /> : null}
          Refresh
        </Button>
      </div>
      {error ? (
        <div className="mt-2 border border-destructive/30 bg-destructive/10 px-2 py-1 text-xs text-destructive">
          {error}
        </div>
      ) : null}
      <div className="mt-2 grid gap-1.5">
        {pendingProposals.map((proposal) => (
          <div
            key={proposal.proposal_id}
            className="min-w-0 border border-border bg-background/40 px-2 py-1.5"
          >
            <div className="flex min-w-0 items-start justify-between gap-2">
              <div className="min-w-0">
                <div className="line-clamp-2 text-xs font-medium text-foreground">
                  {factProposalSummary(proposal)}
                </div>
                <div className="mt-0.5 font-mono-ui text-[11px] text-text-tertiary break-all">
                  {factProposalDetail(proposal)}
                </div>
              </div>
              <StateBadge state={proposal.state} />
            </div>
            <div className="mt-1 flex flex-wrap items-center justify-between gap-2 text-[11px] text-text-tertiary">
              <span>updated={formatUnixTime(proposal.updated_at)}</span>
              <div className="flex flex-wrap justify-end gap-2">
                <ProposalActionButton
                  action="apply"
                  label="Apply fact"
                  icon={<CheckCircle2 className="h-3.5 w-3.5" />}
                  proposalId={proposal.proposal_id}
                  actioning={actioning}
                  onAction={onAction}
                />
                <ProposalActionButton
                  action="reject"
                  label="Reject"
                  icon={<Archive className="h-3.5 w-3.5" />}
                  proposalId={proposal.proposal_id}
                  actioning={actioning}
                  outlined
                  onAction={onAction}
                />
              </div>
            </div>
          </div>
        ))}
        {pendingProposals.length === 0 ? (
          <div className="text-xs text-text-tertiary">
            {proposals.length === 0
              ? "No fact proposals are waiting in this profile."
              : "No proposals are waiting for review."}
          </div>
        ) : null}
        {resolvedProposals.length ? (
          <details className="min-w-0">
            <summary className="cursor-pointer text-[11px] text-text-tertiary">
              {resolvedProposals.length} resolved proposal
              {resolvedProposals.length === 1 ? "" : "s"}
            </summary>
            <div className="mt-1.5 grid gap-1.5">
              {resolvedProposals.map((proposal) => (
                <div
                  key={proposal.proposal_id}
                  className="min-w-0 border border-border/60 bg-background/20 px-2 py-1.5"
                >
                  <div className="flex min-w-0 items-start justify-between gap-2">
                    <div className="min-w-0">
                      <div className="line-clamp-1 text-xs text-text-secondary">
                        {factProposalSummary(proposal)}
                      </div>
                      <div className="mt-0.5 text-[11px] text-text-tertiary">
                        updated={formatUnixTime(proposal.updated_at)}
                      </div>
                    </div>
                    <StateBadge state={proposal.state} />
                  </div>
                </div>
              ))}
            </div>
          </details>
        ) : null}
      </div>
    </div>
  );
}
