# Release Automation

TraceDecay uses two workflows for stable releases:

1. `Release-plz` runs on pushes to `master`.
   - Opens or updates a release PR.
   - Bumps `Cargo.toml` and `Cargo.lock`.
   - Updates `CHANGELOG.md`.
   - Publishes the `tracedecay` crate to crates.io when the release PR is merged.
   - Creates the `vX.Y.Z` tag and GitHub Release.
2. `Release` runs after a GitHub Release is published.
   - Builds platform binaries.
   - Uploads release assets.
   - Updates the Homebrew tap, Scoop bucket, and `server.json`.

`release.yml` intentionally does not run `cargo publish`; crates.io publishing belongs to `release-plz.yml`.

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

- `RELEASE_PLZ_TOKEN`: fine-grained PAT or GitHub App token with read/write `Contents` and `Pull requests` access. This token is important because releases created with the default `GITHUB_TOKEN` do not trigger the follow-up `release.yml` workflow.
- `TAP_GITHUB_TOKEN`: token that can push to `ScriptedAlchemy/homebrew-tap` and `ScriptedAlchemy/scoop-bucket`.

## Crates.io Setup

The `tracedecay` crate uses crates.io Trusted Publishing. The trusted publisher is GitHub Actions for `ScriptedAlchemy/tracedecay`, workflow `release-plz.yml`, environment `crates-io`.

The first version of a crate must exist before trusted publishing can be configured. `tracedecay` already exists on crates.io, so release-plz publishes via GitHub Actions OIDC instead of a long-lived crates.io token.

After that, release-plz detects unpublished changes from crates.io, opens a release PR, and publishes on merge.

## Normal Release Flow

1. Merge feature/fix PRs into `master`.
2. `Release-plz` opens or updates a release PR.
3. Review the generated version and changelog.
4. Merge the release PR.
5. `Release-plz` publishes the crate and creates the GitHub Release.
6. The GitHub Release triggers `release.yml`, which builds and uploads binaries and updates package-manager manifests.

## Manual Recovery

If release-plz publishes the crate but the binary artifact workflow does not run, check whether `RELEASE_PLZ_TOKEN` was configured. Then manually dispatch `Release` from the Actions tab against the release tag.
