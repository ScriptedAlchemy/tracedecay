# Bazel CI on the private fleet

GitHub-hosted runners remain the workflow runners. Linux x86_64 test shards
and Clippy, plus repository gates and the shipped CLI build, connect to the
private NativeLink coordinator through the official Tailscale action. GitHub
supplies checkout, analysis, local compilation, test execution, artifacts, and
check reporting; Ubuntu-main and
Omarchy supply additional Linux compilation and test capacity. The Mac worker
serves macOS actions; this CI integration does not send Linux actions to it. Windows, ARM Linux,
and macOS CI retain their existing hosted execution. The Mac currently has a
newer Xcode/SDK than the macos-26 image; matching OS and CPU alone does not make
native compilation compatible.

Rustc and TestRunner actions use Bazel dynamic execution with sandboxed local
execution and remote execution. Both may start the same action; Bazel keeps the
first result and cancels the other. Local work respects the runner CPU count
and half its RAM. The shared action cache remains enabled. Tests carry Node, Rust Analyzer,
and their compiler tools as declared runfiles.
Each remote test reserves four worker CPU units and the Rust harness uses four
threads. The local branch reserves the hosted runner CPU budget so suites do not
compete with one another for local cores. Remote infrastructure failures allow
local fallback; an actual test failure still fails the test. BuildBuddy events
upload asynchronously, so a reporting outage does not fail a successful build.
A terminated runner can leave an incomplete report. Fleet jobs skip restoring
and saving the multi-GB Actions disk cache; other jobs retain it.

## Trust configuration

`TS_BAZEL_CLIENT_ID` is a repository variable, not a secret. Its Tailscale OIDC
identity must have only Auth Keys write scope, restricted to `tag:bazel-ci`:

- Issuer: `https://token.actions.githubusercontent.com`
- Subject: `repo:ScriptedAlchemy/tracedecay:*`
- Custom claim `repository_id`: `1261602522`
- Custom claim `actor_id`: `25274700` (ScriptedAlchemy)
- Custom claim `workflow_ref`: `ScriptedAlchemy/tracedecay/.github/workflows/ci.yml@*`
- Audience: `api.tailscale.com/<client-id>`

Tailscale checks these signed claims independently of the workflow condition.
Only the current repository owner is enrolled. Adding another maintainer
requires an explicit trust change. The workflow also excludes fork PRs.
Keep GitHub's fork contributor approval policy at `all_external_contributors`;
approved outside-contributor runs use GitHub-hosted execution without fleet
credentials. Approving a fork run does not enroll its author in the fleet.
Do not enable write tokens for fork PRs.

The CI tag may reach only Ubuntu-main TCP 50051 (cache/execution), 1985
(BuildBuddy events), and 443 (build reports). It receives no SSH or worker
registration access. The coordinator host firewall must allow Tailscale
clients on 50051 while keeping 50061 restricted to the three fixed workers.
Tailscale grants enforce the tag restriction; host iptables cannot match tags.
The action registers ephemeral nodes and logs them out during job cleanup.

## Verification

An authenticated hosted run must show Tailscale OIDC enrollment, remote cache
or execution activity in Bazel's action summary, and the BuildBuddy invocation
URL. A cache hit alone does not prove remote execution. Cold dynamic builds may
show both sandboxed and remote actions; warm builds may use the cache entirely.
Actionlint checks workflow syntax but does not establish network or OIDC access.
