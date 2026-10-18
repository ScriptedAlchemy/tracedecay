"""Shared questions and state renderers. Written after the dataset was frozen."""
import json

CATS = ["session_recall", "memory_store", "call_graph", "impact", "refactor_edit",
        "review_changes", "type_orientation", "project_context", "unexpected_changes"]

NOUL_INS = {
    "session_recall": "Does the user's latest message ask to look up, recover or reuse something from an earlier conversation, a previous session, or another agent's transcript?",
    "memory_store": "Does the user's latest message state or confirm a lasting preference, convention or fact that the assistant should remember and apply in future sessions?",
    "call_graph": "Does the user's latest message ask which functions call some code, what some code calls, or how execution flows through the code?",
    "impact": "Does the user's latest message ask what would break, or which code and tests would be affected, if something is changed?",
    "refactor_edit": "Does the user's latest message ask for the same mechanical edit to be applied in many places, such as a rename, moving code, or repeating a change across files?",
    "review_changes": "Does the user's latest message ask to review a diff, staged changes, recent commits or a pull request?",
    "type_orientation": "Does the user's latest message ask which types implement an interface, about a class hierarchy, where objects are constructed, where a field is read or written, or about duplicated logic?",
    "project_context": "Does the user's latest message ask about code in another repository or a sibling project?",
    "unexpected_changes": "Does the user's latest message ask about commits or file edits that neither the user nor the assistant made?",
}

def noul_questions(cats=CATS):
    return {c: {"type": "noul", "instructions": NOUL_INS[c]} for c in cats}

CHOICE_CRIT = {
    "none": "none of the below: ordinary coding, edits, running commands, questions or chat",
    "session_recall": "recover something from an earlier conversation, previous session or another agent's transcript",
    "memory_store": "a lasting preference, convention or fact to remember for future sessions",
    "call_graph": "who calls some code, what it calls, or how execution flows",
    "impact": "what breaks or which code and tests are affected by a change",
    "refactor_edit": "the same mechanical edit applied in many places: rename, move, repeat a change",
    "review_changes": "review a diff, staged changes, commits or a pull request",
    "type_orientation": "implementors, class hierarchy, construction sites, field reads/writes, duplicated logic",
    "project_context": "code in another repository or sibling project",
    "unexpected_changes": "commits or edits that neither the user nor the assistant made",
}
CHOICE_Q = {"intent": {"type": "choice",
                       "instructions": "Which kind of help does the user's latest message ask for?",
                       "criteria": CHOICE_CRIT}}

ROLE = {"user": "User", "assistant": "Assistant", "tool": "Tool call"}

def prompt_only(c):
    return "User's latest message: " + c["current_prompt"]

def text_window(c, turns=None, prompt_first=False):
    turns = c["turns"] if turns is None else turns
    if not turns:
        return prompt_only(c)
    hist = "\n".join("%s: %s" % (ROLE[t["role"]], t["text"]) for t in turns)
    if prompt_first:
        rev = "\n".join("%s: %s" % (ROLE[t["role"]], t["text"]) for t in reversed(turns))
        return "User's latest message: %s\n\nEarlier turns, most recent first:\n%s" % (c["current_prompt"], rev)
    return "Earlier in the conversation:\n%s\n\nUser's latest message: %s" % (hist, c["current_prompt"])

def json_window(c, turns=None):
    turns = c["turns"] if turns is None else turns
    return {"current_prompt": c["current_prompt"],
            "recent_turns": [{"role": t["role"], "text": t["text"]} for t in turns if t["role"] != "tool"],
            "recent_tools": [t["text"] for t in turns if t["role"] == "tool"]}

def load(path="dataset.json"):
    return json.load(open(path))
