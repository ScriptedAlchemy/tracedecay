# Release Automation

TraceDecay uses two workflows with one publication authority:

1. `Release Please` runs on pushes to `master`.
   - Opens or updates a release PR.
   - Bumps `.release-please-manifest.json`, `version.txt`, `Cargo.toml`, and
     `Cargo.lock`.
   - Updates `CHANGELOG.md`.
   - Creates the `vX.Y.Z` tag and GitHub Release.
2. `Release` runs after a GitHub Release is published.
   - Builds platform binaries.
   - Uploads release assets, checksums, and `install.sh`.
   - Updates the in-repository `server.json` MCP registry manifest.

Neither workflow runs `cargo publish`. Stable distribution is through GitHub
Release assets.

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

Release PRs may modify only `.release-please-manifest.json`, `CHANGELOG.md`,
`Cargo.lock`, `Cargo.toml`, and `version.txt`. The
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

No SDK is currently a release artifact. The Rust SDK and every other Cargo
workspace package are private (`publish = false`), and the binary release
workflow packages no SDK clients. Do not dispatch the existing npm workflow
until its unprivileged build derives catalog-to-client parity from the mounted
HTTP operation authority and fails before the protected OIDC job whenever a
concrete schema, generated client, or route is missing.

## Normal Release Flow

1. Merge feature/fix PRs into `master`.
2. `Release Please` opens or updates a release PR.
3. Review the generated version and changelog.
4. Merge the release PR.
5. `Release Please` creates the tag and GitHub Release.
6. The GitHub Release triggers `release.yml`, which builds and uploads
   checksummed GitHub Release assets and refreshes `server.json`.

## Manual Recovery

If the GitHub Release is created but the binary artifact workflow does not run,
check whether `RELEASE_PLZ_TOKEN` was configured. For recovery, dispatch the
workflow from the release tag ref and pass that same tag as `release_tag`.
Recovery verifies every retained archive or MCPB against its GitHub attestation,
exact tag SHA, and signer workflow, then builds only targets with missing
assets. It never rebuilds an uploaded binary to compare bytes from a later
runner or linker.
