# Issue 753 delivery checkpoint

- `cc-1369` showed a ready activation receipt; the test had read the typed
  status envelope at the wrong JSON depth. All state/receipt assertions were
  corrected.
- `cc-1375` then reached the second activation but searched before its
  asynchronous configuration transition settled. The journey now waits for
  the exact expected receipt after second activation and rollback.
- `cc-1411` was terminated by a broker-daemon shutdown after 20 minutes
  without a test failure. The same proof is now running directly with nonce
  `semantic-753-proof-direct`.
- Latest fetched PR head was `5de159b26180eb3325b20cfe8df3ca15510ace42`;
  the worktree base was 33 commits behind it.
- Post-reboot direct proof reached an exact activated configuration but the
  model lifecycle was still `installed`; commit `31a005501` now waits for
  lifecycle `ready` before evaluation. Direct proof rerun
  `semantic-753-proof-lifecycle-ready` is in progress.
- The lifecycle-ready rerun proved the remaining production defect: native
  evaluation leaves lifecycle `installed`, and committing an already-cached
  semantic observation did not restore lifecycle `ready`. The cache commit now
  marks lifecycle ready after the generation pointer commits successfully.
- The first lifecycle-commit proof attempt hit the existing typed evaluation
  deadline under host variance before activation. The bounded test helper now
  retries one typed timeout just as it already retries one target-conflict;
  production budgets remain unchanged.
- Both bounded attempts still timed out in a two-worker test runtime. The
  production journey now uses four Tokio workers so the paged evaluator and
  daemon owners have representative runtime capacity; no production deadline
  or resource budget changed.
