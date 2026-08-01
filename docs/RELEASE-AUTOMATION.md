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

Rust workspace crates use crates.io Trusted Publishing. The sole publication
authority is GitHub Actions for `ScriptedAlchemy/tracedecay`, workflow
`release-plz.yml`, environment `crates-io`. The root workspace and
`release-plz.toml` decide which crates are versioned and published; no SDK
workflow runs `cargo publish`.

Workspace library crates (`crates/tracedecay-sdk`, and similarly
`tracedecay-domain`/`tracedecay-store`) publish through this same root
release-plz path. Each library crate's `[[package]]` entry disables its own
GitHub Release and git tag, leaving those binary-distribution identities with
the root `tracedecay` crate.

Release PRs may modify only `CHANGELOG.md`, `Cargo.lock`, and `Cargo.toml`. The
read-only `Release PR integrity` workflow loads its guard from the trusted base
commit, not from the proposed release branch. If a reviewed release PR must
carry another change, apply the `release-extra-files-approved` label; tracked
files that are also ignored remain forbidden because release-plz omits them
when it creates its temporary repository copy.

## SDK Distribution

The TypeScript SDK (`sdks/typescript`, package `@tracedecay/sdk`) is the only
non-Rust registry-published SDK. Its version is reviewed in `package.json`.
A maintainer dispatches `sdk-publish.yml` from `master`; the workflow has no
package selector and rejects non-canonical repositories and non-`master` refs.

The unprivileged job installs pinned tooling, packs one npm tarball, and runs
typecheck, unit tests, and real-daemon installed-package conformance against
that exact tarball. It stages a digest-verified npm CLI with the tested package.
Only the protected publish job receives `id-token: write`; it re-verifies both
artifacts and publishes the unchanged tarball without installing executable
packages or using an npm token.

The Rust SDK (`crates/tracedecay-sdk`) is published to crates.io through the
standard release automation. There is no Python SDK.

### npm Trusted Publishing

Configure the existing `@tracedecay/sdk` package's trusted publisher at
`https://www.npmjs.com/package/@tracedecay/sdk/access` → **Trusted
Publisher** → **GitHub Actions**:

- Organization or user: `ScriptedAlchemy`
- Repository: `tracedecay`
- Workflow filename: `sdk-publish.yml`
- Environment name: `npm-tracedecay-sdk`
- Allowed actions: **Allow npm publish** (staged publishing is not used here)

Create the `npm-tracedecay-sdk` GitHub Actions environment referenced by the
workflow. Its protection rules are required:

- **Deployment branches and tags**: restrict to `master` only. The workflow's
  exact repository/ref guard is defense in depth, not a substitute.
- **Required reviewers**: add at least one reviewer who is not the person
  expected to dispatch the workflow. GitHub does not block the dispatching
  actor from also being a required reviewer, so this only holds if the
  reviewer list is enforced by team convention as someone other than whoever
  ran `workflow_dispatch`.
- Leave **"Allow administrators to bypass configured protection rules"**
  disabled, and do not grant repository admins a standing bypass.

## Normal Release Flow

1. Merge feature/fix PRs into `master`.
2. `Release-plz` opens or updates a release PR.
3. Review the generated version and changelog.
4. Merge the release PR.
5. `Release-plz` publishes the crate and creates the GitHub Release.
6. The GitHub Release triggers `release.yml`, which builds and uploads binaries and updates package-manager manifests.

## Manual Recovery

If release-plz publishes the crate but the binary artifact workflow does not run, check whether `RELEASE_PLZ_TOKEN` was configured. Then manually dispatch `Release` from the Actions tab against the release tag.
