# Other branches

Read-only branch exploration for the `tracedecay:exploring-code` skill.

- List tracked branches
- Search or compare another branch without switching checkout
- Branch-fallback WARNING handling

## Other branches

1. **What's tracked → `tracedecay_branch_list`**; **search another branch →
   `tracedecay_branch_search`** (`branch`, `query`); **compare branches →
   `tracedecay_branch_diff`** (`base?`, `head?`, `file?`, `kind?`) — all
   read-only, never touching your checkout.
2. Branch tracking is opt-in per branch (`tracedecay branch add <branch>` in
   the terminal; the hooks auto-track branches you visit). A branch-fallback
   `WARNING` prefix means results came from the nearest tracked ancestor —
   surface that to the user.
