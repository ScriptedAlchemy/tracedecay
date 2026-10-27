# Other branches

Read-only branch exploration for the `tracedecay:exploring-code` skill.

| Question | Call |
| --- | --- |
| Which local branch refs exist | `tracedecay_branch_list` |
| Find a symbol on a local branch | `tracedecay_branch_search` with `branch` and `query` |
| Compare two indexed local branch tips | `tracedecay_branch_diff` with required `base`; `head` defaults to the current branch |

Branch reads do not switch the checkout. Search and diff require immutable
code-index generations sealed for the branches' exact current commits. Report
an unavailable generation as unavailable; results do not fall back to an
ancestor or the active checkout. Keep `next_cursor` with the original query
and filters when another bounded page is needed.

Use `branch_diff`'s `file` or `kind` filter when the question is already scoped.
For PR orientation or a committed ref range, start with `tracedecay_pr_context`:
its local-code briefing compares exact Git blobs even when indexed branch
generations are unavailable. Its `base_ref` and `head_ref` also accept
remote-tracking refs; the branch tools above select local branch names.

Read a committed source span with `source_lines` using the full commit
`revision`, repository-relative `file`, and one-based inclusive
`start_line`/`end_line`. Continue from `next_line` with the original revision,
file, and end line. This follows the normal admitted source-service lifecycle
and path policy; unavailable primitive authority is still unavailable.
The PR briefing reads retained hosted evidence by default; request
`refresh_github: true` only when a configured matching PR refresh is useful.
