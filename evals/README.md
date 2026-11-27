# Evaluations

TraceDecay keeps its evaluations here. Each group has its own runner:

- `memory/` contains deterministic memory scenarios and the cost-gated
  real-model runner shared with the Rust memory suite.
- `hermetic/` contains isolated Claude and Codex lifecycle runners, corpora,
  fixtures, scorers, and smoke commands.
- `agent_adoption/` contains the neutral-prompt adoption and ablation matrix,
  including its dry-run and self-test tooling.

Each subdirectory documents its own prerequisites and runnable commands.
