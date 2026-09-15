//! Git identity and isolation shared by every in-crate fixture that shells
//! out to `git`.

/// The `-c` overrides every fixture git invocation carries, so the operator's
/// global git configuration — hooks, gc, identity, commit signing — cannot
/// reach a fixture commit, and a CI runner that has no `user.identity` at all
/// can still commit.
pub const GIT_FIXTURE_CONFIG: [&str; 10] = [
    "-c",
    "core.hooksPath=.git/no-hooks",
    "-c",
    "gc.auto=0",
    "-c",
    "user.name=TraceDecay Test",
    "-c",
    "user.email=tracedecay-test@example.com",
    "-c",
    "commit.gpgsign=false",
];
