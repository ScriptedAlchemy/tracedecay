import { Archive, CheckCircle2, Eye, Power, RotateCcw } from "lucide-react";
import type { ReactNode } from "react";

import { Button } from "../sdk";
import { Spinner } from "../Spinner";
import {
  formatUnixTime,
  managedSkillStateClass,
  managedSkillSummary,
} from "./historyFormat";
import type {
  ManagedSkill,
  ManagedSkillState,
  SkillImprovementRecommendation,
  SkillStaleRecommendation,
  SkillUsageSummary,
} from "../types";

type ManagedSkillAction = "approve" | "discard-update" | "disable" | "archive" | "restore";

function SkillStateBadge({ state }: { state: ManagedSkillState }) {
  return (
    <span
      className={`shrink-0 rounded-sm border px-1.5 py-0.5 text-[10px] uppercase tracking-[0.08em] ${managedSkillStateClass(state)}`}
    >
      {state}
    </span>
  );
}

function SkillActionButton({
  action,
  label,
  icon,
  skillId,
  actioning,
  outlined = true,
  onAction,
}: {
  action: ManagedSkillAction;
  label: string;
  icon: ReactNode;
  skillId: string;
  actioning: string | null;
  outlined?: boolean;
  onAction: (action: string, skillId: string) => void;
}) {
  const loading = actioning?.endsWith(`:${action}`);

  return (
    <Button
      size="xs"
      outlined={outlined}
      disabled={Boolean(actioning)}
      onClick={() => onAction(action, skillId)}
      className="gap-1.5"
    >
      {loading ? <Spinner /> : icon}
      {label}
    </Button>
  );
}

export function ManagedSkillsSection({
  skills,
  selectedSkillId,
  selectedSkill,
  selectedUsage,
  selectedRecommendation,
  selectedImprovementRecommendation,
  loading,
  error,
  actioning,
  onRefresh,
  onLoadSkill,
  onAction,
}: {
  skills: ManagedSkill[];
  selectedSkillId: string | null;
  selectedSkill: ManagedSkill | null;
  selectedUsage: SkillUsageSummary | null;
  selectedRecommendation: SkillStaleRecommendation | null;
  selectedImprovementRecommendation: SkillImprovementRecommendation | null;
  loading: boolean;
  error: string;
  actioning: string | null;
  onRefresh: () => void;
  onLoadSkill: (skillId: string) => void;
  onAction: (action: string, skillId: string) => void;
}) {
  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
            Managed skills
          </div>
          <div className="mt-0.5 text-[11px] text-text-tertiary">
            Profile-owned skill drafts and approval state.
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
      {skills.length ? (
        <div className="mt-2 grid gap-2 xl:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
          <div className="flex min-w-0 flex-col gap-1.5">
            {skills.map((skill) => {
              const selected = selectedSkillId === skill.metadata.id;
              return (
                <button
                  key={skill.metadata.id}
                  type="button"
                  onClick={() => onLoadSkill(skill.metadata.id)}
                  className={`min-w-0 border px-2 py-1.5 text-left ${
                    selected
                      ? "border-primary/60 bg-primary/10"
                      : "border-border bg-background/40 hover:border-primary/40"
                  }`}
                >
                  <div className="flex min-w-0 items-start justify-between gap-2">
                    <div className="min-w-0">
                      <div className="truncate text-xs font-medium text-foreground">
                        {skill.metadata.title}
                      </div>
                      <div className="mt-0.5 truncate font-mono-ui text-[11px] text-text-tertiary">
                        {skill.metadata.id}
                      </div>
                    </div>
                    <SkillStateBadge state={skill.metadata.state} />
                  </div>
                  <div className="mt-1 line-clamp-2 text-[11px] text-text-secondary">
                    {skill.metadata.summary}
                  </div>
                </button>
              );
            })}
          </div>
          {selectedSkill ? (
            <div className="min-w-0 border border-border bg-background/40 px-3 py-2">
              <div className="flex min-w-0 items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="text-xs font-medium text-foreground">
                    {selectedSkill.metadata.title}
                  </div>
                  <div className="mt-0.5 font-mono-ui text-[11px] text-text-tertiary break-all">
                    {managedSkillSummary(selectedSkill)}
                  </div>
                </div>
                <SkillStateBadge state={selectedSkill.metadata.state} />
              </div>
              <div className="mt-2 grid gap-1 text-[11px] text-text-tertiary sm:grid-cols-2">
                <span className="font-mono-ui break-all">
                  checksum={selectedSkill.metadata.checksum}
                </span>
                <span>updated={formatUnixTime(selectedSkill.metadata.updated_at)}</span>
                <span>support files={selectedSkill.support_files.length}</span>
                <span>pinned={selectedSkill.metadata.pinned ? "yes" : "no"}</span>
              </div>
              {selectedUsage ? (
                <div className="mt-2 grid gap-1 border border-border/60 bg-secondary/20 p-2 text-[11px] text-text-tertiary sm:grid-cols-2">
                  <span className="font-mono-ui">
                    views={selectedUsage.view_count}
                  </span>
                  <span className="font-mono-ui">
                    uses={selectedUsage.use_count}
                  </span>
                  <span className="font-mono-ui">
                    patches={selectedUsage.patch_count}
                  </span>
                  <span>last={formatUnixTime(selectedUsage.last_activity_at)}</span>
                  <span className="sm:col-span-2">
                    targets={selectedUsage.targets.length ? selectedUsage.targets.join(", ") : "none"}
                  </span>
                </div>
              ) : null}
              {selectedRecommendation ? (
                <div
                  className={`mt-2 border px-2 py-1.5 text-[11px] ${
                    selectedRecommendation.stale || selectedRecommendation.recommendation !== "keep"
                      ? "border-warning/30 bg-warning/10 text-warning"
                      : "border-border bg-background/50 text-text-tertiary"
                  }`}
                >
                  <span className="font-mono-ui">
                    {selectedRecommendation.recommendation}
                  </span>
                  <span> · {selectedRecommendation.reason}</span>
                </div>
              ) : null}
              {selectedImprovementRecommendation?.improvement ? (
                <div className="mt-2 border border-warning/30 bg-warning/10 px-2 py-1.5 text-[11px] text-warning">
                  <span className="font-mono-ui">
                    {selectedImprovementRecommendation.recommendation}
                  </span>
                  <span> · {selectedImprovementRecommendation.reason}</span>
                  <span className="ml-1 font-mono-ui">
                    priority={selectedImprovementRecommendation.priority}
                  </span>
                </div>
              ) : null}
              {selectedSkill.pending_update ? (
                <div className="mt-2 border border-warning/30 bg-warning/10 p-2 text-[11px] text-warning">
                  <div className="font-mono-ui">
                    staged update · checksum={selectedSkill.pending_update.metadata.checksum}
                  </div>
                  <div className="mt-1 text-text-secondary">
                    {selectedSkill.pending_update.metadata.summary}
                  </div>
                  <div className="mt-1 text-text-tertiary">
                    staged={formatUnixTime(selectedSkill.pending_update.staged_at)}
                    {" · "}
                    support files={selectedSkill.pending_update.support_files.length}
                  </div>
                </div>
              ) : null}
              <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap border border-border bg-background/70 p-2 font-mono-ui text-[11px] leading-relaxed text-text-secondary">
                {selectedSkill.body_markdown || "No skill body."}
              </pre>
              <div className="mt-2 flex flex-wrap justify-end gap-2">
                <Button
                  size="xs"
                  outlined
                  disabled={Boolean(actioning)}
                  onClick={() => onLoadSkill(selectedSkill.metadata.id)}
                  className="gap-1.5"
                >
                  <Eye className="h-3.5 w-3.5" />
                  View
                </Button>
                <SkillActionButton
                  action="approve"
                  label="Approve"
                  icon={<CheckCircle2 className="h-3.5 w-3.5" />}
                  skillId={selectedSkill.metadata.id}
                  actioning={actioning}
                  outlined={false}
                  onAction={onAction}
                />
                {selectedSkill.pending_update ? (
                  <SkillActionButton
                    action="discard-update"
                    label="Discard"
                    icon={<RotateCcw className="h-3.5 w-3.5" />}
                    skillId={selectedSkill.metadata.id}
                    actioning={actioning}
                    onAction={onAction}
                  />
                ) : null}
                <SkillActionButton
                  action="disable"
                  label="Disable"
                  icon={<Power className="h-3.5 w-3.5" />}
                  skillId={selectedSkill.metadata.id}
                  actioning={actioning}
                  onAction={onAction}
                />
                <SkillActionButton
                  action="archive"
                  label="Archive"
                  icon={<Archive className="h-3.5 w-3.5" />}
                  skillId={selectedSkill.metadata.id}
                  actioning={actioning}
                  onAction={onAction}
                />
                <SkillActionButton
                  action="restore"
                  label="Restore"
                  icon={<RotateCcw className="h-3.5 w-3.5" />}
                  skillId={selectedSkill.metadata.id}
                  actioning={actioning}
                  onAction={onAction}
                />
              </div>
            </div>
          ) : null}
        </div>
      ) : (
        <div className="mt-2 text-xs text-text-tertiary">
          No managed skill drafts are waiting in this profile.
        </div>
      )}
    </div>
  );
}
