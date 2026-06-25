import { Archive, CheckCircle2 } from "lucide-react";
import type { ReactNode } from "react";

import { Button } from "../sdk";
import { Spinner } from "../Spinner";
import {
  factProposalDetail,
  factProposalSummary,
  formatUnixTime,
  managedSkillStateClass,
} from "./historyFormat";
import type { FactProposalRecord } from "../types";

type FactProposalAction = "apply" | "reject";

function ProposalActionButton({
  action,
  label,
  icon,
  proposalId,
  pending,
  actioning,
  outlined = false,
  onAction,
}: {
  action: FactProposalAction;
  label: string;
  icon: ReactNode;
  proposalId: string;
  pending: boolean;
  actioning: string | null;
  outlined?: boolean;
  onAction: (action: FactProposalAction, proposalId: string) => void;
}) {
  const loading = actioning?.endsWith(`:${action}`);

  return (
    <Button
      size="xs"
      outlined={outlined}
      disabled={!pending || Boolean(actioning)}
      onClick={() => onAction(action, proposalId)}
      className="gap-1.5"
    >
      {loading ? <Spinner /> : icon}
      {label}
    </Button>
  );
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
  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
            Fact proposals
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
      {proposals.length ? (
        <div className="mt-2 grid gap-1.5">
          {proposals.map((proposal) => {
            const pending = proposal.state === "pending_approval";
            return (
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
                  <span
                    className={`shrink-0 rounded-sm border px-1.5 py-0.5 text-[10px] uppercase tracking-[0.08em] ${managedSkillStateClass(proposal.state)}`}
                  >
                    {proposal.state}
                  </span>
                </div>
                <div className="mt-1 flex flex-wrap items-center justify-between gap-2 text-[11px] text-text-tertiary">
                  <span>updated={formatUnixTime(proposal.updated_at)}</span>
                  <div className="flex flex-wrap justify-end gap-2">
                    <ProposalActionButton
                      action="apply"
                      label="Apply fact"
                      icon={<CheckCircle2 className="h-3.5 w-3.5" />}
                      proposalId={proposal.proposal_id}
                      pending={pending}
                      actioning={actioning}
                      onAction={onAction}
                    />
                    <ProposalActionButton
                      action="reject"
                      label="Reject"
                      icon={<Archive className="h-3.5 w-3.5" />}
                      proposalId={proposal.proposal_id}
                      pending={pending}
                      actioning={actioning}
                      outlined
                      onAction={onAction}
                    />
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      ) : (
        <div className="mt-2 text-xs text-text-tertiary">
          No fact proposals are waiting in this profile.
        </div>
      )}
    </div>
  );
}
