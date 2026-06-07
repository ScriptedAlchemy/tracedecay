## Learned User Preferences

- User prefers fresh, tool-backed verification for setup/configuration work and often asks agents to prove changes actually work.
- User wants repo-native project tooling used for codebase review, planning, and durable decision capture when available.
- User prefers local checkout tooling over global installs during active development, so tool behavior reflects the current branch.
- For unshipped PR branch work, replace in-progress designs directly rather than adding compatibility shims for old branch-only behavior.
- When the user asks to remember preferences or decisions, persist concise durable facts using the project memory system when available.

## Workspace Guidance

- Keep persistent guidance general and durable; avoid recording transient branch state, temporary schema numbers, or moment-in-time tool status here.
- Store detailed implementation decisions in the project memory system or PR docs instead of expanding this file with narrow session notes.
