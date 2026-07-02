import { Button } from "../sdk";
import { Spinner } from "../Spinner";
import { FactProposalsSection } from "./FactProposalsSection";
import { ManagedSkillsSection } from "./ManagedSkillsSection";
import type {
  FactProposalRecord,
  ManagedSkill,
  SkillImprovementRecommendation,
  SkillStaleRecommendation,
  SkillUsageSummary,
} from "../types";

interface CurationProposalsPanelProps {
  factProposals: FactProposalRecord[];
  factProposalsLoading: boolean;
  factProposalsError: string;
  factProposalActioning: string | null;
  managedSkills: ManagedSkill[];
  selectedManagedSkillId: string | null;
  selectedManagedSkill: ManagedSkill | null;
  selectedUsage: SkillUsageSummary | null;
  selectedRecommendation: SkillStaleRecommendation | null;
  selectedImprovementRecommendation: SkillImprovementRecommendation | null;
  managedSkillsLoading: boolean;
  managedSkillsError: string;
  managedSkillActioning: string | null;
  loadFactProposals: (force?: boolean) => void;
  loadManagedSkills: (force?: boolean) => void;
  loadManagedSkill: (skillId: string) => void;
  runFactProposalAction: (action: "apply" | "reject", proposalId: string) => void;
  runManagedSkillAction: (action: string, skillId: string) => void;
}

export function CurationProposalsPanel({
  factProposals,
  factProposalsLoading,
  factProposalsError,
  factProposalActioning,
  managedSkills,
  selectedManagedSkillId,
  selectedManagedSkill,
  selectedUsage,
  selectedRecommendation,
  selectedImprovementRecommendation,
  managedSkillsLoading,
  managedSkillsError,
  managedSkillActioning,
  loadFactProposals,
  loadManagedSkills,
  loadManagedSkill,
  runFactProposalAction,
  runManagedSkillAction,
}: CurationProposalsPanelProps) {
  const refreshing = factProposalsLoading || managedSkillsLoading;
  return (
    <div
      role="tabpanel"
      id="curation-panel-proposals"
      aria-labelledby="curation-tab-proposals"
      className="flex flex-1 min-h-0 flex-col gap-3 overflow-y-auto overflow-x-hidden pr-1"
    >
      <div className="flex min-w-0 items-center justify-between gap-2 shrink-0">
        <div className="min-w-0">
          <div className="text-xs font-medium text-foreground">Proposals</div>
          <div className="text-[11px] text-text-tertiary">
            Facts and skills the automation loops staged for your approval.
          </div>
        </div>
        <Button
          size="xs"
          ghost
          disabled={refreshing}
          onClick={() => {
            loadFactProposals(true);
            loadManagedSkills(true);
          }}
          className="shrink-0 gap-2"
        >
          {refreshing ? <Spinner /> : null}
          Refresh
        </Button>
      </div>
      <FactProposalsSection
        proposals={factProposals}
        loading={factProposalsLoading}
        error={factProposalsError}
        actioning={factProposalActioning}
        onRefresh={() => loadFactProposals(true)}
        onAction={runFactProposalAction}
      />
      <ManagedSkillsSection
        skills={managedSkills}
        selectedSkillId={selectedManagedSkillId}
        selectedSkill={selectedManagedSkill}
        selectedUsage={selectedUsage}
        selectedRecommendation={selectedRecommendation}
        selectedImprovementRecommendation={selectedImprovementRecommendation}
        loading={managedSkillsLoading}
        error={managedSkillsError}
        actioning={managedSkillActioning}
        onRefresh={() => loadManagedSkills(true)}
        onLoadSkill={loadManagedSkill}
        onAction={runManagedSkillAction}
      />
    </div>
  );
}
