import { CurationTabPanel } from "./CurationTabPanel";
import { FactProposalsSection } from "./FactProposalsSection";
import { ManagedSkillsSection } from "./ManagedSkillsSection";
import type {
  FactProposalRecord,
  ManagedSkill,
  SkillImprovementRecommendation,
  SkillStaleRecommendation,
  SkillUsageSummary,
} from "../types";
import type { ManagedSkillExportsResult } from "./useManagedSkills";

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
  managedSkillExports: ManagedSkillExportsResult | null;
  loadFactProposals: (force?: boolean) => void;
  loadManagedSkills: (force?: boolean) => void;
  loadManagedSkill: (skillId: string) => void;
  runFactProposalAction: (
    action: "apply" | "reject",
    proposalId: string,
  ) => void;
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
  managedSkillExports,
  loadFactProposals,
  loadManagedSkills,
  loadManagedSkill,
  runFactProposalAction,
  runManagedSkillAction,
}: CurationProposalsPanelProps) {
  return (
    <CurationTabPanel
      tab="proposals"
      title="Proposals"
      subtitle="Memory fact outcomes and managed skill drafts staged by automation loops."
    >
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
        exportsResult={managedSkillExports}
        onRefresh={() => loadManagedSkills(true)}
        onLoadSkill={loadManagedSkill}
        onAction={runManagedSkillAction}
      />
    </CurationTabPanel>
  );
}
