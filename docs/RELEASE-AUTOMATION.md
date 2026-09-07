# Release Automation

TraceDecay uses two workflows with one publication authority:

1. `Release Please` runs on pushes to `master`.
   - Opens or updates a release PR.
   - Bumps `.release-please-manifest.json`, `version.txt`, `Cargo.toml`,
     `server.json`, and `Cargo.lock`.
   - Updates `CHANGELOG.md`.
   - Creates the `vX.Y.Z` tag and GitHub Release.
2. `Release` runs after a GitHub Release is published.
   - Builds platform binaries.
   - Uploads release assets, checksums, and `install.sh`.
   - Updates the in-repository `server.json` MCP registry manifest.
   - Builds, conformance-tests, and publishes the TypeScript SDK
     (`@tracedecay/sdk`) to npm, gated on the release verification job.

Release packaging decision (owner, 2026-08-07): binaries ship through GitHub
Release assets; the TypeScript SDK ships through npm on the same release
trigger; crates.io publication waits until crate naming and structure are
settled. Neither workflow runs `cargo publish`, and no crates.io publish step
may be added before that decision.

## Required GitHub Setup

Set repository Actions workflow permissions to allow write access:

```bash
gh api \
  --method PUT \
  repos/ScriptedAlchemy/tracedecay/actions/permissions/workflow \
  -f default_workflow_permissions=write \
  -F can_approve_pull_request_reviews=true
```

Add these repository secrets:

- `RELEASE_PLZ_TOKEN`: fine-grained PAT or GitHub App token with read/write
  `Contents` and `Pull requests` access. The existing secret name is retained
  for compatibility. Releases created with the default `GITHUB_TOKEN` do not
  trigger the follow-up `release.yml` workflow.
npm publication is tokenless: no npm secret exists anywhere in the repository
or its workflows. The publish job authenticates through npm trusted publishing
(OIDC) — it holds `id-token: write`, and the pinned npm CLI (12.0.2, above the
11.5.1 trusted-publishing floor; Node 22.23.2, above the 22.14.0 floor)
exchanges the GitHub OIDC token for a short-lived publish credential itself.
Provenance is attached automatically by trusted publishing. No
`NPM_TOKEN`/`NODE_AUTH_TOKEN`, `registry-url`, or `.npmrc` auth may be added
to the publish job: a configured token shadows the OIDC exchange.

## One-time npm trusted publisher setup (owner, before the first release)

npm's trusted publisher is configured in the settings of an existing package,
so a brand-new package needs a one-time manual seed publish (an interactive
`npm publish` by a maintainer with 2FA) to create `@tracedecay/sdk` before the
trusted publisher can be added. After that, on npmjs.com under
Package → Settings → Trusted publishing, add a GitHub Actions publisher with
exactly:

- Organization or user: `ScriptedAlchemy`
- Repository: `tracedecay`
- Workflow filename: `release.yml` (filename only, extension included)
- Environment name: `npm-tracedecay-sdk`
- Allowed actions: `npm publish`

All fields are case-sensitive and unvalidated at save time; mismatches only
surface as `ENEEDAUTH`/404 at publish time — the publish job names this
configuration in its failure message. Only GitHub-hosted runners are
supported (the job uses `ubuntu-latest`). After the first successful OIDC
publish, set Package → Settings → Publishing access to "Require two-factor
authentication and disallow tokens" so trusted publishing is the only publish
path. On the GitHub side, create the `npm-tracedecay-sdk` environment with
required reviewers if release-time approval is wanted; the environment gates
review, while authentication itself is the OIDC workflow identity.

Release PRs may modify only `.release-please-manifest.json`, `CHANGELOG.md`,
`Cargo.lock`, `Cargo.toml`, `server.json`, and `version.txt`. The
read-only `Release PR integrity` workflow loads its guard from the trusted base
commit, not from the proposed release branch. If a reviewed release PR must
carry another change, apply the `release-extra-files-approved` label; tracked
files that are also ignored remain forbidden.

## Release artifact acceptance

Release acceptance exercises the produced archive and installed binary, never
a source-tree file inventory or a release-PR path policy. The archive must
contain a self-contained Rust package graph and the embedded dashboard and
first-party host assets required by the binary.

The installed binary is exercised with a fresh isolated host profile for every
supported host. Each official host operation must install, update, and
uninstall only its owned files while preserving unrelated profile content; the
same embedded artifact identity must be observed throughout. A supported host
that defers or cannot complete one of those operations blocks acceptance. A
host without an evidenced native registration remains a typed unavailable
result, rather than a successful empty install.

Recorded native host events remain historical-ingestion evidence: they pass
through the production decoder and ingestion path, not a synthetic packaging
fixture. They do not substitute for the installed host lifecycle journey.

## SDK release boundary

The TypeScript SDK (`@tracedecay/sdk`) is a release artifact: `release.yml`
publishes it to npm on the stable release trigger. The Rust SDK and every
other Cargo workspace package remain private (`publish = false`) until the
crate-naming decision lands, and the binary release jobs package no SDK
clients. The npm publication keeps build authority separate from its
protected publish job. The unprivileged build job regenerates the client
exclusively from the canonical SDK executable-binding registry and verifies
codegen parity before running typecheck, tests, a package dry-run, and
real-daemon conformance against the exact tarball whose digest it records.
The publish job depends on both that build and the release verification job,
digest-verifies the artifact bytes, and publishes through tokenless npm
trusted publishing (OIDC, provenance attached automatically) using the
digest-pinned reviewed npm CLI.
Missing schemas or mounted routes remain explicit unavailable entries in the
binding registry; they are not compared against a separate manually
maintained HTTP operation set.

The SDK package version lives in `sdks/typescript/package.json` and is
independent of the daemon version; bump it in ordinary PRs. A prerelease SDK
version (containing `-`) publishes under the `beta` dist-tag, mirroring the
beta release convention; stable versions take `latest`. Re-running the
workflow for a tag whose SDK version is already on the registry is a
byte-verified no-op; identical version with different bytes fails and
requires an SDK version bump. The `scripts/check-sdk-publish-workflow.py`
gate (run by SDK conformance CI) enforces this job isolation.

## Normal Release Flow

1. Merge feature/fix PRs into `master`.
2. `Release Please` opens or updates a release PR.
3. Review the generated version and changelog.
4. Merge the release PR.
5. `Release Please` creates the tag and GitHub Release.
6. The GitHub Release triggers `release.yml`, which builds and uploads
   checksummed GitHub Release assets, refreshes `server.json`, and publishes
   `@tracedecay/sdk` to npm once release verification passes.

## Beta Channel

The `codex/tracedecay-total-redesign-plan` branch runs its own release-please
channel: `beta-release-please.yml` with `release-please-config-beta.json` and
`.release-please-manifest-beta.json` (versioning strategy `prerelease`,
prerelease type `beta`). Every push to the branch opens or updates a release
PR; merging it tags `vX.Y.Z-beta.N` and publishes a GitHub prerelease, which
triggers `release-beta.yml` to build, attest, and upload
`tracedecay-beta-<tag>-<platform>` archives plus `SHA256SUMS`. Prereleases are
never marked `latest`, and the CLI's beta upgrade channel
(`src/cloud.rs::asset_name`) resolves exactly these asset names. Manual
install (macOS arm64):

```sh
gh release download <tag> -p "tracedecay-beta-<tag>-aarch64-macos.tar.gz"
tar xzf "tracedecay-beta-<tag>-aarch64-macos.tar.gz"
install -m 755 tracedecay ~/.cargo/bin/tracedecay
```

## Manual Recovery

If the GitHub Release is created but the binary artifact workflow does not run,
check whether `RELEASE_PLZ_TOKEN` was configured. For recovery, dispatch the
workflow from the release tag ref and pass that same tag as `release_tag`.
Recovery verifies every retained archive or MCPB against its GitHub attestation,
exact tag SHA, and signer workflow, then builds only targets with missing
assets. It never rebuilds an uploaded binary to compare bytes from a later
runner or linker.
