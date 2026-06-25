import { useCallback, useState } from "react";

import type {
  ManagedSkill,
  ManagedSkillResponse,
  SkillImprovementRecommendation,
  SkillStaleRecommendation,
  SkillUsageSummary,
} from "../types";
import { errorMessage } from "./errors";
import type { CurationApi } from "./useCurationData";

type ManagedSkillAction = "approve" | "discard-update" | "disable" | "archive" | "restore";

function indexBySkillId<T extends { skill_id: string }>(items: T[] = []): Record<string, T> {
  return Object.fromEntries(items.map((item) => [item.skill_id, item]));
}

export function useManagedSkills(api: CurationApi) {
  const [managedSkills, setManagedSkills] = useState<ManagedSkill[]>([]);
  const [selectedManagedSkillId, setSelectedManagedSkillId] = useState<string | null>(null);
  const [selectedManagedSkill, setSelectedManagedSkill] = useState<ManagedSkill | null>(null);
  const [managedSkillUsage, setManagedSkillUsage] = useState<Record<string, SkillUsageSummary>>({});
  const [managedSkillRecommendations, setManagedSkillRecommendations] = useState<
    Record<string, SkillStaleRecommendation>
  >({});
  const [
    managedSkillImprovementRecommendations,
    setManagedSkillImprovementRecommendations,
  ] = useState<Record<string, SkillImprovementRecommendation>>({});
  const [managedSkillsLoading, setManagedSkillsLoading] = useState(false);
  const [managedSkillsError, setManagedSkillsError] = useState("");
  const [managedSkillActioning, setManagedSkillActioning] = useState<string | null>(null);

  const applyManagedSkillResponse = useCallback((response: ManagedSkillResponse) => {
    const skillId = response.skill.metadata.id;
    setSelectedManagedSkillId(skillId);
    setSelectedManagedSkill(response.skill);
    if (response.usage_summary) {
      setManagedSkillUsage((current) => ({
        ...current,
        [skillId]: response.usage_summary,
      }));
    }
    if (response.stale_recommendation) {
      setManagedSkillRecommendations((current) => ({
        ...current,
        [skillId]: response.stale_recommendation,
      }));
    }
    if (response.improvement_recommendation) {
      setManagedSkillImprovementRecommendations((current) => ({
        ...current,
        [skillId]: response.improvement_recommendation,
      }));
    }
  }, []);

  const loadManagedSkill = useCallback((id: string) => {
    setManagedSkillsError("");
    return api
      .getManagedSkill(id)
      .then((response) => {
        applyManagedSkillResponse(response);
        return response.skill;
      })
      .catch((err) => {
        setManagedSkillsError(errorMessage(err));
        throw err;
      });
  }, [api, applyManagedSkillResponse]);

  const loadManagedSkills = useCallback((showSpinner = false) => {
    if (showSpinner) setManagedSkillsLoading(true);
    setManagedSkillsError("");
    return api
      .getManagedSkills()
      .then(async (response) => {
        const skills = response.skills || [];
        setManagedSkills(skills);
        setManagedSkillUsage(indexBySkillId(response.usage_summaries));
        setManagedSkillRecommendations(indexBySkillId(response.stale_recommendations));
        setManagedSkillImprovementRecommendations(
          indexBySkillId(response.improvement_recommendations),
        );
        const nextId = selectedManagedSkillId && skills.some((skill) =>
          skill.metadata.id === selectedManagedSkillId
        )
          ? selectedManagedSkillId
          : (skills[0]?.metadata.id ?? null);
        setSelectedManagedSkillId(nextId);
        if (nextId) {
          await loadManagedSkill(nextId);
        } else {
          setSelectedManagedSkill(null);
        }
        if (response.error) setManagedSkillsError(response.error);
        return response;
      })
      .catch((err) => {
        setManagedSkillsError(errorMessage(err));
        throw err;
      })
      .finally(() => {
        if (showSpinner) setManagedSkillsLoading(false);
      });
  }, [api, loadManagedSkill, selectedManagedSkillId]);

  const runManagedSkillAction = useCallback(async (
    action: ManagedSkillAction,
    id = selectedManagedSkillId,
  ) => {
    if (!id) return null;
    setManagedSkillActioning(`${id}:${action}`);
    setManagedSkillsError("");
    try {
      const call = {
        approve: api.approveManagedSkill,
        "discard-update": api.discardManagedSkillUpdate,
        disable: api.disableManagedSkill,
        archive: api.archiveManagedSkill,
        restore: api.restoreManagedSkill,
      }[action];
      const response = await call(id);
      applyManagedSkillResponse(response);
      await loadManagedSkills(false);
      return response;
    } catch (err) {
      setManagedSkillsError(errorMessage(err));
      throw err;
    } finally {
      setManagedSkillActioning(null);
    }
  }, [api, applyManagedSkillResponse, loadManagedSkills, selectedManagedSkillId]);

  return {
    managedSkills,
    selectedManagedSkillId,
    selectedManagedSkill,
    managedSkillUsage,
    managedSkillRecommendations,
    managedSkillImprovementRecommendations,
    managedSkillsLoading,
    managedSkillsError,
    managedSkillActioning,
    loadManagedSkills,
    loadManagedSkill,
    runManagedSkillAction,
  };
}
