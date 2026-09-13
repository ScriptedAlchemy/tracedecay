/** Cross-field validation for the public automatic-curation terminal. */

const MAX_TERMINAL_COUNT = 1_000_000;
const MEMORY_CURATOR_SKIP_REASONS = new Set([
  "automation_disabled",
  "memory_curator_disabled",
  "delegated_host_mode",
  "backend_disabled",
  "scheduler_lock_active",
  "task_not_schedulable",
  "scheduler_schedule_invalid",
  "scheduler_schedule_manual",
  "scheduler_idle_window_active",
  "scheduler_non_retryable_failure",
  "scheduler_cooldown_active",
  "scheduler_interval_not_elapsed",
  "scheduler_cron_not_due",
  "similarity_authority_unavailable",
  "partial_coverage_no_candidates",
  "nothing_to_review",
]);

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function boundedCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0 &&
    (value as number) <= MAX_TERMINAL_COUNT;
}

function safeUnsigned(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
      .map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

async function canonicalSha256(value: unknown): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(canonicalJson(value)),
  );
  return `sha256:${Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0")).join("")}`;
}

function canonicalIdentifier(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 &&
    value.trim() === value && new TextEncoder().encode(value).length <= 512 &&
    !/\p{Cc}/u.test(value);
}

function rawSha256(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}

function taggedSha256(value: unknown): value is string {
  return typeof value === "string" && /^sha256:[0-9a-f]{64}$/.test(value);
}

function ownerMatches(value: unknown): value is Record<string, unknown> {
  const owner = record(value);
  return owner !== undefined && (owner.kind === "profile" ||
    (owner.kind === "project" && canonicalIdentifier(owner.project_id)));
}

function sanitizationReceiptMatches(value: unknown): boolean {
  const sanitization = record(value);
  const receipt = record(sanitization?.receipt);
  const payload = record(sanitization?.payload);
  return sanitization !== undefined && receipt !== undefined && payload !== undefined &&
    canonicalIdentifier(receipt.receipt_id) &&
    canonicalIdentifier(receipt.sanitizer_version) && taggedSha256(payload.digest) &&
    safeUnsigned(payload.byte_len) && payload.byte_len > 0 &&
    (sanitization.disposition === "accepted" || sanitization.disposition === "redacted") &&
    (sanitization.sensitivity === "non_sensitive" ||
      sanitization.sensitivity === "sensitive" || sanitization.sensitivity === "secret") &&
    !(sanitization.disposition === "accepted" && sanitization.sensitivity === "secret");
}

function factIdMatchesOwner(value: unknown, ownerBinding: string): value is string {
  return typeof value === "string" &&
    new RegExp(`^fact\\.v1\\.${ownerBinding}\\.[0-9a-f]{64}$`).test(value);
}

interface CurationTracker {
  owner: Record<string, unknown>;
  disposition?: unknown;
  committedEventIds: Set<string>;
  durableOperationIdentities: Set<string>;
  changedFactIds: string[];
  replayFactId?: string;
  replayEventId?: string;
  factsAdded: number;
  factsUpdated: number;
  factsMerged: number;
  factsRemoved: number;
  normalizedTags: number;
  factsLinked: number;
}

function appendChanged(tracker: CurationTracker, factId: string): void {
  if (!tracker.changedFactIds.includes(factId)) tracker.changedFactIds.push(factId);
}

function acceptCommit(
  tracker: CurationTracker,
  value: unknown,
  factId: string,
  eventCount: number | undefined,
  activeAssertion: "any" | "present" | "absent",
): boolean {
  const commit = record(value);
  const eventIds = commit?.committed_event_ids;
  const lastEventId = commit?.last_event_id;
  if (
    commit === undefined || !Array.isArray(eventIds) || eventIds.length === 0 ||
    canonicalJson(commit.owner) !== canonicalJson(tracker.owner) ||
    commit.fact_id !== factId ||
    (eventCount !== undefined && eventIds.length !== eventCount) ||
    !canonicalIdentifier(lastEventId) || eventIds.at(-1) !== lastEventId ||
    (commit.active_assertion_id != null &&
      !canonicalIdentifier(commit.active_assertion_id)) ||
    (activeAssertion === "present" && commit.active_assertion_id == null) ||
    (activeAssertion === "absent" && commit.active_assertion_id != null) ||
    (tracker.disposition !== undefined && tracker.disposition !== commit.disposition)
  ) return false;
  for (const eventId of eventIds) {
    if (!canonicalIdentifier(eventId) || tracker.committedEventIds.has(eventId)) return false;
    tracker.committedEventIds.add(eventId);
  }
  tracker.disposition = commit.disposition;
  if (tracker.replayFactId === undefined) {
    tracker.replayFactId = factId;
    tracker.replayEventId = lastEventId;
  }
  return true;
}

function comparisonMatches(effect: Record<string, unknown>): boolean {
  return typeof effect.closest_fact_id === "string" &&
    effect.closest_fact_id !== effect.fact_id &&
    Number.isSafeInteger(effect.similarity_millionths) &&
    (effect.similarity_millionths as number) >= 0 &&
    (effect.similarity_millionths as number) <= 1_000_000;
}

function relationMatches(
  relationValue: unknown,
  sourceFactId: string,
  targetFactId: string,
  ownerBinding: string,
): boolean {
  const relation = record(relationValue);
  const provenance = record(relation?.provenance);
  const evidence = relation?.evidence_fact_ids;
  const label = provenance?.source_label;
  return relation !== undefined && provenance !== undefined &&
    sanitizationReceiptMatches(provenance?.sanitization_receipt) &&
    sourceFactId !== targetFactId && Array.isArray(evidence) &&
    evidence.length > 0 && evidence.length <= 256 &&
    evidence.every((factId) => factIdMatchesOwner(factId, ownerBinding)) &&
    evidence.every((factId, index) =>
      index === 0 || (evidence[index - 1] as string) < (factId as string)) &&
    Number.isSafeInteger(relation.confidence_millionths) &&
    (relation.confidence_millionths as number) >= 0 &&
    (relation.confidence_millionths as number) <= 1_000_000 &&
    typeof label === "string" && label.length > 0 && label.trim() === label &&
    new TextEncoder().encode(label).length <= 4_096 && !/\p{Cc}/u.test(label);
}

async function acceptEffect(
  tracker: CurationTracker,
  value: unknown,
  ownerBinding: string,
): Promise<boolean> {
  const effect = record(value);
  if (effect === undefined) return false;
  switch (effect.kind) {
    case "add": {
      if (!factIdMatchesOwner(effect.fact_id, ownerBinding)) return false;
      if (
        effect.closest_fact_id !== undefined && effect.closest_fact_id !== null &&
        !factIdMatchesOwner(effect.closest_fact_id, ownerBinding)
      ) return false;
      const hasCommit = effect.commit !== undefined && effect.commit !== null;
      const snapshotMatches = effect.disposition === "added"
        ? hasCommit && effect.closest_fact_id == null && effect.similarity_millionths == null
        : effect.disposition === "near_duplicate"
        ? (!hasCommit && effect.closest_fact_id === effect.fact_id &&
            effect.similarity_millionths === 1_000_000) ||
          (hasCommit && comparisonMatches(effect))
        : effect.disposition === "possible_conflict" && hasCommit && comparisonMatches(effect);
      if (!snapshotMatches) return false;
      if (hasCommit) {
        if (!acceptCommit(tracker, effect.commit, effect.fact_id, undefined, "present")) {
          return false;
        }
        tracker.factsAdded += 1;
        appendChanged(tracker, effect.fact_id);
      }
      return true;
    }
    case "update":
      if (
        !factIdMatchesOwner(effect.fact_id, ownerBinding) ||
        !Number.isSafeInteger(effect.trust_delta_millionths) ||
        (effect.trust_delta_millionths as number) < -1_000_000 ||
        (effect.trust_delta_millionths as number) > 1_000_000 ||
        !acceptCommit(tracker, effect.commit, effect.fact_id, undefined, "present")
      ) return false;
      tracker.factsUpdated += 1;
      appendChanged(tracker, effect.fact_id);
      return true;
    case "merge": {
      const outcome = record(effect.outcome);
      const losers = outcome?.deleted_loser_fact_ids;
      const commits = outcome?.commit_receipts;
      const operationId = outcome?.operation_id;
      const winnerFactId = outcome?.winner_fact_id;
      if (
        outcome === undefined || !canonicalIdentifier(operationId) ||
        !rawSha256(outcome.input_digest) ||
        !factIdMatchesOwner(winnerFactId, ownerBinding) ||
        !Array.isArray(losers) || losers.length === 0 || losers.length > 256 ||
        new Set(losers).size !== losers.length ||
        losers.some((factId) =>
          factId === winnerFactId || !factIdMatchesOwner(factId, ownerBinding)) ||
        !Array.isArray(commits) ||
        commits.length !== losers.length + (outcome.content_updated === true ? 1 : 0)
      ) return false;
      const loserFactIds = losers as string[];
      let commitIndex = 0;
      if (outcome.content_updated === true) {
        if (!acceptCommit(
          tracker,
          commits[0],
          winnerFactId,
          2,
          "present",
        )) return false;
        appendChanged(tracker, winnerFactId);
        commitIndex = 1;
      }
      for (const [index, loser] of loserFactIds.entries()) {
        if (!acceptCommit(tracker, commits[commitIndex + index], loser, 2, "absent")) {
          return false;
        }
        appendChanged(tracker, loser);
      }
      tracker.factsMerged += losers.length;
      return true;
    }
    case "remove": {
      if (!factIdMatchesOwner(effect.target_fact_id, ownerBinding)) return false;
      const hasCommit = effect.commit !== undefined && effect.commit !== null;
      if ((effect.disposition === "removed") !== hasCommit) return false;
      if (hasCommit) {
        if (!acceptCommit(tracker, effect.commit, effect.target_fact_id, 1, "absent")) {
          return false;
        }
        tracker.factsRemoved += 1;
        appendChanged(tracker, effect.target_fact_id);
      }
      return effect.disposition === "removed" || effect.disposition === "already_removed" ||
        effect.disposition === "not_found";
    }
    case "normalize_tags": {
      if (!factIdMatchesOwner(effect.fact_id, ownerBinding)) return false;
      const identity = await canonicalSha256([
        "tracedecay.project-memory.curation-normalize-identity.v1",
        effect.fact_id,
      ]);
      if (
        tracker.durableOperationIdentities.has(identity) ||
        !acceptCommit(tracker, effect.commit, effect.fact_id, 2, "present")
      ) return false;
      tracker.durableOperationIdentities.add(identity);
      tracker.normalizedTags += 1;
      appendChanged(tracker, effect.fact_id);
      return true;
    }
    case "link_facts": {
      if (
        !factIdMatchesOwner(effect.source_fact_id, ownerBinding) ||
        !factIdMatchesOwner(effect.target_fact_id, ownerBinding)
      ) return false;
      const relation = record(effect.relation);
      if (relation === undefined) return false;
      const identity = await canonicalSha256([
        "tracedecay.project-memory.curation-link-identity.v1",
        effect.source_fact_id,
        effect.target_fact_id,
        relation.kind,
      ]);
      if (
        tracker.durableOperationIdentities.has(identity) ||
        !relationMatches(
          effect.relation,
          effect.source_fact_id,
          effect.target_fact_id,
          ownerBinding,
        )
      ) return false;
      tracker.durableOperationIdentities.add(identity);
      const hasCommit = effect.commit !== undefined && effect.commit !== null;
      if (
        (effect.disposition === "linked" &&
          (!hasCommit || !acceptCommit(
            tracker,
            effect.commit,
            effect.source_fact_id,
            1,
            "any",
          ))) ||
        (effect.disposition === "already_linked" && hasCommit) ||
        (effect.disposition !== "linked" && effect.disposition !== "already_linked")
      ) return false;
      if (hasCommit) {
        tracker.factsLinked += 1;
        appendChanged(tracker, effect.source_fact_id);
        appendChanged(tracker, effect.target_fact_id);
      }
      return true;
    }
    default:
      return false;
  }
}

async function curationReceiptMatches(runId: string, value: unknown): Promise<number | undefined> {
  const settled = record(value);
  const receipt = record(settled?.receipt);
  const effects = receipt?.operation_effects;
  const changed = receipt?.changed_fact_ids;
  const owner = record(receipt?.owner);
  if (
    settled === undefined || receipt === undefined || !ownerMatches(owner) ||
    receipt.automation_run_id !== runId || !canonicalIdentifier(receipt.operation_id) ||
    !rawSha256(receipt.input_digest) || !Array.isArray(effects) ||
    effects.length === 0 || effects.length > 256 ||
    receipt.accepted_operations !== effects.length || !Array.isArray(changed) ||
    changed.length > 256 || settled.canonical_digest !== await canonicalSha256([
      "tracedecay.automation-run.curation-receipt.v1",
      receipt,
    ]) || (owner.kind !== "profile" &&
      (owner.kind !== "project" || !canonicalIdentifier(owner.project_id)))
  ) return undefined;
  const ownerDigest = await canonicalSha256(["fact-owner.v1", owner]);
  const tracker: CurationTracker = {
    owner,
    committedEventIds: new Set(),
    durableOperationIdentities: new Set(),
    changedFactIds: [],
    factsAdded: 0,
    factsUpdated: 0,
    factsMerged: 0,
    factsRemoved: 0,
    normalizedTags: 0,
    factsLinked: 0,
  };
  for (const effect of effects) {
    if (!await acceptEffect(tracker, effect, ownerDigest.slice("sha256:".length))) {
      return undefined;
    }
  }
  if (
    (receipt.replay_fact_id ?? undefined) !== tracker.replayFactId ||
    (receipt.replay_event_id ?? undefined) !== tracker.replayEventId ||
    receipt.facts_added !== tracker.factsAdded ||
    receipt.facts_updated !== tracker.factsUpdated ||
    receipt.facts_merged !== tracker.factsMerged ||
    receipt.facts_removed !== tracker.factsRemoved ||
    receipt.normalized_tags !== tracker.normalizedTags ||
    receipt.facts_linked !== tracker.factsLinked || changed.length !== tracker.changedFactIds.length ||
    !changed.every((factId, index) => factId === tracker.changedFactIds[index])
  ) return undefined;
  return effects.length;
}

export async function factStoreCurateTerminalMatches(
  request: unknown,
  envelope: unknown,
): Promise<boolean> {
  const bounds = record(request);
  const outer = record(envelope);
  const outcome = record(outer?.outcome);
  const effect = record(outcome?.value);
  const result = record(effect?.payload);
  const terminal = record(result?.terminal);
  const summary = record(terminal?.summary);
  const receipts = result?.committed_receipts;
  if (
    bounds === undefined || outer === undefined || outcome?.outcome !== "effect" ||
    result === undefined || terminal === undefined || summary === undefined ||
    !Array.isArray(receipts) || !canonicalIdentifier(outer.request_id) ||
    result.run_id !== outer.request_id ||
    result.task !== "memory_curator"
  ) return false;

  const factReviewLimit = bounds.fact_review_limit ?? 24;
  const minimumConfidence = bounds.min_confidence_millionths ?? 720_000;
  if (
    typeof factReviewLimit !== "number" || !Number.isSafeInteger(factReviewLimit) ||
    factReviewLimit < 1 || factReviewLimit > 1_000 ||
    typeof minimumConfidence !== "number" || !Number.isSafeInteger(minimumConfidence) ||
    minimumConfidence < 0 ||
    minimumConfidence > 1_000_000
  ) return false;
  const expectedDigest = await canonicalSha256([
    "tracedecay.automation-run.request-identity.v1",
    {
      kind: "memory_curator",
      options: {
        fact_review_limit: factReviewLimit,
        min_confidence_millionths: minimumConfidence,
      },
    },
  ]);
  if (result.request_digest !== expectedDigest) return false;

  const reviewed = summary.reviewed_count;
  const accepted = summary.accepted_count;
  const rejected = summary.rejected_count;
  const skipped = summary.skipped_count;
  if (
    !boundedCount(reviewed) || !boundedCount(accepted) ||
    !boundedCount(rejected) || !boundedCount(skipped)
  ) return false;
  if (terminal.status === "skipped") {
    return reviewed === 0 && accepted === 0 && rejected === 0 && skipped === 1 &&
      receipts.length === 0 && typeof terminal.reason === "string" &&
      MEMORY_CURATOR_SKIP_REASONS.has(terminal.reason);
  }
  if (
    terminal.status !== "completed" || skipped !== 0 ||
    reviewed !== accepted + rejected || rejected !== 0 || receipts.length > 1
  ) return false;

  let acceptedOperations = 0;
  for (const item of receipts) {
    const committed = record(item);
    const settled = record(committed?.receipt);
    if (committed?.kind !== "curation" || settled === undefined) return false;
    const accepted = await curationReceiptMatches(result.run_id as string, settled);
    if (accepted === undefined) return false;
    acceptedOperations += accepted;
  }
  return accepted === acceptedOperations;
}
