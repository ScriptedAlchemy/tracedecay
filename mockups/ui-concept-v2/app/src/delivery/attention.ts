import type { AttentionItem } from "../app/workspace";
import workload from "../data/tracked-workload";

export const DELIVERY_ATTENTION: AttentionItem[] = [
  ...(workload.trackingCoverage.state === "unavailable" ? [{
    id: "delivery:tracking-coverage", title: "Delivery tracking evidence unavailable",
    detail: workload.trackingCoverage.reason,
    source: "coverage", severity: "information", status: "active", owner: "system",
    evidence: "unavailable", mode: "snapshot", observedAt: workload.trackingCoverage.observedAt,
    sourceRef: "TraceDecay tracking and indexed-head export",
    target: { surface: "delivery", params: { state: "01" } },
  } satisfies AttentionItem] : []),
  ...workload.prs.filter((pr) => pr.state === "open" && ((pr.updatedAt && Date.parse(workload.capturedAt) - Date.parse(pr.updatedAt) >= 30 * 86400000) || workload.reviewActivity.some((activity) => activity.prId === pr.id && activity.candidateFlags.length))).map((pr): AttentionItem => {
    const activity = workload.reviewActivity.find((a) => a.prId === pr.id);
    return {
      id: `delivery:${pr.id}`,
      title: `${pr.id} · follow-up candidate`,
      detail: `${activity?.candidateFlags.map((flag) => flag.basis).join(" ") || "Provider metadata unchanged for at least 30 days at the export anchor; not a substantive-review clock."} Owner assignment and merge readiness are not established.`,
      source: activity ? "review" : "coverage", severity: "information", status: "active", owner: "unknown",
      evidence: "inferred", mode: "snapshot", observedAt: null,
      sourceRef: pr.url, repository: pr.repo,
      target: { surface: "delivery", params: { state: "08", pr: pr.id } },
    };
  }),
  { id: "delivery:fixture:ci-failure", title: "Fixture · bundle-size check failed", detail: "Authored test failure at12:47:05. No real check or rerun integration.", source: "ci", severity: "error", status: "active", owner: "you", evidence: "explicit", mode: "fixture", observedAt: "2025-05-09T12:47:05Z", sourceRef: "lookbook/pngs/08-delivery/final/05-temporal-replay.png", target: { surface: "delivery", params: { state: "05" } } },
  { id: "delivery:fixture:743-review", title: "PR743 packet · unresolved retained findings", detail: "Retained review packet: comment target, digest comparison and pipefail findings. No current resolution evidence queried.", source: "review", severity: "warning", status: "active", owner: "you", evidence: "explicit", mode: "fixture", observedAt: null, sourceRef: "https://github.com/ScriptedAlchemy/tracedecay/pull/743", repository: "ScriptedAlchemy/tracedecay", target: { surface: "delivery", params: { state: "10" } } },
];
