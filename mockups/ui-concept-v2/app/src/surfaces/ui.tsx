import type { ReactNode } from "react";
import { Corners } from "../app/shell/Corners";

export type LaneState =
  | "ready"
  | "partial"
  | "loading"
  | "indexing"
  | "unavailable"
  | "empty"
  | "denied"
  | "unauthorized"
  | "not_published"
  | "restricted"
  | "concept";

const STATE_LABEL: Record<LaneState, string> = {
  ready: "ready",
  partial: "partial",
  loading: "loading",
  indexing: "indexing",
  unavailable: "unavailable",
  empty: "served empty",
  denied: "denied",
  unauthorized: "unauthorized",
  not_published: "not_published",
  restricted: "restricted",
  concept: "CONCEPT",
};

export function Badge(props: { state: LaneState; children?: ReactNode }) {
  return (
    <span className={`badge st-${props.state}`}>
      {props.children ?? STATE_LABEL[props.state]}
    </span>
  );
}

export function Panel(props: {
  className?: string;
  corners?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={`panel ${props.className ?? ""}`.trim()}>
      {props.corners === false ? null : <Corners />}
      {props.children}
    </div>
  );
}
