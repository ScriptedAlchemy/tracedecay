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
serves macOS actions; this CI integration does not send Linux actions to it.
ARM Linux retains hosted execution. macOS ARM64 selects the Darwin fleet profile;
native compilation still requires compatible Xcode/SDK installations, not just
matching OS and CPU architecture.

Windows x86_64 CI enrolls through the same restricted identity and shares
`Rustc` actions, including native linking, between the hosted runner and MINI.
Build scripts and tests remain on GitHub's Windows runner. Both machines expose
MSVC 14.44.35207 through a `V:` drive mapping to the existing Visual Studio installation;
Bazel selects Windows SDK 10.0.26100.0. This avoids machine-specific Enterprise
versus BuildTools paths without copying the toolchain. The setup fails if the
pinned compiler is missing or `V:` is already occupied. The Windows execution
platform matches `OSFamily=windows` and `cpu_arch=x86_64`.

All fleet-enabled CI runners use dynamic execution for Rustc actions. The
GitHub-hosted runner and compatible remote workers may compile the same action;
Bazel keeps the first result and cancels the competing branch. Local compilation
uses two CPU slots and half the hosted runner's RAM, alongside sixteen in-flight
actions on Windows or forty on Linux/macOS. These resource estimates bound
admission, not the compiler's actual memory consumption. Workstations already
enrolled as NativeLink workers retain their shared-scheduler profile so the same
physical resources are not admitted independently twice.

Linux and macOS use sandboxed local execution. Windows uses the local strategy
with `--experimental_local_lockfree_output` so a local compiler does not take the
output lock before execution and prevent a remote result from winning.
Linux/macOS TestRunner actions also use dynamic execution; Windows tests remain
local. Tests carry Node, Rust Analyzer, and their compiler tools as declared
runfiles. Each remote test reserves four worker CPU units and its Rust harness
uses four threads. The local test branch reserves the hosted runner CPU budget
so suites do not compete with one another for local cores. The shared action
cache remains enabled.

Remote-to-local fallback is disabled: dynamic execution already owns its local
branch, and fallback could start a second unsandboxed compiler writing the same
outputs. Remote infrastructure failures are retried; exhausted retries, actual
compiler errors, failed tests, and remote execution timeouts still fail the build.
BuildBuddy events upload asynchronously, so a reporting outage does not fail a
successful build.
A terminated runner can leave an incomplete report.

Completed cacheable local actions upload their results synchronously to the shared
cache. This avoids leaving completed outputs in a background upload queue when a
new push cancels the runner. Cache reuse still requires matching action inputs,
flags and toolchains, and retained output blobs. Cancellation during compilation
or upload does not create a reusable result. Shared-cache entries are independent
of GitHub run IDs and are not deleted when a workflow is cancelled. In contrast,
the hosted-only disk cache is saved after builds and its save is skipped on
cancellation; a cancelled VM's unsaved disk contents do not survive.

Every Windows job using `setup-bazel`, including release jobs, verifies Defender
real-time scanning is disabled (or its service is stopped) before building. The
preflight is restricted to disposable GitHub-hosted runners, reports CPU/memory
and free-space information in the job summary, and fails if scanning remains
enabled or its effective state cannot be checked. It does not change persistent
fleet machines. Windows test jobs also put temporary files on the workspace drive.

CI checks Tailscale enrollment and TCP connectivity to the executor before
selecting the private fleet. If either fails, it uses the hosted runner and
restores the usual master Actions disk cache. `fleet-enabled` reports that
observed selection; passing a client ID alone does not enable it. Jobs with a
reachable executor skip the multi-GB Actions disk-cache restore. The existing
Linux matrix is chosen before this check, so a fleet job that falls back still
builds and tests `//...` in that job rather than expanding into hosted shards.

The connectivity check proves that the coordinator accepts connections, not
that compatible workers are online. A reachable coordinator can queue an
action while its worker pool is offline. Dynamic compilation can still progress on the hosted runner while
waiting for a compatible worker. The checked-in execution platforms select OS and CPU architecture, with
CPU admission requirements. They do not define a high-memory worker
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
