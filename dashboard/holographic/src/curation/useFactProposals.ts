import { useCallback, useState } from "react";

import type { FactProposalRecord } from "../types";
import { errorMessage } from "./errors";
import type { CurationApi } from "./useCurationData";

type FactProposalAction = "apply" | "reject";

export function useFactProposals({
  api,
  onApplied,
}: {
  api: CurationApi;
  onApplied?: () => void;
}) {
  const [factProposals, setFactProposals] = useState<FactProposalRecord[]>([]);
  const [factProposalsLoading, setFactProposalsLoading] = useState(false);
  const [factProposalsError, setFactProposalsError] = useState("");
  const [factProposalActioning, setFactProposalActioning] = useState<string | null>(null);

  const loadFactProposals = useCallback((showSpinner = false) => {
    if (showSpinner) setFactProposalsLoading(true);
    setFactProposalsError("");
    return api
      .getFactProposals({ limit: 50 })
      .then((response) => {
        setFactProposals(response.proposals || []);
        if (response.error) setFactProposalsError(response.error);
        return response;
      })
      .catch((err) => {
        setFactProposalsError(errorMessage(err));
        throw err;
      })
      .finally(() => {
        if (showSpinner) setFactProposalsLoading(false);
      });
  }, [api]);

  const runFactProposalAction = useCallback(async (
    action: FactProposalAction,
    id: string,
  ) => {
    setFactProposalActioning(`${id}:${action}`);
    setFactProposalsError("");
    try {
      const response = action === "apply"
        ? await api.applyFactProposal(id)
        : await api.rejectFactProposal(id, "rejected from dashboard");
      setFactProposals((current) =>
        current.map((proposal) =>
          proposal.proposal_id === response.proposal.proposal_id
            ? response.proposal
            : proposal
        )
      );
      await loadFactProposals(false);
      if (action === "apply") {
        onApplied?.();
      }
      return response;
    } catch (err) {
      setFactProposalsError(errorMessage(err));
      throw err;
    } finally {
      setFactProposalActioning(null);
    }
  }, [api, loadFactProposals, onApplied]);

  return {
    factProposals,
    factProposalsLoading,
    factProposalsError,
    factProposalActioning,
    loadFactProposals,
    runFactProposalAction,
  };
}
