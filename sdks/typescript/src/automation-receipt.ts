/** Cross-field validation for the public automatic-curation receipt. */

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
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

const RECEIPT_FIELDS = ["request_digest", "run_id", "state", "task"];

/**
 * `fact_store_curate` answers at admission. The receipt must name exactly the
 * run this request admitted; the run's terminal is read afterwards from the
 * automation run ledger (`automation_run_view`).
 */
export async function factStoreCurateReceiptMatches(
  request: unknown,
  envelope: unknown,
): Promise<boolean> {
  const bounds = record(request);
  const outer = record(envelope);
  const outcome = record(outer?.outcome);
  const effect = record(outcome?.value);
  const receipt = record(effect?.payload);
  if (
    bounds === undefined || outer === undefined || outcome?.outcome !== "effect" ||
    receipt === undefined || !canonicalIdentifier(outer.request_id) ||
    Object.keys(receipt).sort().join(",") !== RECEIPT_FIELDS.join(",") ||
    receipt.run_id !== outer.request_id || receipt.task !== "memory_curator" ||
    receipt.state !== "started"
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
  return receipt.request_digest === await canonicalSha256([
    "tracedecay.automation-run.request-identity.v1",
    {
      kind: "memory_curator",
      options: {
        fact_review_limit: factReviewLimit,
        min_confidence_millionths: minimumConfidence,
      },
    },
  ]);
}
