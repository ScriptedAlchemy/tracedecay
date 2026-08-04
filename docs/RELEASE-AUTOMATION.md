# Release Automation

TraceDecay uses two workflows for stable releases:

1. `Release Please` runs on pushes to `master`.
   - Opens or updates a release PR.
   - Bumps `.release-please-manifest.json`, `version.txt`, `Cargo.toml`, and
     `Cargo.lock`.
   - Updates `CHANGELOG.md`.
   - Creates the `vX.Y.Z` tag and GitHub Release.
2. `Release` runs after a GitHub Release is published.
   - Builds platform binaries.
   - Uploads release assets, checksums, and `install.sh`.
   - Updates the Homebrew tap, Scoop bucket, and `server.json`.

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
- `TAP_GITHUB_TOKEN`: token that can push to `ScriptedAlchemy/homebrew-tap` and `ScriptedAlchemy/scoop-bucket`.

Release PRs may modify only `.release-please-manifest.json`, `CHANGELOG.md`,
`Cargo.lock`, `Cargo.toml`, and `version.txt`. The
read-only `Release PR integrity` workflow loads its guard from the trusted base
commit, not from the proposed release branch. If a reviewed release PR must
carry another change, apply the `release-extra-files-approved` label; tracked
files that are also ignored remain forbidden.

## Normal Release Flow

1. Merge feature/fix PRs into `master`.
2. `Release Please` opens or updates a release PR.
3. Review the generated version and changelog.
4. Merge the release PR.
5. `Release Please` creates the tag and GitHub Release.
6. The GitHub Release triggers `release.yml`, which builds and uploads binaries and updates package-manager manifests.

## Manual Recovery

If the GitHub Release is created but the binary artifact workflow does not run,
check whether `RELEASE_PLZ_TOKEN` was configured. Then manually dispatch
`Release` from the Actions tab against the release tag.
