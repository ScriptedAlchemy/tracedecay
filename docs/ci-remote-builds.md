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

Windows x86_64 CI enrolls through the same restricted identity and sends
`Rustc` actions, including native linking, to the Windows MINI worker.
Build scripts and tests remain on GitHub's Windows runner. Both machines expose
MSVC 14.44.35207 through a `V:` drive mapping to the existing Visual Studio installation;
Bazel selects Windows SDK 10.0.26100.0. This avoids machine-specific Enterprise
versus BuildTools paths without copying the toolchain. The setup fails if the
pinned compiler is missing or `V:` is already occupied. The Windows execution
platform matches `OSFamily=windows` and `cpu_arch=x86_64`.

Linux and macOS CI runners use dynamic execution for Rustc actions. The
GitHub-hosted runner and compatible remote workers may compile the same action;
Bazel keeps the first result and cancels the competing branch. Local compilation
uses two CPU slots and half the hosted runner's RAM, alongside forty in-flight
actions. Windows keeps sixteen in-flight actions and the same two local CPU slots
for build scripts, tests, and fallback compilation. These resource estimates bound
admission, not the compiler's actual memory consumption. Workstations already
enrolled as NativeLink workers retain their shared-scheduler profile so the same
physical resources are not admitted independently twice.

Linux and macOS use sandboxed local execution, so the losing branch never
writes the shared output tree. Windows has no local sandbox: an unsandboxed
dynamic branch wrote the execroot while the remote result for the same action
landed, failing builds with missing `.rlib`/`.pdb` outputs and output-digest
mismatches. Windows therefore compiles Rustc actions remotely only.
Linux/macOS TestRunner actions also use dynamic execution; Windows tests remain
local. Tests carry Node, Rust Analyzer, and their compiler tools as declared
runfiles. Each remote test reserves four worker CPU units and its Rust harness
uses four threads. The local test branch reserves the hosted runner CPU budget
so suites do not compete with one another for local cores. The shared action
cache remains enabled.

Linux and macOS disable remote-to-local fallback because dynamic execution
already owns their local branch. Windows enables it: after the remote attempt
fails with a worker infrastructure error, the hosted runner compiles that action
once with the same pinned compiler and SDK. The two copies run in sequence and
never concurrently. Actual compiler errors, failed tests, and remote execution
timeouts still fail the build.
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
action while its worker pool is offline. Linux and macOS dynamic compilation can
still progress on the hosted runner while waiting for a compatible worker;
Windows compilation waits for MINI. The checked-in execution platforms select OS
and CPU architecture, with CPU admission requirements. They do not define a high-memory worker
class for asupersync. Worker memory capacity and scheduling belong to the
private fleet configuration and must be verified from executed-action worker
identities and metrics.

## Trust configuration

The OIDC clients are repository variables, not secrets. Each one's Tailscale
OIDC identity must have only Auth Keys write scope, restricted to
`tag:bazel-ci`:

- Issuer: `https://token.actions.githubusercontent.com`
- Subject: `repo:ScriptedAlchemy/tracedecay:*`
- Custom claim `repository_id`: `1261602522`
- Custom claim `actor_id`: the actor listed for that client below
- Custom claim `workflow_ref`: `ScriptedAlchemy/tracedecay/.github/workflows/<workflow>@*`
  for every workflow that requests the client
- Audience: `api.tailscale.com/<client-id>`

Tailscale checks these signed claims independently of the workflow condition.
Adding another maintainer requires an explicit trust change.

### The subject claim does not separate forks from branches

GitHub mints the OIDC token for the repository whose workflow runs, so a fork
pull request presents the same `repo:ScriptedAlchemy/tracedecay:pull_request`
subject as a same-repository one. A trust condition written on the `sub` claim
alone therefore also accepts a fork's run. The `head.repo.full_name` test inside
a workflow is not an authorization boundary either, because a pull request can
edit the workflow that performs it. Every client's trust condition must
therefore require the `actor_id` custom claim.

### Clients

Three clients can request fleet credentials, and each is a separate Tailscale
OAuth client with its own audience and `actor_id` claim:

| Variable | `actor_id` claim | Actor |
| --- | --- | --- |
| `TS_BAZEL_CLIENT_ID` | `25274700` | `ScriptedAlchemy` |
| `TS_BAZEL_BOT_CLIENT_ID` | `158243242` | the repository automation bot |
| `TS_BAZEL_RENOVATE_CLIENT_ID` | `29139614` | `renovate[bot]` |

Every workflow that requests a client id, and the client it may receive:

- `ci.yml`: `debug-cli`, `gates`, `macos-test`, `linux-test`, `clippy`,
  `windows-test`, `shipped-cli` — any of the three.
- `sdk-conformance.yml`: `packages`, `production-router` — owner or bot.
- `release.yml`, `release-beta.yml`: `build` — owner or bot.
- `release-please.yml`: `release-please` — owner or bot.
- `upstream-compat.yml`: `bazel-10-readiness` — owner or bot.
- `hermetic-evals.yml`: `test` — owner or bot.

Each of those sites also pins the same repository and actor in a workflow
expression. The expression is convenience: the signed `actor_id` claim in
Tailscale is the control, and it is the only part that a pull request cannot
edit. These conditions are configured in the Tailscale admin console and are
not stored in this repository, so a change to a client, its audience, or its
claims needs a matching console change; review both.

`setup-bazel` refuses to exchange OIDC from any runner whose
`runner.environment` is not `github-hosted`, because a persistent machine would
keep the node identity after the job ends. All Actions jobs use GitHub-hosted
runners; NativeLink remains a separate Bazel execution service.

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
