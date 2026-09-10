import { ATTENTION, HONEST_INBOX, type AttentionSource, type HonestInbox, type StatusTone } from "./data";

const HONEST_TONE: Record<HonestInbox, StatusTone> = {
  unauthorized: "danger",
  denied: "danger",
  "rate-limited": "amber",
  stale: "amber",
  not_published: "violet",
  unavailable: "quiet",
  "served-empty": "ready",
};

export function AttentionChip(props: { id: AttentionSource; on?: boolean }) {
  const a = ATTENTION.find((x) => x.id === props.id)!;
  return (
    <span className={`dl-att tone-${a.tone}${props.on ? " is-on" : ""}`}>
      {a.label} {a.count}
    </span>
  );
}

export function AttentionRow() {
  return (
    <div className="dl-chiprow" aria-label="Review attention sources">
      {ATTENTION.map((a) => (
        <span key={a.id} className={`dl-att tone-${a.tone}`}>
          {a.label} · {a.count}
        </span>
      ))}
    </div>
  );
}

export function HonestMark(props: { id: HonestInbox }) {
  return <span className={`dl-honest tone-${HONEST_TONE[props.id]}`}>{props.id}</span>;
}

export function HonestLegend() {
  return (
    <div className="dl-filters" aria-label="Honest inbox states">
      {HONEST_INBOX.map((h) => (
        <div key={h.id} className="dl-stat">
          <span>
            <HonestMark id={h.id} />
          </span>
          <b>{h.note}</b>
        </div>
      ))}
    </div>
  );
}

export function GradeMark(props: { grade: string }) {
  const g = props.grade;
  const tone: StatusTone =
    g === "EXACT" || g === "EXPLICIT"
      ? "ready"
      : g === "INFERRED"
        ? "live"
        : g === "AMBIGUOUS" || g === "STALE"
          ? "amber"
          : "violet";
  return <span className={`dl-att tone-${tone}`}>{g}</span>;
}
