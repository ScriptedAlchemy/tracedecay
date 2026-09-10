import { useEffect, type Dispatch, type ReactNode, type SetStateAction } from "react";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { CHECK_MATRIX, PR743_STEPS, PR743_TRANSCRIPT, REVIEW_THREADS, STORY_STEPS } from "./data";
import { GradeMark, HonestMark } from "./marks";
import { isLocalFeedback, REVIEW_707_REVISION, type FeedbackLifecycle, type LocalFeedback, type ReviewMode } from "./review-state";

function ChipButton(props: {
  children: ReactNode;
  onClick: () => void;
  pressed?: boolean;
  disabled?: boolean;
  title?: string;
}) {
  return (
    <button
      type="button"
      className="chip"
      style={{ background: "transparent", cursor: props.disabled ? "default" : "pointer" }}
      aria-pressed={props.pressed}
      disabled={props.disabled}
      title={props.title}
      onClick={props.onClick}
    >
      {props.children}
    </button>
  );
}

function ReviewTabs(props: { mode: ReviewMode; onMode: (mode: ReviewMode) => void }) {
  return (
    <>
      {(["story", "code", "evidence", "feedback"] as const).map((mode) => (
        <button
          key={mode}
          type="button"
          className={props.mode === mode ? "is-on" : ""}
          aria-pressed={props.mode === mode}
          onClick={() => props.onMode(mode)}
        >
          {mode === "code" ? "CODE & IMPACT" : mode.toUpperCase()}
        </button>
      ))}
    </>
  );
}

function beginLocalFeedback(
  current: LocalFeedback | null,
  next: LocalFeedback,
  setCurrent: Dispatch<SetStateAction<LocalFeedback | null>>,
  setHistory: Dispatch<SetStateAction<LocalFeedback[]>>,
) {
  if (current && (current.body ?? "").trim()) {
    setHistory((history) => {
      const valid = history.filter(isLocalFeedback);
      return valid.some((item) => item.id === current.id) ? valid : [...valid, current];
    });
  }
  setCurrent(next);
}

function useLocalFeedbackState(scope: "08" | "09" | "10") {
  const [storedFeedback, setFeedback] = useWorkspaceState<LocalFeedback | null>(`delivery:${scope}:feedback`, null);
  const [storedHistory, setFeedbackHistory] = useWorkspaceState<LocalFeedback[]>(`delivery:${scope}:feedback-history`, []);
  const feedback = isLocalFeedback(storedFeedback) ? storedFeedback : null;
  const feedbackHistory = storedHistory.filter(isLocalFeedback);
  useEffect(() => {
    if (storedFeedback !== null && !isLocalFeedback(storedFeedback)) setFeedback(null);
    const validHistory = storedHistory.filter(isLocalFeedback);
    if (validHistory.length !== storedHistory.length) setFeedbackHistory(validHistory);
  }, [setFeedback, setFeedbackHistory, storedFeedback, storedHistory]);
  return { feedback, setFeedback, feedbackHistory, setFeedbackHistory };
}

function LocalFeedbackPanel(props: { feedback: LocalFeedback | null; history: LocalFeedback[]; onFeedback: (feedback: LocalFeedback) => void }) {
  if (!props.feedback) return <p className="dl-microbody">No local feedback attached to this revision.</p>;
  const feedback = props.feedback;
  const setLifecycle = (lifecycle: FeedbackLifecycle) => props.onFeedback({ ...feedback, lifecycle });
  const sourceRequired = !feedback.sourceRef?.trim();
  const bodyRequired = !(feedback.body ?? "").trim();
  const canResolve = feedback.lifecycle === "acknowledged" && !sourceRequired;
  const terminal = feedback.lifecycle === "acted-upon" || feedback.lifecycle === "contradicted";
  return (
    <div className="dl-fbox">
      <div className="fk">LOCAL TRACEDECAY FEEDBACK · PROVIDER READ ONLY</div>
      <div className="fr"><span className="fl mono">{feedback.anchor}</span><b className="mono">@ {feedback.revision}</b></div>
      <div className="fr"><span className="fl">Lifecycle</span><b>{feedback.lifecycle}</b></div>
      <label className="dl-microbody" style={{ display: "block", marginTop: 6 }}>
        Local {feedback.kind}
        <textarea
          className="dl-search"
          value={feedback.body ?? ""}
          placeholder="Write feedback attached to this exact anchor and revision"
          disabled={terminal}
          onChange={(event) => props.onFeedback({ ...feedback, body: event.target.value })}
          style={{ minHeight: 56, resize: "vertical" }}
        />
      </label>
      <label className="dl-microbody" style={{ display: "block", marginTop: 6 }}>
        User-supplied correction / resolution reference (not validated)
        <input
          className="dl-search"
          value={feedback.sourceRef ?? ""}
          placeholder="Required for acted-upon or contradicted"
          disabled={terminal}
          onChange={(event) => props.onFeedback({ ...feedback, sourceRef: event.target.value })}
        />
      </label>
      <div className="dl-actionrow" style={{ paddingInline: 0 }}>
        <ChipButton onClick={() => undefined} pressed={feedback.lifecycle === "open"} disabled>Open</ChipButton>
        <ChipButton onClick={() => setLifecycle("acknowledged")} pressed={feedback.lifecycle === "acknowledged"} disabled={feedback.lifecycle !== "open" || bodyRequired}>Acknowledge</ChipButton>
        <ChipButton onClick={() => setLifecycle("acted-upon")} pressed={feedback.lifecycle === "acted-upon"} disabled={!canResolve} title={!canResolve ? "Acknowledge and enter an exact source reference first" : undefined}>Acted upon</ChipButton>
        <ChipButton onClick={() => setLifecycle("contradicted")} pressed={feedback.lifecycle === "contradicted"} disabled={!canResolve} title={!canResolve ? "Acknowledge and enter an exact source reference first" : undefined}>Contradicted</ChipButton>
      </div>
      {bodyRequired ? <p className="dl-hint">Enter the local feedback text before acknowledging it.</p> : null}
      {sourceRequired ? <p className="dl-hint">A correction, test, or CI result is not inferred. Any entered reference is user supplied and not validated against source or CI.</p> : null}
      {props.history.length ? (
        <div className="dl-fbox" style={{ marginTop: 8 }}>
          <div className="fk">EARLIER LOCAL FEEDBACK ({props.history.length})</div>
          {props.history.map((item) => <div className="fr" key={item.id}><span className="fl">{item.kind}: {item.body}</span><b>{item.lifecycle} · {item.revision}</b></div>)}
        </div>
      ) : null}
    </div>
  );
}

function useReviewKeys(actions: { previous: () => void; next: () => void; focus?: () => void; journey?: () => void; evidence?: () => void }) {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.isContentEditable || target?.matches("input, textarea, select")) return;
      if (event.altKey && event.key === "ArrowLeft") actions.previous();
      else if (event.altKey && event.key === "ArrowRight") actions.next();
      else if (!event.altKey && event.key.toLowerCase() === "f") actions.focus?.();
      else if (!event.altKey && event.key.toLowerCase() === "j") actions.journey?.();
      else if (!event.altKey && event.key.toLowerCase() === "e") actions.evidence?.();
      else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [actions.evidence, actions.focus, actions.journey, actions.next, actions.previous]);
}

function DiffLine(props: { a?: string; b?: string; kind: "ctx" | "add" | "del" | "hi" | "hunk"; text: string; beacon?: string }) {
  const cls =
    props.kind === "add" ? "ln add" : props.kind === "del" ? "ln del" : props.kind === "hi" ? "ln hi" : props.kind === "hunk" ? "ln hunk" : "ln";
  return (
    <div className={cls}>
      <span className="n">{props.a ?? ""}</span>
      <span className="n">{props.b ?? ""}</span>
      <span>{props.text}</span>
      {props.beacon ? <strong style={{ marginLeft: "auto", padding: "0 5px", color: "var(--activity-amber)", border: "1px solid currentColor", fontSize: 8, whiteSpace: "nowrap" }}>{props.beacon}</strong> : null}
    </div>
  );
}

const LANE_TILES = [
  { label: "CURRENT", n: 5, color: "var(--signal-cyan)" },
  { label: "OUTDATED", n: 2, color: "var(--activity-amber)" },
  { label: "RESOLVED", n: 1, color: "var(--state-ready)" },
  { label: "EDITED", n: 0, color: "var(--state-violet)" },
  { label: "DELETED", n: 0, color: "var(--state-danger)" },
];

const COMMITS_707 = [
  ["d4e56a", "feat(ingest): add exponential backoff", "12h ago"],
  ["c3b2a1d", "test(ingest): backoff unit tests", "12h ago"],
  ["b7a8b9d", "refactor(retry): extract jitter util", "13h ago"],
  ["a6f5e4d", "docs: update ingest retry docs", "13h ago"],
  ["9e8d7c6", "fix(ingest): add retry context", "14h ago"],
  ["8c7b6a5", "chore: rename retry config key", "15h ago"],
  ["7b6a5c4", "chore: lint fixes", "15h ago"],
];

export function ReviewCoverage() {
  const [threadIndex, setThreadIndex] = useWorkspaceState("delivery:08:thread", 0);
  const { feedback, setFeedback, feedbackHistory, setFeedbackHistory } = useLocalFeedbackState("08");
  const selectedThread = REVIEW_THREADS[threadIndex];
  return (
    <div className="dl-stage is-review">
      <aside className="dl-pane">
        <h3>
          PULL REQUESTS <span>#707</span>
        </h3>
        <div className="dl-scroll">
          {[
            ["#707 feat: add ingest retry backoff", "12h ago", "8", true],
            ["#704 fix: memory leak in parser", "1d ago", "4", false],
            ["#702 chore: deps bump", "2d ago", "2", false],
            ["#699 feat: export metrics", "2d ago", "6", false],
            ["#697 refactor: rule engine", "3d ago", "3", false],
            ["#694 test: add edge cases", "4d ago", "1", false],
          ].map(([t, age, n, on]) => (
            <div className={on ? "dl-navitem is-on" : "dl-navitem"} key={t as string}>
              <b>{t as string}</b>
              <em>
                {age as string} · {n as string}
              </em>
            </div>
          ))}
          <div className="more mono" style={{ fontSize: 9, color: "var(--ink-muted)", padding: "3px 0 8px" }}>
            Load more…
          </div>
          <div className="dl-fbox">
            <div className="fk">
              BRANCH / WORKTREE <span>HEAD</span>
            </div>
            <div className="fr">
              <span className="fl mono" style={{ color: "var(--signal-cyan)" }}>
                feature/ingest-backoff
              </span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">
              IDENTITY (EXACT) <span><GradeMark grade="EXACT" /></span>
            </div>
            {[
              ["BASE", "main (a1b2c3d)"],
              ["HEAD", "feature/ingest-backoff (d4e56a)"],
              ["MERGE-BASE", "base of #707 (9f8e7d6)"],
            ].map(([k, v]) => (
              <div className="fr" key={k}>
                <span className="fl">{k}</span>
                <b className="mono">{v}</b>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">SCOPE</div>
            <div className="fr">
              <span className="fl">Changed files</span>
              <b>12 / 48</b>
            </div>
            <div className="fr">
              <span className="fl">Commits</span>
              <b>7</b>
            </div>
            <div className="fr">
              <span className="fl">Review threads</span>
              <b>8</b>
            </div>
            <p className="dl-microbody">(current 5 / outdated 2 / resolved 1 / edited 0 / deleted 0)</p>
          </div>
          <div className="dl-fbox">
            <div className="fk">COMMITS (7)</div>
            {COMMITS_707.map(([h, msg, age]) => (
              <div className="fr" key={h}>
                <span className="fl mono">
                  <em style={{ color: "var(--signal-cyan)", fontStyle: "normal" }}>{h}</em> {msg}
                </span>
                <b>{age}</b>
              </div>
            ))}
          </div>
          <p className="dl-microbody">All times observed from provider.</p>
        </div>
      </aside>
      <section className="dl-pane">
        <h3>
          REVIEW LANES <span>anchors remapped to current HEAD</span>
        </h3>
        <div className="dl-lanetiles">
          {LANE_TILES.map((t) => (
            <div className="tile" key={t.label} style={{ borderColor: `color-mix(in oklab, ${t.color} 55%, var(--edge-subtle))` }}>
              <span className="l" style={{ color: t.color }}>
                {t.label}
              </span>
              <b>{t.n}</b>
              <span className="s">threads</span>
            </div>
          ))}
        </div>
        <div className="dl-anchor" aria-label="Revision-specific review coverage">
          <span className="k">LOCAL REVIEW COVERAGE</span>
          <span className="mono path">BASE {REVIEW_707_REVISION.base} → REVIEWED {REVIEW_707_REVISION.reviewed} → HEAD {REVIEW_707_REVISION.head}</span>
          <span className="grow" />
          <span className="dl-att tone-amber">4 HEAD LINES CHANGED SINCE LOCAL REVIEW</span>
        </div>
        <div className="dl-anchor">
          <span className="k">ANCHOR (CURRENT THREAD)</span>
          <span className="mono path">{selectedThread.file}:{selectedThread.line}</span>
          <span className="grow" />
          <span className="mono url">Open in provider · https://github.com/tracedecay/tracedecay/pull/707#discussion_{selectedThread.id.toLowerCase()} ⧉</span>
        </div>
        <div className="dl-diffsplit">
          <div className="dl-diff">
            <DiffLine kind="hunk" text="⌐ src/ingest/retry.ts   @@ -132,11 +132,18 @@ export async function ingestWithRetry(input: Input): Promise<Result> {" />
            <DiffLine a="132" b="132" kind="ctx" text="    let attempt = 0;" />
            <DiffLine a="133" b="133" kind="ctx" text="    const maxAttempts = cfg.maxAttempts ?? 5;" />
            <DiffLine a="134" b="134" kind="ctx" text="    const baseDelayMs = cfg.baseDelayMs ?? 250;" />
            <DiffLine a="135" b="135" kind="ctx" text="    const jitter = cfg.jitterPct ?? 0.2;" />
            <DiffLine a="136" b="136" kind="del" text="-   const shouldRetry = (err: Error) => isRetryable(err);" beacon="CHANGED SINCE REVIEW" />
            <DiffLine a="137" b="" kind="ctx" text="" />
            <DiffLine a="" b="136" kind="add" text="+   const shouldRetry = (err: Error, attempt: number) => {" beacon="NEW AT HEAD" />
            <DiffLine a="" b="137" kind="add" text="+     if (attempt >= maxAttempts) return false;" beacon="NEW AT HEAD" />
            <DiffLine a="" b="138" kind="add" text="+     return isRetryable(err);" beacon="NEW AT HEAD" />
            <DiffLine a="" b="139" kind="add" text="+   };" beacon="NEW AT HEAD" />
            <DiffLine a="138" b="140" kind="ctx" text="    while (true) {" />
            <DiffLine a="139" b="141" kind="ctx" text="      try {" />
            <DiffLine a="140" b="142" kind="ctx" text="        return await doIngest(input);" />
            <DiffLine a="141" b="143" kind="ctx" text="      } catch (err) {" />
            <DiffLine a="142" b="144" kind="hi" text="        if (!shouldRetry(err, attempt)) {   ⓘ" />
            <DiffLine a="143" b="145" kind="ctx" text="          throw err;" />
            <DiffLine a="144" b="146" kind="ctx" text="        }" />
            <DiffLine a="145" b="147" kind="ctx" text="        const delay = backoff(attempt, baseDelayMs, jitter);" />
            <DiffLine a="146" b="148" kind="ctx" text="        await wait(delay);" />
            <DiffLine a="147" b="149" kind="ctx" text="        attempt += 1;" />
          </div>
          <div className="dl-threadcol">
            <div className="dl-kicker">THREADS ON THIS FILE (3)</div>
            {REVIEW_THREADS.map((t, i) => (
              <button
                type="button"
                className={i === threadIndex ? "dl-thread is-on" : "dl-thread"}
                aria-pressed={i === threadIndex}
                onClick={() => setThreadIndex(i)}
                key={t.id}
                style={{ display: "block", width: "100%", background: "transparent", color: "inherit", textAlign: "left", cursor: "pointer" }}
              >
                <div className="l1 mono">
                  {t.id} <em>{t.state}</em>
                </div>
                <div className="l2 mono">
                  {t.file}:{t.line}
                </div>
                <div className="l3">{t.body}</div>
                <div className="l4 mono">{t.age}</div>
              </button>
            ))}
          </div>
        </div>
        <div className="dl-matrixrow">
          <div className="dl-scroll" style={{ flex: "0 0 auto", maxHeight: 196, padding: 0 }}>
            <div className="dl-kicker">CHECK MATRIX (WORKFLOW / JOB / CHECK)</div>
            <table className="dl-matrix">
              <thead>
                <tr>
                  <th>WORKFLOW</th>
                  <th>JOB</th>
                  <th>CHECK</th>
                  <th>STATUS</th>
                  <th>OBSERVED AT</th>
                  <th>PROVIDER OUTCOME</th>
                  <th>DETAILS</th>
                </tr>
              </thead>
              <tbody>
                {CHECK_MATRIX.map((c) => (
                  <tr key={`${c.wf}-${c.check}`}>
                    <td>{c.wf}</td>
                    <td>{c.job}</td>
                    <td>{c.check}</td>
                    <td>
                      {c.status === "Success" ? (
                        <span style={{ color: "var(--state-ready)" }}>✓ Success</span>
                      ) : c.status === "Failure" ? (
                        <span style={{ color: "var(--state-danger)" }}>✕ Failure</span>
                      ) : c.status === "Skipped" ? (
                        <span style={{ color: "var(--ink-muted)" }}>⊘ Skipped</span>
                      ) : (
                        <span>—</span>
                      )}
                    </td>
                    <td>{c.observed}</td>
                    <td>
                      {c.provider === "COMPLETE" ? (
                        <span className="dl-att tone-ready">COMPLETE</span>
                      ) : c.provider === "RATE-LIMITED" ? (
                        <HonestMark id="rate-limited" />
                      ) : c.provider === "DENIED" ? (
                        <HonestMark id="denied" />
                      ) : c.provider === "NOT_PUBLISHED" ? (
                        <HonestMark id="not_published" />
                      ) : (
                        <HonestMark id="unavailable" />
                      )}
                    </td>
                    <td>
                      {c.check === "Integration Tests"
                        ? "See details →"
                        : c.provider === "RATE-LIMITED"
                          ? "Limited retrieval"
                          : c.provider === "DENIED"
                            ? "Access denied"
                            : c.provider === "NOT_PUBLISHED"
                              ? "Protected env"
                              : c.provider === "UNAVAILABLE"
                                ? "Not configured"
                                : "—"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="dl-fbox" style={{ marginBottom: 0 }}>
            <div className="fk">LEGEND (PROVIDER OUTCOME)</div>
            {[
              { label: "COMPLETE", note: "All data retrieved", tone: "ready" },
              { label: "PARTIAL", note: "Some data missing", tone: "amber" },
              { label: "UNAVAILABLE", note: "Provider no data", tone: "violet" },
              { label: "DENIED", note: "Access denied", tone: "danger" },
              { label: "RATE-LIMITED", note: "Retrieval limited", tone: "amber" },
              { label: "STALE", note: "Outdated data", tone: "amber" },
              { label: "FAILED", note: "Check failed", tone: "danger" },
            ].map((l) => (
              <div className="fr" key={l.label}>
                <span className="fl">
                  <i className="mark" style={{ background: `var(--${l.tone === "ready" ? "state-ready" : l.tone === "amber" ? "activity-amber" : l.tone === "violet" ? "state-violet" : "state-danger"})` }} />
                  {l.label}
                </span>
                <b style={{ color: "var(--ink-muted)", fontWeight: 400 }}>{l.note}</b>
              </div>
            ))}
          </div>
        </div>
        <div className="dl-failloc">
          <span className="k">
            <i>●</i> CI FAILURE LOCALIZATION
          </span>
          <span className="seg mono">
            <em>Integration Tests</em> · Failing tests observed <b style={{ color: "var(--state-danger)" }}>3</b>
          </span>
          <span className="seg mono">First failing commit range · 9f8e7d6…d4e56a</span>
          <span className="seg mono">Likely area · src/ingest/retry.ts, src/ingest/*/*.test.ts</span>
        </div>
      </section>
      <aside className="dl-pane">
        <h3>
          SELECTED REVIEW THREAD <span>{selectedThread.state} · Observed {selectedThread.age}</span>
        </h3>
        <div className="dl-scroll">
          <div className="dl-umbtitle">
            <b style={{ color: "var(--signal-cyan)" }}>{selectedThread.id}</b>
          </div>
          <div className="dl-fbox">
            {[
              ["Provider", "github.com"],
              ["Author class", "Code Reviewer"],
              ["Author", "octocat"],
              ["Observed at", "2025-05-09 14:36:59 UTC"],
              ["Thread state", selectedThread.state.toLowerCase()],
            ].map(([k, v]) => (
              <div className="fr" key={k}>
                <span className="fl">{k}</span>
                <b className="mono">{v}</b>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">BODY (EXCERPT) · Available (excerpts only)</div>
            <p className="dl-microbody">
              “{selectedThread.body}” <em>(truncated)</em>
            </p>
          </div>
          <div className="dl-fbox">
            <div className="fk">ANCHORS</div>
            <div className="fr">
              <span className="fl">Current (remapped)</span>
              <b className="mono">{selectedThread.file}:{selectedThread.line} (HEAD)</b>
            </div>
            <div className="fr">
              <span className="fl">Original</span>
              <b className="mono">{threadIndex === 0 ? "retry.ts:140 (as submitted)" : "Provider original anchor unavailable"}</b>
            </div>
            <div className="fr">
              <span className="fl">Freshness</span>
              <b className="mono">{selectedThread.state === "OUTDATED" ? `Outdated: ${selectedThread.age}` : `Fresh: ${selectedThread.age}`}</b>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">CALLERS (TOP 3) · IMPLEMENTATIONS (1)</div>
            {["src/ingest/runner.ts:87", "src/ingest/cli.ts:19", "src/ingest/scheduler.ts:103", "src/ingest/retry.ts:135"].map((x) => (
              <div className="fr" key={x}>
                <span className="fl mono">{x}</span>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">AFFECTED TESTS (OBSERVED)</div>
            <div className="fr">
              <span className="fl mono">src/ingest/retry.test.ts</span>
            </div>
            <div className="fr">
              <span className="fl mono">src/ingest/runner.integration.test.ts</span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">BRANCH DIAGNOSTICS</div>
            {[
              ["Ahead/Behind", "+3 / -1"],
              ["Divergence from base", "4 commits"],
              ["Conflicts", "None"],
              ["Force-pushed", "None"],
            ].map(([k, v]) => (
              <div className="fr" key={k}>
                <span className="fl">{k}</span>
                <b className="mono">{v}</b>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">CI CONTEXT (THIS THREAD)</div>
            <div className="fr">
              <span className="fl mono">Integration Tests</span>
              <b style={{ color: "var(--state-danger)" }}>FAILED</b>
            </div>
            <div className="fr">
              <span className="fl">See CHECK MATRIX</span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">PROVIDER REVIEW (OBSERVED)</div>
            <div className="fr">
              <span className="fl">Approval state</span>
              <b style={{ color: "var(--activity-amber)" }}>Changes requested</b>
            </div>
            <div className="fr">
              <span className="fl">Review summary</span>
              <b>See thread body</b>
            </div>
          </div>
          <div className="dl-actionrow" style={{ paddingInline: 0 }}>
            <ChipButton onClick={() => beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "comment", body: "", anchor: `${selectedThread.file}:${selectedThread.line}`, revision: REVIEW_707_REVISION.head, lifecycle: "open" }, setFeedback, setFeedbackHistory)}>Comment locally</ChipButton>
            <ChipButton onClick={() => beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "challenge", body: "", anchor: `${selectedThread.file}:${selectedThread.line}`, revision: REVIEW_707_REVISION.head, lifecycle: "open" }, setFeedback, setFeedbackHistory)}>Challenge locally</ChipButton>
          </div>
          <LocalFeedbackPanel feedback={feedback} history={feedbackHistory} onFeedback={setFeedback} />
          <p className="dl-hint">
            Provider review is OBSERVED data, not correctness authority. Review attention is bound to named sources on
            the selected hunk — not a numeric PR risk score.
          </p>
        </div>
      </aside>
    </div>
  );
}

/* ---- State 09: Follow the Story review workspace ---- */

function WfChip(props: { label: string; state?: "on" | "guard" | "dead" | "new" }) {
  const cls =
    props.state === "on"
      ? "dl-wfchip is-on"
      : props.state === "guard"
        ? "dl-wfchip is-guard"
        : props.state === "dead"
          ? "dl-wfchip is-dead"
          : props.state === "new"
            ? "dl-wfchip is-new"
            : "dl-wfchip";
  return (
    <span className={cls}>
      {props.label}
      <em>(Job)</em>
    </span>
  );
}

export function FollowStory() {
  const [stepIndex, setStepIndex] = useWorkspaceState("delivery:09:step", 3);
  const [mode, setMode] = useWorkspaceState<ReviewMode>("delivery:09:mode", "code");
  const [reviewMark, setReviewMark] = useWorkspaceState("delivery:09:mark", "Understood");
  const { feedback, setFeedback, feedbackHistory, setFeedbackHistory } = useLocalFeedbackState("09");
  const previous = () => setStepIndex((index) => Math.max(0, index - 1));
  const next = () => setStepIndex((index) => Math.min(STORY_STEPS.length - 1, index + 1));
  const selectedStep = STORY_STEPS[stepIndex];
  useReviewKeys({ previous, next });
  return (
    <div className="dl-stage is-story">
      <div className="dl-storyhead">
        <span className="crumb mono">Separate authored example / Compiler cache / PR #8127<br/><a href="?data=fixture&surface=delivery&state=04&pr=707">Return to PR707 journey example</a></span>
        <span className="dl-att tone-violet">SYNTHETIC RECONSTRUCTION</span>
        <span className="grow" />
        <ChipButton onClick={previous} disabled={stepIndex === 0}>‹ Previous</ChipButton>
        <ChipButton onClick={next} disabled={stepIndex === STORY_STEPS.length - 1}>Next ›</ChipButton>
      </div>
      <div className="dl-storybar" aria-label="Review episodes">
        {STORY_STEPS.map((s, index) => (
          <button
            type="button"
            key={s.n}
            className={index === stepIndex ? "st is-active" : index < stepIndex ? "st is-done" : "st"}
            aria-pressed={index === stepIndex}
            onClick={() => setStepIndex(index)}
            style={{ appearance: "none", backgroundColor: index === stepIndex ? "color-mix(in oklab, var(--signal-cyan) 10%, #070b12)" : "#070b12", color: "inherit", textAlign: "left", cursor: "pointer" }}
          >
            <span className="n">{index < stepIndex ? "✓" : s.n}</span>
            <span className="l">
              {s.n} {s.label}
              {index === stepIndex ? <em className="act">ACTIVE</em> : null}
            </span>
            <span className="t">{s.time}</span>
          </button>
        ))}
      </div>
      <div className="dl-storyhead sub">
        <span className="mono">Review coverage at b7f91a2: 4 of 7 episodes · 62% code reviewed</span>
        <span className="grow" />
        <span className="mono" style={{ color: "var(--state-danger)" }}>
          ⊙ Unresolved: 2
        </span>
        <span className="mono" style={{ color: "var(--activity-amber)" }}>
          ⚠ Risky: 1
        </span>
      </div>
      <div className="dl-split story4">
        <aside className="dl-pane">
          <h3>
            BUNDLES <span>3</span>
          </h3>
          <div className="dl-scroll">
            <input className="dl-search" placeholder="Search bundles…" readOnly />
            {[
              { label: "workflow", n: "12 agents", c: "#5ee7ff", on: true },
              { label: "bench corpus", n: "16 agents", c: "#9be15d", on: false },
              { label: "review fixes", n: "9 agents", c: "#c084fc", on: false },
            ].map((b) => (
              <div className={b.on ? "dl-navitem is-on" : "dl-navitem"} key={b.label}>
                <b>
                  <i className="mark" style={{ background: b.c, width: 7, height: 7, borderRadius: "50%", display: "inline-block", marginRight: 6 }} />
                  {b.label}
                </b>
                <em>{b.n} ›</em>
              </div>
            ))}
            <div className="dl-kicker" style={{ marginTop: 12 }}>
              MINIMAP
            </div>
            <div className="dl-storymap">
              <svg viewBox="0 0 120 64" preserveAspectRatio="none" aria-hidden="true">
                {[
                  { y: 10, c: "#5ee7ff", seg: [4, 58, 70, 112] },
                  { y: 22, c: "#9be15d", seg: [16, 44, 60, 96] },
                  { y: 34, c: "#c084fc", seg: [8, 30, 52, 104] },
                  { y: 46, c: "#f0b429", seg: [26, 50, 78, 110] },
                ].map((r) => (
                  <g key={r.y}>
                    <line x1="2" x2="118" y1={r.y} y2={r.y} stroke={r.c} strokeOpacity="0.18" />
                    <line x1={r.seg[0]} x2={r.seg[1]} y1={r.y} y2={r.y} stroke={r.c} strokeOpacity="0.8" strokeWidth="2" />
                    <line x1={r.seg[2]} x2={r.seg[3]} y1={r.y} y2={r.y} stroke={r.c} strokeOpacity="0.5" strokeWidth="2" strokeDasharray="2 3" />
                  </g>
                ))}
                <rect x="46" y="2" width="34" height="60" fill="rgba(94,231,255,0.07)" stroke="#5ee7ff" strokeWidth="0.8" />
              </svg>
            </div>
            <p className="dl-microbody mono" style={{ marginTop: 6 }}>
              Focus: pull_request hunk · 100%
            </p>
          </div>
        </aside>
        <aside className="dl-pane">
          <h3>
            STORY / STEP {selectedStep.n} {selectedStep.label} <span>{stepIndex + 1} of 7</span>
          </h3>
          <div className="dl-scroll">
            {stepIndex !== 3 ? (
              <div className="dl-fbox">
                <div className="fk">SELECTED SOURCED EPISODE</div>
                <p className="dl-microbody">{selectedStep.n} {selectedStep.label} · {selectedStep.time}</p>
                <p className="dl-hint">Detailed source content for this episode is unavailable in the bounded synthetic reconstruction.</p>
              </div>
            ) : null}
            <div style={{ display: stepIndex === 3 ? undefined : "none" }}>
            <dl className="dl-qa">
              <dt>
                ▸ What was the agent trying to do? <span className="src-chip">RECORDED TRANSCRIPT · 13:15</span>
              </dt>
              <dd>
                Add a base-unavailable guard for probe-head if profiling data for the probed output and run a head-only
                flow when base artifacts are unavailable.
              </dd>
              <dt>
                ▸ What did it observe? <span className="src-chip">TASK · 13:08</span>
              </dt>
              <dd>
                The probe-head job exposes outputs.available. When base artifacts are missing, compare cannot run; we
                should still publish head evidence.
              </dd>
              <dt>
                ▸ Explicit decision <span className="src-chip">PR BODY · 13:10</span>
              </dt>
              <dd>
                Condition profile-head on probe-head.available == true. Add base-unavailable guard to publish head-only
                artifact and skip base-dependent jobs.
              </dd>
              <dt>
                ▸ What happened next <span className="src-chip">COMMIT b7f91a2</span>
              </dt>
              <dd>Updated workflow: gated profile-head, added base-unavailable job, adjusted compare/comment dependencies.</dd>
              <dt>
                ▸ Why this matters <span className="src-chip">REPOSITORY FACT · INFERRED</span>
              </dt>
              <dd>
                Compiler cache provides immediate feedback to contributors even when base data cannot be retrieved,
                improving insight latency without blocking merges.
              </dd>
            </dl>
            <div className="dl-kicker">CAUSAL STRAND</div>
            <div className="dl-chiprow">
              {[
                ["Objective", "08:31"],
                ["Constraint", "09:21"],
                ["Decision", "10:36"],
                ["Implementation", "13:15"],
                ["Tests", "15:02"],
              ].map(([l, t], i) => (
                <span className={i === 3 ? "dl-chip is-on" : "dl-chip"} key={l}>
                  {l} {t}
                </span>
              ))}
            </div>
            <div className="dl-kicker" style={{ marginTop: 8 }}>
              AGENT ACTIVITIES
            </div>
            {[
              { label: "Primary agent", color: "#5ee7ff" },
              { label: "Subagent: probe", color: "#9be15d" },
              { label: "Subagent: workflow", color: "#c084fc" },
            ].map((l) => (
              <div className="dl-ministrip" key={l.label}>
                <span>{l.label}</span>
                <span className="dots">
                  {Array.from({ length: 7 }, (_, i) => (
                    <i key={i} style={{ background: l.color, opacity: 0.4 + (i % 3) * 0.25 }} />
                  ))}
                </span>
              </div>
            ))}
            <div className="dl-fbox" style={{ marginTop: 6 }}>
              <div className="fr">
                <span className="fl amber">Feedback (acted on) · 16:48</span>
              </div>
            </div>
            </div>
          </div>
        </aside>
        <section className="dl-pane">
          <div className="dl-tabs">
            <ReviewTabs mode={mode} onMode={setMode} />
            <span className="grow" />
            <span className="mono dim">Split: 55 / 45 ————●——</span>
          </div>
          {mode !== "code" ? (
            <div className="dl-scroll" role="tabpanel" aria-label={`${mode} review panel`} style={{ padding: 10 }}>
              <div className="dl-fbox">
                <div className="fk">{mode.toUpperCase()} · STEP {selectedStep.n} {selectedStep.label}</div>
                {mode === "story" ? (
                  <p className="dl-microbody">Selected sourced episode at {selectedStep.time}. Its recorded artifacts remain visible in the Story rail.</p>
                ) : mode === "evidence" ? (
                  <p className="dl-microbody">Exact transcript, PR body, commit, workflow run, and artifact anchors are available in the Source &amp; Evidence rail below.</p>
                ) : (
                  <LocalFeedbackPanel feedback={feedback} history={feedbackHistory} onFeedback={setFeedback} />
                )}
              </div>
            </div>
          ) : null}
          <div className="dl-codepath" style={{ display: mode === "code" ? undefined : "none" }}>
            🗎 .github/workflows/compiler-cache.yml ⧉
            <span style={{ float: "right", color: "var(--ink-muted)" }}>TEMPORAL SEMANTIC WORKFLOW (STEP 04 · DECISION SELECTED)</span>
          </div>
          <div className="dl-dagrow" style={{ display: mode === "code" ? undefined : "none" }}>
            <div className="col">
              <div className="k">BEFORE</div>
              <div className="chips">
                <WfChip label="probe-head" state="on" />
                <i className="arrow">→</i>
                <WfChip label="profile-head" />
                <i className="arrow">→</i>
                <WfChip label="probe-base" />
                <i className="arrow">→</i>
                <WfChip label="compare" />
                <i className="arrow">→</i>
                <WfChip label="comment" />
              </div>
            </div>
            <div className="col">
              <div className="k">AFTER</div>
              <div className="chips">
                <WfChip label="probe-head" state="on" />
                <i className="arrow">→</i>
                <WfChip label="profile-head" state="on" />
                <i className="arrow">→</i>
                <WfChip label="probe-base ✕" state="guard" />
                <i className="arrow dim">→</i>
                <WfChip label="compare" state="dead" />
                <i className="arrow dim">→</i>
                <WfChip label="comment" state="dead" />
              </div>
              <div className="chips" style={{ marginTop: 4 }}>
                <WfChip label="base-unavailable" state="new" />
                <i className="arrow">↗</i>
                <span className="mono" style={{ fontSize: 8, color: "var(--state-ready)", alignSelf: "center" }}>
                  head-only publish path
                </span>
              </div>
            </div>
          </div>
          <div className="dl-daglegend mono" style={{ display: mode === "code" ? undefined : "none" }}>
            <span>
              <i className="ex" /> Executed
            </span>
            <span>
              <i className="gd" /> Guarded / Skipped
            </span>
            <span>
              <i className="nw" /> New / Modified
            </span>
          </div>
          <div className="dl-diff2col" style={{ display: mode === "code" ? undefined : "none" }}>
            <div className="col">
              <div className="ph mono">
                BEFORE (STEP 04 · as submitted) <em className="del">−6</em>
              </div>
              <div className="dl-diff">
                <DiffLine kind="hunk" text="@@ -12,16 @@ on:" />
                <DiffLine a="12" kind="ctx" text="  pull_request:" />
                <DiffLine a="13" kind="ctx" text="    types: [opened, synchronize, reopened]" />
                <DiffLine a="14" kind="ctx" text="  concurrency:" />
                <DiffLine a="15" kind="ctx" text="    group: compiler-${{ github.event.pull_request.number }}" />
                <DiffLine a="16" kind="ctx" text="    cancel-in-progress: true" />
                <DiffLine a="17" kind="ctx" text="  jobs:" />
                <DiffLine a="18" kind="ctx" text="    probe-head:" />
                <DiffLine a="19" kind="ctx" text="      runs-on: ubuntu-latest" />
                <DiffLine a="20" kind="ctx" text="      outputs:" />
                <DiffLine a="21" kind="ctx" text="        available: ${{ steps.check.outputs.available }}" />
                <DiffLine a="22" kind="del" text="-   profile-head:" />
                <DiffLine a="23" kind="del" text="-     needs: probe-head" />
                <DiffLine a="24" kind="del" text="-     runs-on: ubuntu-latest" />
                <DiffLine a="25" kind="del" text="-     steps:" />
                <DiffLine a="26" kind="ctx" text="        - uses: actions/checkout@v4" />
              </div>
            </div>
            <div className="col">
              <div className="ph mono">
                AFTER (HEAD · b7f91a2) <em className="add">+10</em>
              </div>
              <div className="dl-diff">
                <DiffLine kind="hunk" text="@@ +22,26 @@ jobs:" />
                <DiffLine b="22" kind="ctx" text="    profile-head:" />
                <DiffLine b="33" kind="add" text="+     needs: probe-head" />
                <DiffLine b="34" kind="hi" text="+     if: needs.probe-head.outputs.available == 'true'" />
                <DiffLine b="35" kind="add" text="+     runs-on: ubuntu-latest" />
                <DiffLine b="36" kind="ctx" text="      steps:" />
                <DiffLine b="37" kind="ctx" text="        - uses: actions/checkout@v4" />
                <DiffLine b="41" kind="add" text="+   base-unavailable:" />
                <DiffLine b="42" kind="add" text="+     if: needs.probe-head.outputs.available != 'true'" />
                <DiffLine b="43" kind="add" text="+     runs-on: ubuntu-latest" />
                <DiffLine b="44" kind="add" text="+     steps:" />
                <DiffLine b="45" kind="add" text="+       - name: Publish head-only evidence" />
              </div>
            </div>
          </div>
          <div className="dl-feedbackrow" style={{ display: mode === "code" || mode === "feedback" ? undefined : "none" }}>
            <div className="callout">
              <b>TraceDecay (local) · Just now</b>
              <p>Does base-unavailable still publish head evidence?</p>
            </div>
            <div className="callout local">
              <b>LOCAL TRACEDECAY FEEDBACK · provider write unavailable/read-only</b>
              <p className="mono">This comment 16:48 —— Action taken 16:50 —— ✔ head-only artifact confirmed 17:12</p>
            </div>
          </div>
          <div className="dl-actionrow">
            <ChipButton onClick={() => { setMode("feedback"); beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "comment", body: "", anchor: ".github/workflows/compiler-cache.yml:34", revision: "b7f91a2", lifecycle: "open" }, setFeedback, setFeedbackHistory); }}>🗨 Comment on hunk</ChipButton>
            <ChipButton onClick={() => { setMode("feedback"); beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "challenge", body: "", anchor: "episode:04/decision", revision: "b7f91a2", lifecycle: "open" }, setFeedback, setFeedbackHistory); }}>⚠ Challenge decision</ChipButton>
            <ChipButton onClick={() => { setMode("feedback"); beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "comment", body: "", anchor: `episode:${selectedStep.n}`, revision: "b7f91a2", lifecycle: "open" }, setFeedback, setFeedbackHistory); }}>⧉ Attach to episode</ChipButton>
            <span className="grow" />
            <button type="button" className="dl-att tone-ready" aria-pressed={reviewMark === "Understood"} onClick={() => setReviewMark("Understood")}>✓ UNDERSTOOD</button>
            <button type="button" className="dl-att tone-amber" aria-pressed={reviewMark === "Risky"} onClick={() => setReviewMark("Risky")}>⚠ RISKY</button>
            <button type="button" className="dl-att tone-danger" aria-pressed={reviewMark === "Needs clarification"} onClick={() => setReviewMark("Needs clarification")}>⊙ NEEDS CLARIFICATION</button>
          </div>
        </section>
      </div>
      <div className="dl-evidencestrip">
        <div className="k">SOURCE &amp; EVIDENCE (RESIZABLE) — EXACT ANCHORS <span className="mono dim">Drag to resize</span></div>
        <div className="cards">
          {[
            {
              k: "🗎 RECORDED TRANSCRIPT",
              t: "13:15",
              grade: "EXPLICIT",
              body: "Agent: Add compiler cache guard for this PR's head if profiling data is available. Gate on the probed output and run a head-only flow.",
              anchor: "transcript/13:15",
            },
            {
              k: "🗎 PR BODY EXCERPT",
              t: "13:10",
              grade: "EXPLICIT",
              body: "When base artifacts are missing, we should still publish head-only evidence so reviewers get insight without blocking.",
              anchor: "pr_body/L22-L29",
            },
            {
              k: "⧉ COMMIT",
              t: "13:42",
              grade: "EXACT",
              body: "b7f91a2 · feat(workflow): gate profile-head on probe availability; add base-unavailable job for head-only publishing",
              anchor: "b7f91a2",
            },
            {
              k: "⚙ WORKFLOW RUN (HEAD)",
              t: "16:15",
              grade: "EXACT",
              body: "profile-head: success (38s) · base-unavailable: success (21s) · compare: skipped (guarded)",
              anchor: "run/9b3c5a1e2d44",
            },
            {
              k: "🗃 ARTIFACT EVIDENCE",
              t: "17:12",
              grade: "EXACT",
              body: "head-only artifact confirmed — compiler-cache-profile-head.json (24.3 KB) uploaded by base-unavailable job",
              anchor: "artifact/9b3c5a1e2d44",
            },
          ].map((c) => (
            <div className="card" key={c.k}>
              <div className="h mono">
                {c.k} <em>{c.t}</em> <GradeMark grade={c.grade} />
              </div>
              <p>{c.body}</p>
              <div className="a mono">Anchor: {c.anchor}</div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

/* ---- State 10: PR #743 Decision to Code ---- */

const PR743_FINDINGS_V2 = [
  {
    id: "P1",
    tag: "TRUST BOUNDARY",
    title: "Comment target comes from attacker-controlled artifact.",
    body: "The privileged comment workflow reads a PR number from an artifact the PR itself produced.",
  },
  {
    id: "P1",
    tag: "INVALID COMPARISON",
    title: "Unequal sealed-state digests warn but still permit comparison.",
    body: "Comparison remains enabled after the warning path.",
  },
  {
    id: "P2",
    tag: "PIPE FAILURE",
    title: "Missing pipefail can hide timing failure through tee.",
    body: "A broken pipe can hide timing failure through tee.",
  },
];

const PR743_REVISION = "39e4704e0f8c24811f851eecc693cd514401e858";

export function DecisionToCode() {
  const { navigate } = useDemo();
  const [stepIndex, setStepIndex] = useWorkspaceState("delivery:10:step", 4);
  const [mode, setMode] = useWorkspaceState<ReviewMode>("delivery:10:mode", "code");
  const [showJourney, setShowJourney] = useWorkspaceState("delivery:10:journey", true);
  const [showEvidence, setShowEvidence] = useWorkspaceState("delivery:10:evidence", true);
  const [reviewMark, setReviewMark] = useWorkspaceState("delivery:10:mark", "Needs clarification");
  const { feedback, setFeedback, feedbackHistory, setFeedbackHistory } = useLocalFeedbackState("10");
  const previous = () => setStepIndex((index) => Math.max(0, index - 1));
  const next = () => setStepIndex((index) => Math.min(PR743_STEPS.length - 1, index + 1));
  const focusCode = () => { setShowJourney(false); setShowEvidence(false); setMode("code"); };
  const selectedStep = PR743_STEPS[stepIndex];
  useReviewKeys({ previous, next, focus: focusCode, journey: () => setShowJourney((shown) => !shown), evidence: () => setShowEvidence((shown) => !shown) });
  return (
    <div className="dl-stage is-story">
      <div className="dl-storyhead">
        <span className="crumb mono">Delivery / Hotpath CI / PR #743 / Decision to Code</span>
        <span className="mono dim">PR #743 · MERGED · 39e4704e0f8c…e858 · ci/hotpath-pr-trigger → master · 2 files · +124 / −17</span>
        <span className="grow" />
        <span className="dl-att tone-violet">PROVIDER MODE · READ ONLY</span>
      </div>
      <div className="dl-storybar" aria-label="PR 743 eight-step review">
        {PR743_STEPS.map((s, index) => (
          <button
            type="button"
            key={`${s.n}${s.label}`}
            className={index === stepIndex ? "st is-active" : index < stepIndex ? "st is-done" : "st"}
            aria-pressed={index === stepIndex}
            onClick={() => setStepIndex(index)}
            style={{ appearance: "none", backgroundColor: index === stepIndex ? "color-mix(in oklab, var(--signal-cyan) 10%, #070b12)" : "#070b12", color: "inherit", textAlign: "left", cursor: "pointer" }}
          >
            <span className="n">{index < stepIndex ? "✓" : s.n}</span>
            <span className="l">
              {s.n} {s.label}
            </span>
            <span className="t">{s.time}</span>
          </button>
        ))}
      </div>
      <div className="dl-split" style={{ gridTemplateColumns: `${showJourney ? "280px" : "0"} minmax(0, 1fr) ${showEvidence ? "260px" : "0"}` }}>
        <aside className="dl-pane" style={{ display: showJourney ? undefined : "none" }}>
          <h3>
            RECOVERED PRODUCER JOURNEY <span>STEP {stepIndex + 1} / 8</span>
          </h3>
          <div className="dl-scroll">
            <div className="dl-fbox" style={{ borderColor: "color-mix(in oklab, var(--state-violet) 40%, var(--edge-subtle))" }}>
              <div className="fk">PERSISTED TRANSCRIPT, NOT PRIVATE REASONING</div>
              <p className="dl-microbody">
                Root session caf0eec6-9fd3-4927-9e33-454e6b627137 maps to hosted link session_01CPdGeG8tU25R3QSh5dE7XW.
                Hidden chain-of-thought remains unavailable.
              </p>
            </div>
            <div className="dl-timeline">
              {PR743_TRANSCRIPT.map((row) => (
                <article key={row.t} className="dl-pr" data-grade={row.grade}>
                  <span className="id">{row.t}</span>
                  <span className="repo">{row.who}</span>
                  <div className="title">{row.text}</div>
                  <div className="meta">
                    <GradeMark grade={row.grade} />
                  </div>
                </article>
              ))}
            </div>
            <div className="dl-fbox" style={{ marginTop: 8 }}>
              <div className="fr">
                <span className="fl">
                  <i className="mark" style={{ background: "var(--signal-cyan)" }} />
                  Daemon-free benchmark agent
                </span>
                <b style={{ color: "var(--ink-muted)", fontWeight: 400 }}>prerequisite work</b>
              </div>
              <div className="fr">
                <span className="fl">
                  <i className="mark" style={{ background: "var(--state-violet)" }} />
                  Nested API researcher
                </span>
                <b style={{ color: "var(--ink-muted)", fontWeight: 400 }}>source evidence only</b>
              </div>
              <p className="dl-microbody" style={{ marginTop: 4 }}>
                No Codex producer attribution. Subagents did not author PR #743's two workflow hunks.
              </p>
            </div>
          </div>
        </aside>
        <section className="dl-pane" style={{ gridColumn: "2" }}>
          <div className="dl-tabs">
            <ReviewTabs mode={mode} onMode={setMode} />
            <span className="grow" />
            <ChipButton onClick={() => setShowJourney((shown) => !shown)} pressed={showJourney}>Journey</ChipButton>
            <ChipButton onClick={focusCode} pressed={!showJourney && !showEvidence}>Focus code</ChipButton>
            <ChipButton onClick={() => setShowEvidence((shown) => !shown)} pressed={showEvidence}>Evidence</ChipButton>
            <ChipButton onClick={previous} disabled={stepIndex === 0}>‹ Previous</ChipButton>
            <ChipButton onClick={next} disabled={stepIndex === PR743_STEPS.length - 1}>Next ›</ChipButton>
          </div>
          {mode !== "code" ? (
            <div className="dl-scroll" role="tabpanel" aria-label={`${mode} review panel`} style={{ padding: 10 }}>
              <div className="dl-fbox">
                <div className="fk">{mode.toUpperCase()} · STEP {selectedStep.n} {selectedStep.label}</div>
                {mode === "story" ? (
                  <p className="dl-microbody"><GradeMark grade={PR743_TRANSCRIPT[Math.min(stepIndex, PR743_TRANSCRIPT.length - 1)].grade} /> {PR743_TRANSCRIPT[Math.min(stepIndex, PR743_TRANSCRIPT.length - 1)].text}</p>
                ) : mode === "evidence" ? (
                  <p className="dl-microbody">Observed profile path: head_bench_available=false; no timing or workload JSON. The exact run evidence remains in the evidence rail.</p>
                ) : (
                  <LocalFeedbackPanel feedback={feedback} history={feedbackHistory} onFeedback={setFeedback} />
                )}
              </div>
            </div>
          ) : null}
          <div className="dl-codepath" style={{ display: mode === "code" ? undefined : "none" }}>CODE &amp; IMPACT · EXACT REPOSITORY DIFF <span style={{ float: "right", color: "var(--ink-muted)" }}>2 FILES · +124 / −17</span></div>
          <div className="dl-diff2col" style={{ display: mode === "code" ? undefined : "none" }}>
            <div className="col">
              <div className="ph mono">
                🗎 .github/workflows/hotpath-profile.yml <em className="add">+96</em> <em className="del">−10</em>
              </div>
              <div className="dl-diff">
                <DiffLine kind="hunk" text="@@ on: / concurrency @@" />
                <DiffLine a="2" b="2" kind="hi" text="  # Manually triggered while the workload is the bounded MCP smoke…" />
                <DiffLine a="18" b="18" kind="add" text="+ pull_request:" />
                <DiffLine a="" b="19" kind="add" text="+ concurrency:" />
                <DiffLine a="" b="20" kind="add" text="+   group: hotpath-profile-${{ github.event.pull_request.number }}" />
                <DiffLine a="" b="21" kind="add" text="+   cancel-in-progress: true" />
                <DiffLine a="24" b="24" kind="add" text="+ INDEX_BENCH_SOURCE: crates/tracedecay-query/src/bin/tracedecay…" />
                <DiffLine a="70" b="70" kind="ctx" text="      - name: Detect benchmark at head" />
                <DiffLine a="73" b="73" kind="ctx" text="        run: cp -r benchmark_data/index-bench/corpus …" />
                <DiffLine a="76" b="76" kind="del" text="-     if: steps.head_bench.outputs.available == 'true'" />
                <DiffLine a="" b="76" kind="add" text="+     if: steps.comparable.outputs.available == 'true'" />
                <DiffLine a="77" b="77" kind="ctx" text="      - name: Profile head" />
                <DiffLine a="81" b="81" kind="ctx" text="        if: steps.comparable.outputs.available == 'true'" />
                <DiffLine a="88" b="88" kind="add" text="+     - name: Compare workload identity" />
                <DiffLine a="91" b="91" kind="ctx" text="      - name: Checkout base" />
                <DiffLine a="96" b="96" kind="add" text="+       if: steps.head_bench.outputs.available == 'true'" />
                <DiffLine a="127" b="127" kind="ctx" text="      - name: Compare workload identity" beacon="UNEXERCISED · DIGEST WARNING" />
              </div>
            </div>
            <div className="col">
              <div className="ph mono">
                🗎 .github/workflows/hotpath-comment.yml <em className="add">+28</em> <em className="del">−7</em>
              </div>
              <div className="dl-diff">
                <DiffLine kind="hunk" text="@@ comparable profile gate @@" />
                <DiffLine a="23" b="23" kind="ctx" text="      - name: Check for a comparable profile" />
                <DiffLine a="24" b="24" kind="ctx" text="        run: |" />
                <DiffLine a="25" b="25" kind="add" text="+         pr=$(cat /tmp/metrics/pr_number.txt || true)" beacon="TRUST BOUNDARY" />
                <DiffLine a="26" b="26" kind="add" text='+         if [ -z "$pr" ] || [ "$pr" = "null" ]; then' />
                <DiffLine a="27" b="27" kind="add" text='+           echo "available=false" >> "$GITHUB_OUTPUT"' />
                <DiffLine a="28" b="28" kind="ctx" text="          elif [ ! -s /tmp/metrics/head_timing.json ] …" />
                <DiffLine a="29" b="29" kind="add" text="+           echo … /tmp/metrics/base_timing.json ]; then" />
                <DiffLine a="30" b="30" kind="add" text='+           echo "available=true" >> "$GITHUB_OUTPUT"' />
                <DiffLine a="55" b="55" kind="ctx" text="      - name: Head profile (timing)" />
                <DiffLine a="60" b="60" kind="hi" text="        if: steps.comparable.outputs.available == 'true'" beacon="UNEXERCISED" />
                <DiffLine a="64" b="64" kind="ctx" text="        run: cp … --benchmark-id index-bench-timing" />
                <DiffLine a="66" b="66" kind="del" text="-       --benchmark-id index-bench-timing" />
                <DiffLine a="127" b="127" kind="ctx" text="      - name: Compare workload identity" />
              </div>
            </div>
          </div>
          <div className="dl-wfrows" style={{ display: mode === "code" ? undefined : "none" }}>
            <div className="wf">
              <span className="k">PROFILE WORKFLOW · DEPENDENCY + GUARDS</span>
              {[
                ["checkout head", ""],
                ["detect head", ""],
                ["head?", "guard"],
                ["pin corpus", "dead"],
                ["profile head", "dead"],
                ["checkout base", "dead"],
                ["base?", "dead"],
                ["profile base", "dead"],
                ["digest compare only warns", "guard"],
              ].map(([label, st], i) => (
                <span key={label} style={{ display: "contents" }}>
                  {i > 0 ? <i className="arrow">→</i> : null}
                  <span className={st === "guard" ? "dl-wfchip is-guard" : st === "dead" ? "dl-wfchip is-dead" : "dl-wfchip"}>{label}</span>
                </span>
              ))}
            </div>
            <div className="wf">
              <span className="k">COMMENT WORKFLOW · COMPARABLE PAIR GATE</span>
              {[
                ["workflow_run", ""],
                ["download artifact", ""],
                ["comparable?", "guard"],
                ["install tooling", "dead"],
                ["post comment", "dead"],
                ["old default workflow failed before guard", "dead"],
              ].map(([label, st], i) => (
                <span key={label} style={{ display: "contents" }}>
                  {i > 0 ? <i className={st === "dead" ? "arrow dim" : "arrow"}>→</i> : null}
                  <span className={st === "guard" ? "dl-wfchip is-guard" : st === "dead" ? "dl-wfchip is-dead" : "dl-wfchip"}>{label}</span>
                </span>
              ))}
            </div>
          </div>
          <div className="dl-actionrow">
            <ChipButton onClick={() => { setMode("feedback"); beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "comment", body: "", anchor: ".github/workflows/hotpath-profile.yml:2", revision: PR743_REVISION, lifecycle: "open" }, setFeedback, setFeedbackHistory); }}>Comment on selected hunk</ChipButton>
            <ChipButton onClick={() => { setMode("feedback"); beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "challenge", body: "", anchor: "episode:05/decision-to-code", revision: PR743_REVISION, lifecycle: "open" }, setFeedback, setFeedbackHistory); }}>Challenge visible decision</ChipButton>
            <ChipButton onClick={() => { setMode("feedback"); beginLocalFeedback(feedback, { id: crypto.randomUUID(), kind: "comment", body: "", anchor: `episode:${selectedStep.n}`, revision: PR743_REVISION, lifecycle: "open" }, setFeedback, setFeedbackHistory); }}>Attach to episode</ChipButton>
            <ChipButton onClick={() => setReviewMark("Understood")} pressed={reviewMark === "Understood"}>Mark understood</ChipButton>
            <button type="button" className="dl-att tone-danger" aria-pressed={reviewMark === "Needs clarification"} onClick={() => setReviewMark("Needs clarification")}>Needs clarification</button>
            <ChipButton title="Transcript absent from the loaded Sessions export" onClick={() => navigate("sessions", { session: "caf0eec6-9fd3-4927-9e33-454e6b627137" })}>Locate retained session</ChipButton>
            <span className="mono dim">transcript unavailable in loaded Sessions export</span>
            <ChipButton onClick={() => { setMode("evidence"); setShowEvidence(true); }}>Evidence table</ChipButton>
          </div>
        </section>
        <aside className="dl-pane" style={{ display: showEvidence ? undefined : "none", gridColumn: "3" }}>
          <h3>
            RUN EVIDENCE + REVIEW <span className="dl-att tone-danger">3 UNRESOLVED</span>
          </h3>
          <div className="dl-scroll">
            <div className="dl-fbox">
              <div className="fk">WHAT THE CHANGE WAS TRYING TO DO</div>
              <p className="dl-microbody">
                Run Hotpath on pull requests without breaking heads or bases that do not contain the benchmark.
              </p>
            </div>
            <div className="dl-fbox">
              <div className="fk">DECISION RETAINED</div>
              <p className="dl-microbody">
                Decision retained in transcript + PR/commit: probe head and base independently; emit head-only evidence
                when base is absent; compare only when both exist.
              </p>
            </div>
            {PR743_FINDINGS_V2.map((f) => (
              <div className="dl-find" key={f.tag}>
                <div className="tag">
                  {f.id} · {f.tag}
                </div>
                <h4>{f.title}</h4>
                <p>{f.body}</p>
              </div>
            ))}
            <div className="dl-fbox">
              <div className="fk">OBSERVED OUTCOMES</div>
              {[
                "15 ordinary CI checks passed",
                "Provider check matrix.",
                "Profile path was not exercised — head_bench_available=false; base blank; no timing or workload JSON.",
                "Privileged comment workflow failed pre-merge — workflow_run loaded the old default-branch workflow; missing base_timing.json.",
                "Post-merge guard skipped tooling/comment — no real head/base comparison exists yet.",
              ].map((x) => (
                <div className="fr" key={x}>
                  <span className="fl">{x}</span>
                </div>
              ))}
            </div>
            <div className="dl-fbox">
              <div className="fk">MIRRORED TO #707</div>
              <div className="fr">
                <span className="fl mono">Commit 49fcb0e6 at 21:42:58Z</span>
                <GradeMark grade="EXACT" />
              </div>
            </div>
            <div className="dl-fbox">
              <div className="fk">PRIVATE REASONING</div>
              <div className="fr">
                <span className="fl">Unavailable</span>
                <b>
                  <HonestMark id="unavailable" />
                </b>
              </div>
            </div>
            <p className="dl-hint">
              Only persisted user messages, assistant summaries, subagent reports, PR/commit rationale, git evidence,
              checks, and findings are shown. Review incomplete: 3 unresolved · no full comparison run.
            </p>
          </div>
        </aside>
      </div>
    </div>
  );
}
