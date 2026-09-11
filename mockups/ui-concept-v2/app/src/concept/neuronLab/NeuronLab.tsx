import { useEffect, useRef, useState } from "react";
import { mountNet, mountTubes } from "./altScenes";
import { mountGraph } from "./graphScene";
import { mountVoxelo } from "./voxeloScene";
import type { LabGrain, LabHooks } from "./labUtil";

export type LabMode = "voxelo" | "tubes" | "graph" | "net";

const MODES: { id: LabMode; label: string }[] = [
  { id: "voxelo", label: "VOXELO" },
  { id: "tubes", label: "TUBES" },
  { id: "graph", label: "GRAPH" },
  { id: "net", label: "NET" },
];

function parseLab(): LabMode {
  const q = new URLSearchParams(window.location.search).get("lab");
  if (q === "tubes" || q === "graph" || q === "net") return q;
  return "voxelo";
}

const MOUNT = {
  voxelo: mountVoxelo,
  tubes: mountTubes,
  graph: mountGraph,
  net: mountNet,
};

export function NeuronLab(props: {
  onFocus?: (id: string) => void;
  scope?: string | null;
  grain?: LabGrain;
  onEnterScope?: (projectId: string) => void;
  onLeaveScope?: () => void;
  onGrain?: (grain: LabGrain) => void;
}) {
  const [lab, setLab] = useState<LabMode>(parseLab);
  const stageRef = useRef<HTMLDivElement>(null);
  const grain = props.grain ?? "session";
  const scoped = Boolean(props.scope);
  const hooksRef = useRef({
    onFocus: props.onFocus,
    onEnterScope: props.onEnterScope,
    onLeaveScope: props.onLeaveScope,
  });
  hooksRef.current = {
    onFocus: props.onFocus,
    onEnterScope: props.onEnterScope,
    onLeaveScope: props.onLeaveScope,
  };

  useEffect(() => {
    const u = new URL(window.location.href);
    u.searchParams.set("lab", lab);
    history.replaceState(null, "", u);
  }, [lab]);

  useEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    // grain session = one organism + session tufts on trunks; worktree = repo neighborhood.
    const hooks: LabHooks = {
      onFocus: (id) => hooksRef.current.onFocus?.(id),
      onEnterScope: (id) => hooksRef.current.onEnterScope?.(id),
      onLeaveScope: () => hooksRef.current.onLeaveScope?.(),
      scope: props.scope,
      grain,
    };
    const { dispose } = MOUNT[lab](el, hooks);
    return dispose;
  }, [lab, props.scope, grain]);

  return (
    <div className="neuron-lab">
      <div className="nl-chips" role="tablist" aria-label="Lab mode">
        {MODES.map((m) => (
          <button
            key={m.id}
            type="button"
            role="tab"
            aria-selected={lab === m.id}
            className={lab === m.id ? "nl-chip is-on" : "nl-chip"}
            onClick={() => setLab(m.id)}
          >
            {m.label}
          </button>
        ))}
        {scoped ? (
          <button type="button" className="nl-chip" onClick={() => props.onLeaveScope?.()}>
            FIELD
          </button>
        ) : null}
        {scoped && lab === "voxelo" ? (
          <>
            <button
              type="button"
              className={grain === "session" ? "nl-chip is-on" : "nl-chip"}
              onClick={() => props.onGrain?.("session")}
            >
              SESSIONS
            </button>
            <button
              type="button"
              className={grain === "worktree" ? "nl-chip is-on" : "nl-chip"}
              onClick={() => props.onGrain?.("worktree")}
            >
              WORKTREES
            </button>
          </>
        ) : null}
      </div>
      <div className="nl-stage" ref={stageRef} />
    </div>
  );
}
