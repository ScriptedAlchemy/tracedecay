//! Git identity and isolation shared by every in-crate fixture that shells
//! out to `git`.

#[cfg(test)]
use std::path::Path;

#[cfg(test)]
use tracedecay_runtime_core::path_safety::{plain_git_args, plain_host_path};

/// The `-c` overrides every fixture git invocation carries, so the operator's
/// global git configuration, hooks, gc, identity, commit signing, cannot
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

/// Runs the resolved `git` executable in `root` under [`GIT_FIXTURE_CONFIG`],
/// asserts it succeeded, and returns its trimmed stdout. Paths are spelled
/// plainly so a verbatim Windows root reaches `git` in a form it accepts.
#[cfg(test)]
pub(crate) fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(plain_host_path(root))
    .args(GIT_FIXTURE_CONFIG)
    .args(plain_git_args(args))
    .output()
    .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout is UTF-8")
        .trim()
        .to_owned()
}
