## Learned User Preferences
- User values fresh, tool-backed verification for setup and configuration work and often asks agents to confirm whether changes actually worked.
- User may ask to use TokenSave MCP or CLI tooling for codebase context; check available MCP descriptors and the `tokensave` CLI path before relying on native MCP availability.

## Learned Workspace Facts
- TokenSave is installed at `/home/zack/.cargo/bin/tokensave`, and this repo has project-local MCP config at `.cursor/mcp.json` pointing `tokensave serve --path /home/zack/projects/tokensave`.
- TokenSave is initialized for this repo under `.tokensave/`; `.tokensave/config.json` records `root_dir` as `/home/zack/projects/tokensave`.
- Global Cursor MCP config was cleared of TokenSave; the old Hermes TokenSave config lives at `/home/zack/hermes-agent/.cursor/mcp.json`.
- This workspace has an existing native Grafana and Prometheus setup for the self-hosted GitHub runners; runner container metrics come from cAdvisor in `/home/zack/github-runner/monitoring`.
- The Grafana runner dashboard uses the `github-runner-containers` dashboard slug and is backed by the Prometheus `runner-containers` scrape job.
- `tokensave` already has Rust memory primitives for decisions, code areas, and session recall backed by libSQL/FTS5.
- Holographic memory exploration found no local Hermes plugin source under `/home/zack/projects`; the current design direction favors wrapping `amari-holographic` over manually porting Python math.
