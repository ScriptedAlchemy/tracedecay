# Bazel CI on the private fleet

Repository checks, Linux tests, Clippy, production feature validation, and the
shipped CLI build run independently. They share no generated artifacts between
jobs, so fleet work can start while repository checks run. Each check still
reports its own result before merge.

GitHub-hosted runners remain the workflow runners. Linux x86_64 test shards
and Clippy, plus repository gates and the shipped CLI build, connect to the
private NativeLink coordinator through the official Tailscale action. GitHub
supplies checkout, analysis, local compilation, test execution, artifacts, and
check reporting; Ubuntu-main and
Omarchy supply additional Linux compilation and test capacity. The Mac worker
serves macOS actions; this CI integration does not send Linux actions to it. ARM Linux and macOS CI retain their existing hosted execution. The Mac currently has a
newer Xcode/SDK than the macos-26 image; matching OS and CPU alone does not make
native compilation compatible.

Windows x86_64 CI enrolls through the same restricted identity and sends
`Rustc` actions, including native linking, to the Windows MINI worker. Build
scripts and tests remain on GitHub's Windows runner. Both machines expose
MSVC 14.44.35207 through a `V:` drive mapping to the existing VC installation;
Bazel selects Windows SDK 10.0.26100.0. This avoids machine-specific Enterprise
versus BuildTools paths without copying the toolchain. The setup fails if the
pinned compiler is missing or `V:` is already occupied. The Windows execution
platform matches `OSFamily=windows` and `cpu_arch=x86_64`.

Rustc actions, including asupersync, prefer remote execution. Large Rust
compilations do not race a second copy on the smaller hosted runner.
TestRunner actions use Bazel dynamic execution with sandboxed local execution
and remote execution: both may start the same test, and Bazel keeps the first
result and cancels the other. Local work is admitted against the runner CPU
count and half its RAM; these scheduler estimates do not cap a compiler's
actual memory use. The shared action cache remains enabled. Tests carry Node,
Rust Analyzer, and their compiler tools as declared runfiles.
Each remote test reserves four worker CPU units and the Rust harness uses four
threads. The local branch reserves the hosted runner CPU budget so suites do not
compete with one another for local cores. Once the fleet is selected, remote compilation does not fall back to the hosted
runner. Linux/macOS dynamic tests retain their scheduled local branch. Actual compiler errors, failed
tests, and remote action execution timeouts still fail the build. BuildBuddy events
upload asynchronously, so a reporting outage does not fail a successful build.
A terminated runner can leave an incomplete report.

CI checks Tailscale enrollment and TCP connectivity to the executor before
selecting the private fleet. If either fails, it uses the hosted runner and
restores the usual master Actions disk cache. `fleet-enabled` reports that
observed selection; passing a client ID alone does not enable it. Jobs with a
reachable executor skip the multi-GB Actions disk-cache restore. The existing
Linux matrix is chosen before this check, so a fleet job that falls back still
builds and tests `//...` in that job rather than expanding into hosted shards.

The connectivity check proves that the coordinator accepts connections, not
that compatible workers are online. A reachable coordinator can queue an
action while its worker pool is offline. Local fallback follows infrastructure
errors from Bazel; it does not guarantee immediate recovery from a queue with
no workers. The checked-in execution platforms select OS and CPU architecture,
with CPU admission requirements. They do not define a high-memory worker
class for asupersync. Worker memory capacity and scheduling belong to the
private fleet configuration and must be verified from executed-action worker
identities and metrics.

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
clients on 50051 while keeping 50061 restricted to the enrolled fixed workers, including MINI.
Tailscale grants enforce the tag restriction; host iptables cannot match tags.
The action registers ephemeral nodes and logs them out during job cleanup.

## Verification

An authenticated hosted run must show Tailscale OIDC enrollment, successful
executor connectivity, remote cache or execution activity in Bazel's action
summary, and the BuildBuddy invocation URL. A cache hit alone does not prove
remote execution. Cold builds may show both sandboxed and remote test actions;
warm builds may use the cache entirely. An offline setup must report
`fleet-enabled=false` and restore the hosted action cache.
Actionlint checks workflow syntax but does not establish network or OIDC access.
