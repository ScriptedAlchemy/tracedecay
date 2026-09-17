import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AutomationsPage } from "./AutomationsPage.tsx";

afterEach(() => vi.unstubAllGlobals());

/**
 * The Automations channel: five independent daemon reads drawn as one
 * auditable ledger, with an inspector that previews on hover or focus and
 * pins on click. Every assertion below is about the daemon's own words
 * reaching the screen unchanged, or a typed absence standing where a value
 * was not served.
 */
describe("AutomationsPage scheduler bay", () => {
  it("reads configured as configuration and names the observed scheduler run separately", async () => {
    stubAutomation();
    renderAutomations();

    const bay = await settledRegion("Scheduler status");
    expect(within(bay).getByText("configured")).toBeTruthy();
    expect(within(bay).getByText(/no liveness heartbeat/)).toBeTruthy();
    expect(within(bay).getByText("configuration.revision.automation.test")).toBeTruthy();
    // The one runtime fact: the newest scheduler-triggered completion.
    expect(within(bay).getByText(/observed scheduler run/i)).toBeTruthy();
    expect(within(bay).getByText(/memory_curator · run-mc-1 · completed/)).toBeTruthy();
  });

  it("says when no scheduler-triggered run exists rather than implying liveness", async () => {
    stubAutomation({
      status: scheduler({
        tasks: [{ task: "memory_curator", due: false, skip_reason: "cooldown", last_scheduler_run: null }],
      }),
    });
    renderAutomations();
    const bay = await settledRegion("Scheduler status");
    expect(within(bay).getByText(/no scheduler-triggered run is recorded for any task/)).toBeTruthy();
  });

  it("prints each scheduler task with its due flag, skip reason and last run", async () => {
    stubAutomation();
    renderAutomations();
    const tasks = await settledRegion("Scheduler tasks · due window");
    expect(within(tasks).getByText("1")).toBeTruthy();
    expect(within(tasks).getByText(/of 3 due now/)).toBeTruthy();
    const reflector = within(tasks).getByTestId("task-row-session_reflector");
    expect(within(reflector).getByText("due")).toBeTruthy();
    // The scheduler attached no last run for skill_writer, so the row falls
    // back to the loaded ledger page and says so.
    const writer = within(tasks).getByTestId("task-row-skill_writer");
    expect(within(writer).getByText("no_new_session_activity")).toBeTruthy();
    expect(within(writer).getByText(/from loaded ledger page/)).toBeTruthy();
    expect(within(writer).getByText("failed")).toBeTruthy();
    // No last run anywhere for session_reflector: a typed absence.
    expect(within(reflector).getByText(/none recorded/)).toBeTruthy();
    const curator = within(tasks).getByTestId("task-row-memory_curator");
    expect(within(curator).getByText("succeeded")).toBeTruthy();
  });

  it("tallies exactly the loaded ledger page and names that population", async () => {
    stubAutomation();
    renderAutomations();
    const readouts = await screen.findByLabelText("Ledger window tallies");
    await waitFor(() => expect(within(readouts).getAllByText(/of 3 loaded runs/).length).toBeGreaterThan(0));
    const cell = (label: string) => within(readouts).getByText(label).parentElement as HTMLElement;
    expect(within(cell("due now")).getByText("1")).toBeTruthy();
    expect(within(cell("succeeded")).getByText("1")).toBeTruthy();
    expect(within(cell("failed")).getByText("1")).toBeTruthy();
    expect(within(cell("running")).getByText("1")).toBeTruthy();
    expect(within(cell("skipped")).getByText("0")).toBeTruthy();
  });

  it("prints an em dash with the blocked state when the ledger read fails, never a zero", async () => {
    stubAutomation({ runs: () => new Response("{}", { status: 500 }) });
    renderAutomations();
    const readouts = await screen.findByLabelText("Ledger window tallies");
    await waitFor(() => expect(within(readouts).getAllByText("ledger error").length).toBeGreaterThan(0));
    const cell = within(readouts).getByText("running").parentElement as HTMLElement;
    expect(within(cell).getByText("—")).toBeTruthy();
    expect(within(cell).queryByText("0")).toBeNull();
  });
});

describe("AutomationsPage scheduler control", () => {
  it("uses the daemon response after pausing rather than optimistic state", async () => {
    let paused = false;
    const fetchMock = stubAutomation({
      status: () => jsonResponse(scheduler({ paused })),
      pause: () => {
        paused = true;
        return jsonResponse(scheduler({ paused: true, status: "paused" }));
      },
    });
    renderAutomations();

    const pause = await screen.findByRole<HTMLButtonElement>("button", { name: "Pause scheduler" });
    const resume = screen.getByRole<HTMLButtonElement>("button", { name: "Resume scheduler" });
    expect(resume.disabled).toBe(true);
    await userEvent.click(pause);

    await waitFor(() =>
      expect(screen.getByRole<HTMLButtonElement>("button", { name: "Resume scheduler" }).disabled).toBe(false),
    );
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Pause scheduler" }).disabled).toBe(true);
    expect(screen.getByText("paused")).toBeTruthy();
    const call = fetchMock.mock.calls.find(([url]) => String(url).endsWith("/scheduler/pause"));
    expect(call?.[1]?.method).toBe("POST");
  });

  it("draws no retry, cancel, run-now or approval control", async () => {
    const fetchMock = stubAutomation();
    renderAutomations();
    await settledRegion("Run ledger · latest first");
    expect(screen.queryByRole("button", { name: /^(retry|cancel|run now|approve|apply|review|plan)\b/i })).toBeNull();
    expect(fetchMock.mock.calls.some(([url]) => String(url).includes("/curation/plan"))).toBe(false);
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(false);
  });
});

describe("AutomationsPage ledgers", () => {
  it("joins a user job to its runs on the exact task key and marks the rest absent", async () => {
    stubAutomation();
    renderAutomations();
    const jobs = await settledRegion("Managed jobs · user defined");
    const nightly = within(jobs).getByTestId("job-row-nightly-sweep");
    expect(within(nightly).getByText("user_job:nightly-sweep")).toBeTruthy();
    expect(within(nightly).getByText("0 3 * * *")).toBeTruthy();
    expect(within(nightly).getByText("running")).toBeTruthy();
    const digest = within(jobs).getByTestId("job-row-pr-digest");
    expect(within(digest).getByText(/none in loaded page/)).toBeTruthy();
    expect(within(digest).getByText("disabled")).toBeTruthy();
  });

  it("prints skill state and provenance, and a typed absence where provenance is not served", async () => {
    stubAutomation({
      skills: skillsBody([
        {
          metadata: {
            id: "code-slop",
            title: "Code Slop Cleanup",
            state: "active",
            provenance: { source: "automation_run", actor: "skill_writer", run_id: "run-sw-1" },
          },
        },
        { metadata: { id: "bare", title: "Bare Skill", state: "disabled" } },
      ]),
    });
    renderAutomations();
    const skills = await settledRegion("Skills · managed");
    const slop = within(skills).getByTestId("skill-row-code-slop");
    expect(within(slop).getByText("automation run")).toBeTruthy();
    expect(within(slop).getByText(/skill_writer · run-sw-1/)).toBeTruthy();
    const bare = within(skills).getByTestId("skill-row-bare");
    expect(within(bare).getByText("disabled")).toBeTruthy();
    expect(within(bare).getAllByText(/not served/).length).toBe(2);
  });

  it("renders terminal fact receipts with their state and files them under their run", async () => {
    stubAutomation();
    renderAutomations();
    const outcomes = await settledRegion("Automatic fact outcomes · newest first");
    expect(within(outcomes).getAllByText("A recorded project fact.")).toHaveLength(2);
    expect(within(outcomes).getByText("applied")).toBeTruthy();
    expect(within(outcomes).getByText("quarantined")).toBeTruthy();
    expect(within(outcomes).getByText(/quarantine: validation failed/)).toBeTruthy();
    const ledger = await settledRegion("Run ledger · latest first");
    const curatorRun = within(ledger).getByTestId("run-row-run-mc-1");
    expect(within(curatorRun).getByText(/1 applied · 1 quarantined/)).toBeTruthy();
    const failedRun = within(ledger).getByTestId("run-row-run-sw-1");
    expect(within(failedRun).getByText(/none in loaded page/)).toBeTruthy();
  });

  it("reports an empty receipt list only when its own tally agrees", async () => {
    stubAutomation({ "automatic-fact-receipts": receiptsBody([]) });
    renderAutomations();
    const panel = await settledRegion("Automatic fact outcomes · newest first");
    expect(within(panel).getByText(/no fact application outcomes are recorded/i)).toBeTruthy();
    expect(within(panel).queryByRole("status")).toBeNull();
  });

  it("names a capped receipt page instead of claiming it is complete", async () => {
    const receipts = Array.from({ length: 50 }, (_, index) => receipt(`apply-${index}`));
    stubAutomation({ "automatic-fact-receipts": receiptsBody(receipts) });
    renderAutomations();
    const panel = await settledRegion("Automatic fact outcomes · newest first");
    expect(within(panel).getByRole("status").textContent).toContain(
      "this is the first 50 fact application outcomes",
    );
  });

  it("prints the ledger newest first with measured durations and typed absences", async () => {
    stubAutomation();
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    const rows = within(ledger).getAllByRole("button");
    expect(rows[0]?.getAttribute("aria-label")).toContain("run-nightly-1");
    expect(rows[1]?.getAttribute("aria-label")).toContain("run-mc-1");
    expect(rows[2]?.getAttribute("aria-label")).toContain("run-sw-1");
    const running = within(ledger).getByTestId("run-row-run-nightly-1");
    expect(within(running).getByText(/not settled/)).toBeTruthy();
    expect(within(running).getByText(/no artifact to verify/)).toBeTruthy();
    const curator = within(ledger).getByTestId("run-row-run-mc-1");
    expect(within(curator).getByText("00:04:00")).toBeTruthy();
    expect(within(curator).getByText("3")).toBeTruthy();
    expect(within(curator).getByText(/unchecked · inspect run/)).toBeTruthy();
    expect(within(ledger).getByText(/newest 3 runs served by the daemon · complete/)).toBeTruthy();
  });

  it("reports an empty ledger as a ledger with no runs, not a blocked read", async () => {
    stubAutomation({ runs: runsBody([]) });
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    expect(within(ledger).getByText(/no automation runs are recorded in this ledger/)).toBeTruthy();
    expect(ledger.querySelector("[data-state]")).toBeNull();
  });

  it("marks a bounded page and omitted malformed rows rather than claiming the whole ledger", async () => {
    stubAutomation({
      runs: { ...runsBody([run("run-1", { status: "succeeded" })]), has_more: true, malformed_row_count: 2, completeness: "partial" },
    });
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    expect(within(ledger).getByRole("status").textContent).toMatch(/older ledger records were outside this page; 2 malformed ledger rows were omitted/);
    expect(within(ledger).getByText(/newest 1 run served by the daemon · bounded/)).toBeTruthy();
  });
});

describe("AutomationsPage inspector", () => {
  it("previews on hover, yields to the pinned selection on leave, pins on click and clears on Escape", async () => {
    stubAutomation();
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    const inspector = screen.getByTestId("run-inspector");
    expect(within(inspector).getByText("no selection")).toBeTruthy();

    // Hover previews without selecting.
    fireEvent.pointerEnter(within(ledger).getByTestId("run-row-run-sw-1"));
    expect(within(inspector).getByText("preview")).toBeTruthy();
    expect(within(inspector).getByText(/the backend refused the run/)).toBeTruthy();
    expect(within(ledger).getByTestId("run-row-run-sw-1").getAttribute("data-selected")).toBeNull();

    // Leaving the table with nothing pinned empties the inspector again.
    fireEvent.pointerLeave(ledger.querySelector("table") as HTMLElement);
    expect(within(inspector).getByText("no selection")).toBeTruthy();

    // Click pins; the row wears aria-pressed and the gutter.
    await userEvent.click(within(ledger).getByRole("button", { name: /run-mc-1/ }));
    expect(within(inspector).getByText("selected")).toBeTruthy();
    expect(within(inspector).getByText("run-mc-1")).toBeTruthy();
    expect(within(ledger).getByRole("button", { name: /run-mc-1/ }).getAttribute("aria-pressed")).toBe("true");

    // A hover over another row previews it while the pin stays.
    fireEvent.pointerEnter(within(ledger).getByTestId("run-row-run-sw-1"));
    expect(within(inspector).getByText("preview")).toBeTruthy();
    expect(within(ledger).getByTestId("run-row-run-mc-1").getAttribute("data-selected")).toBe("true");

    // Escape clears everything.
    fireEvent.keyDown(screen.getByTestId("automations-page"), { key: "Escape" });
    expect(within(inspector).getByText("no selection")).toBeTruthy();
    expect(within(ledger).getByRole("button", { name: /run-mc-1/ }).getAttribute("aria-pressed")).toBe("false");
  });

  it("previews on keyboard focus and moves between rows with the arrow keys", async () => {
    stubAutomation();
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    const inspector = screen.getByTestId("run-inspector");
    const first = within(ledger).getByRole("button", { name: /run-nightly-1/ });
    act(() => first.focus());
    expect(within(inspector).getByText("preview")).toBeTruthy();
    expect(within(inspector).getByText("user_job:nightly-sweep")).toBeTruthy();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(document.activeElement?.getAttribute("aria-label")).toContain("run-mc-1");
    expect(within(inspector).getAllByText("run-mc-1").length).toBeGreaterThan(0);
    fireEvent.keyDown(document.activeElement as HTMLElement, { key: "End" });
    expect(document.activeElement?.getAttribute("aria-label")).toContain("run-sw-1");
  });

  it("reads artifacts only once a run is inspected and prints the daemon verdict verbatim", async () => {
    const fetchMock = stubAutomation({
      "runs/run-mc-1/artifacts": artifactsBody("run-mc-1", "ledger_publication_mismatch"),
    });
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    expect(fetchMock.mock.calls.some(([url]) => String(url).includes("/artifacts"))).toBe(false);

    await userEvent.click(within(ledger).getByRole("button", { name: /run-mc-1/ }));
    const inspector = screen.getByTestId("run-inspector");
    const artifacts = await within(inspector).findByRole("region", { name: "artifacts (3)" });
    await waitFor(() => expect(within(artifacts).getByText("ledger publication mismatch")).toBeTruthy());
    expect(within(artifacts).getByText(/1 of 2 expected kinds present · not recorded: feedback/)).toBeTruthy();
    // The ledger row's integrity cell now carries the same verdict.
    await waitFor(() =>
      expect(within(within(ledger).getByTestId("run-row-run-mc-1")).getByText("ledger publication mismatch")).toBeTruthy(),
    );
    expect(fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/runs/run-mc-1/artifacts"))).toHaveLength(1);
  });

  it("reads an artifact payload only when that artifact is chosen and guards its identity", async () => {
    const artifacts = artifactsBody("run-mc-1", "verified");
    const fetchMock = stubAutomation({
      "runs/run-mc-1/artifacts": artifacts,
      "runs/run-mc-1/artifacts/traces": {
        run_id: "run-mc-1",
        artifact: artifacts.artifacts[0],
        payload: { applied_ops: [{ op: "normalize_tags", fact_id: "fact.v1.test" }] },
        error: "",
      },
    });
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    await userEvent.click(within(ledger).getByRole("button", { name: /run-mc-1/ }));
    const inspector = screen.getByTestId("run-inspector");
    const artifactButton = await within(inspector).findByRole("button", { name: /traces/ });
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/artifacts/traces"))).toBe(false);

    await userEvent.click(artifactButton);
    const payload = await screen.findByLabelText("traces artifact payload");
    expect(payload.textContent).toContain("normalize_tags");
    expect(within(inspector).getByText("verified")).toBeTruthy();
    expect(within(inspector).getByText("a".repeat(64))).toBeTruthy();
  });

  it("issues no artifact read for a run that recorded none", async () => {
    const fetchMock = stubAutomation();
    renderAutomations();
    const ledger = await settledRegion("Run ledger · latest first");
    await userEvent.click(within(ledger).getByRole("button", { name: /run-sw-1/ }));
    const inspector = screen.getByTestId("run-inspector");
    expect(within(inspector).getByText(/this run recorded no artifacts in its ledger entry/)).toBeTruthy();
    expect(within(inspector).getByText(/no artifact to verify/)).toBeTruthy();
    // The typed error block: the class word beside its lamp, and the
    // recorded retryable flag as a word rather than a colour.
    const error = within(inspector).getByRole("region", { name: "typed error" });
    expect(within(error).getByText(/model quota exhausted/)).toBeTruthy();
    expect(within(error).getAllByText("retryable")).toHaveLength(2);
    expect(within(error).getByText("yes")).toBeTruthy();
    expect(within(inspector).getByText("3")).toBeTruthy();
    expect(fetchMock.mock.calls.some(([url]) => String(url).includes("/artifacts"))).toBe(false);
  });

  it("inspects a scheduler task and links its last run when that run is in the loaded page", async () => {
    stubAutomation();
    renderAutomations();
    const tasks = await settledRegion("Scheduler tasks · due window");
    await userEvent.click(within(tasks).getByRole("button", { name: /memory_curator/ }));
    const inspector = screen.getByTestId("run-inspector");
    expect(within(inspector).getByText("built-in scheduler task · reading from the scheduler status route")).toBeTruthy();
    expect(within(inspector).getByText("scheduler_cooldown_active")).toBeTruthy();
    await userEvent.click(within(inspector).getByRole("button", { name: "run-mc-1" }));
    expect(within(inspector).getByText("selected")).toBeTruthy();
    expect(within(inspector).getByText("00:04:00")).toBeTruthy();
  });

  it("inspects a fact receipt and says when its run is outside the loaded page", async () => {
    stubAutomation({
      "automatic-fact-receipts": receiptsBody([{ ...receipt("apply-far"), run_id: "run-elsewhere" }]),
    });
    renderAutomations();
    const outcomes = await settledRegion("Automatic fact outcomes · newest first");
    await userEvent.click(within(outcomes).getByRole("button", { name: /apply-far/ }));
    const inspector = screen.getByTestId("run-inspector");
    expect(within(inspector).getByText("run-elsewhere")).toBeTruthy();
    expect(within(inspector).getByText("not in the loaded ledger page")).toBeTruthy();
    expect(within(inspector).getByText("fact.apply-far")).toBeTruthy();
  });
});

/* ---- harness ------------------------------------------------------------ */

async function settledRegion(name: string): Promise<HTMLElement> {
  const region = await screen.findByRole("region", { name });
  await waitFor(() => expect(region.querySelector('[data-state="loading"]')).toBeNull());
  return region;
}

function renderAutomations() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <AutomationsPage />
    </QueryClientProvider>,
  );
}

const NOW = 1_754_000_000;

function scheduler(overrides: { paused?: boolean; status?: string; tasks?: unknown[] } = {}) {
  return {
    status: overrides.status ?? "configured",
    paused: overrides.paused ?? false,
    enabled: true,
    scheduler_tick_secs: 900,
    now: NOW,
    last_session_activity: NOW - 1200,
    configuration_revision_id: "configuration.revision.automation.test",
    control_path: "/x/automation.control.json",
    tasks: overrides.tasks ?? [
      {
        task: "memory_curator",
        due: false,
        skip_reason: "scheduler_cooldown_active",
        last_scheduler_run: run("run-mc-1", { task: "memory_curator", status: "succeeded" }),
      },
      { task: "session_reflector", due: true, skip_reason: null, last_scheduler_run: null },
      { task: "skill_writer", due: false, skip_reason: "no_new_session_activity", last_scheduler_run: null },
    ],
  };
}

function jobsBody(jobs: unknown[], count = jobs.length) {
  return { jobs, count };
}

function skillsBody(skills: unknown[], count = skills.length) {
  return {
    profile_root: "/home/x/.tracedecay",
    skills_root: "/home/x/.tracedecay/managed-skills",
    count,
    skills,
    skill_metadata: [],
    usage_summaries: [],
    stale_recommendations: [],
    improvement_recommendations: [],
  };
}

function receiptsBody(receipts: unknown[], count = receipts.length) {
  return { receipts, count, limit: 50, error: "" };
}

function receipt(id: string, state: "applied" | "quarantined" = "applied") {
  return {
    schema_version: 1,
    apply_id: id,
    run_id: "run-mc-1",
    state,
    evidence_hash: `evidence.${id}`,
    add_fact_request: { content: "A recorded project fact.", category: "preference" },
    quarantine_reason: state === "quarantined" ? "validation failed" : undefined,
    validation: { disposition: state === "applied" ? "accepted" : "rejected", policy: "automatic-memory-v1" },
    applied_fact_id: state === "applied" ? `fact.${id}` : undefined,
    recorded_at_micros: NOW * 1_000_000,
  };
}

function run(
  id: string,
  options: {
    task?: string;
    taskKey?: string | null;
    status: string;
    error?: string;
    errorClass?: string;
    attempts?: number;
    artifactKinds?: string[];
    startedAt?: number;
    completedAt?: number | "";
  },
) {
  const task = options.task ?? "memory_curator";
  const started = options.startedAt ?? NOW - 2 * 86_400;
  return {
    run_id: id,
    task,
    task_key: options.taskKey === undefined ? task : options.taskKey,
    trigger: "scheduler",
    backend: "claude",
    model: "claude-sonnet-5",
    status: options.status,
    reviewed_count: 6,
    accepted_count: 4,
    rejected_count: 2,
    skipped_count: 0,
    error: options.error ?? null,
    error_classification: options.errorClass ?? null,
    error_retryable: options.errorClass ? true : null,
    backend_attempt_count: options.attempts ?? 1,
    started_at: String(started),
    completed_at: options.completedAt === undefined ? String(started + 240) : String(options.completedAt),
    artifact_kinds: options.artifactKinds ?? [],
  };
}

function runsBody(rows: unknown[]) {
  return { runs: rows, count: rows.length, limit: 50, has_more: false, malformed_row_count: 0, completeness: "known", error: "" };
}

function artifactsBody(runId: string, integrity: string) {
  return {
    run_id: runId,
    artifacts: [{ kind: "traces", path: `runs/${runId}/traces.json`, sha256: "a".repeat(64), created_at: String(NOW) }],
    artifact_chain: {
      expected_kinds: ["traces", "feedback"],
      present_kinds: ["traces"],
      metadata_complete: false,
      complete: false,
      integrity_status: integrity,
    },
    count: 1,
    error: "",
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
}

type Reply = unknown | (() => Response);

/** Routes are matched on the path after `/api/automation/`, so a per-run
 * artifact route is keyed as `runs/<id>/artifacts` and a payload as
 * `runs/<id>/artifacts/<kind>`. */
function stubAutomation(overrides: Record<string, Reply> = {}) {
  const fallbacks: Record<string, unknown> = {
    "scheduler/status": scheduler(),
    jobs: jobsBody([
      {
        id: "nightly-sweep",
        name: "Nightly sweep",
        schedule: "0 3 * * *",
        enabled: true,
        interval_secs: null,
        cooldown_secs: 1800,
        skill_ids: ["code-slop"],
        delivery: { mode: "file" },
        created_at: NOW - 86_400,
        updated_at: NOW - 3600,
      },
      { id: "pr-digest", name: "PR digest", schedule: null, enabled: false, interval_secs: 3600 },
    ]),
    skills: skillsBody([{ metadata: { id: "code-slop", title: "Code Slop Cleanup", state: "active" } }]),
    "automatic-fact-receipts": receiptsBody([receipt("apply-1"), receipt("apply-2", "quarantined")]),
    runs: runsBody([
      run("run-nightly-1", {
        task: "user_job",
        taskKey: "user_job:nightly-sweep",
        status: "running",
        startedAt: NOW - 90,
        completedAt: "",
      }),
      run("run-mc-1", { task: "memory_curator", status: "succeeded", artifactKinds: ["traces", "feedback", "validation_gate"] }),
      run("run-sw-1", {
        task: "skill_writer",
        status: "failed",
        error: "the backend refused the run: model quota exhausted",
        errorClass: "retryable",
        attempts: 3,
        startedAt: NOW - 3 * 86_400,
      }),
    ]),
  };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input).split("?")[0] ?? "";
    const key = url.slice(url.indexOf("/api/automation/") + "/api/automation/".length);
    // Accept the short form for the two control routes and the lists.
    const short = key.startsWith("scheduler/") ? key.slice("scheduler/".length) : key;
    const reply = key in overrides ? overrides[key] : short in overrides ? overrides[short] : (fallbacks[key] ?? fallbacks[short] ?? {});
    void init;
    return typeof reply === "function" ? (reply as () => Response)() : jsonResponse(reply);
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}
