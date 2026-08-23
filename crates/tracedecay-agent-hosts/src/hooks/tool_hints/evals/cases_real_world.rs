use super::super::*;
use super::harness::*;

pub(super) fn real_world_prompt_cases() -> Vec<HintEval> {
    vec![
        prompt_eval(
            "raw-codex-jsonl-transcripts",
            "look at raw codex jsonl transcript files if needed as well",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search", "tracedecay_lcm_grep"],
        ),
        prompt_eval(
            "hook-verbosity-adversarial-review",
            "analyze the hook usage and verbosity and repetition in transcripts with codex where we have hints displayed",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search", "tracedecay_lcm_grep"],
        ),
        prompt_eval(
            "repo-local-dev-skill-request",
            "add more skills to .codex for helping debug tracedecay and develop on it",
            None,
            &[],
        ),
        prompt_eval(
            "generic-non-code-chat-complaint",
            "hooks should be smarter when a chat is not inside a git repo; it should be generic like lcm or sessions, not code graph parts",
            None,
            &[],
        ),
        prompt_eval(
            "what-did-we-decide-before",
            "where did we decide how memory curation should work before?",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "informal-prior-session-recall",
            "remind me what we concluded about hook hints last time",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "branch-or-pr-status",
            "What branch this on or pr",
            None,
            &[],
        ),
        prompt_eval("merge-pr-number", "Merge 64", None, &[]),
        prompt_eval(
            "generic-browser-help",
            "how do I open a new browser tab?",
            None,
            &[],
        ),
        prompt_eval(
            "render-model-visible-hook-input",
            "write a parser renderer to render cases where you can see what model gets with extra input from hooks vs what user submitted",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "prior-automation-run",
            "what happened in the last memory curator automation run?",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "unexpected-test-file-on-branch",
            "my branch has a dashboard test file I didn't write and commits I didn't make — who committed this?",
            Some(HintCategory::UnexpectedChanges),
            &["tracedecay_sessions_for"],
        ),
        prompt_eval(
            "branch-amended-under-me",
            "the branch appears to have been rebased and someone amended my branch while I was working — figure out where it came from",
            Some(HintCategory::UnexpectedChanges),
            &["tracedecay_sessions_for", "tracedecay_message_search"],
        ),
        prompt_eval(
            "sibling-rsncc-repo",
            "look in the rsncc sibling repo and check the open PR status there",
            Some(HintCategory::ProjectContext),
            &["tracedecay_project_search"],
        ),
        prompt_eval("what-repo-is-this", "what repo is this?", None, &[]),
        prompt_eval(
            "github-pr-live-status",
            "babysit PR 319 and tell me whether checks are green",
            None,
            &[],
        ),
        prompt_eval(
            "direct-code-change-request",
            "change the button text to Save and run the narrow test",
            None,
            &[],
        ),
    ]
}

#[test]
fn real_world_prompt_eval_matrix() {
    let evals = real_world_prompt_cases();

    for eval in &evals {
        run_eval(eval);
    }
}
