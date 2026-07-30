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

### Publishing a new workspace library crate (e.g. `tracedecay-sdk`)

Workspace library crates (`crates/tracedecay-sdk`, and similarly `tracedecay-domain`/`tracedecay-store`) publish through the same `release-plz.yml` workflow and `crates-io` environment as the root `tracedecay` crate — there is no separate publish workflow. Each such crate carries a `[[package]]` entry in `release-plz.toml` that sets `git_release_enable = false` and `git_tag_enable = false`, so it is versioned and published alongside the root crate without claiming its own `vX.Y.Z` tag or GitHub Release (the root crate keeps sole ownership of those for binary distribution).

Because trusted publishing on crates.io is normally configured only after a crate's first version exists, a brand-new crate like `tracedecay-sdk` needs one of the following before its first automated release:

1. **Preferred — configure a pending trusted publisher.** On crates.io, a maintainer with the rights to claim the crate name creates a "pending" trusted publisher for `tracedecay-sdk` (GitHub Actions, repository `ScriptedAlchemy/tracedecay`, workflow `release-plz.yml`, environment `crates-io`) before the first publish. crates.io accepts the first `cargo publish` for that name via OIDC once the pending publisher exists, with no long-lived token involved.
2. **Fallback — one-time manual bootstrap publish.** If a pending trusted publisher cannot be configured ahead of time, a maintainer runs `cargo publish -p tracedecay-sdk` once from a trusted local machine using a short-lived, scoped crates.io API token (never committed or stored as a repository secret). After that first version exists, configure the crates.io trusted publisher for `ScriptedAlchemy/tracedecay` / `release-plz.yml` / `crates-io` exactly as described above, and all subsequent releases flow through the normal automated path.

Either way, no second GitHub Actions workflow is introduced: `release-plz.yml`'s existing `crates-io` environment and OIDC (`id-token: write`) permission cover every workspace crate release-plz decides to publish.

Release PRs may modify only `CHANGELOG.md`, `Cargo.lock`, and `Cargo.toml`. The
read-only `Release PR integrity` workflow loads its guard from the trusted base
commit, not from the proposed release branch. If a reviewed release PR must
carry another change, apply the `release-extra-files-approved` label; tracked
files that are also ignored remain forbidden because release-plz omits them
when it creates its temporary repository copy.

## npm and PyPI Setup (TypeScript and Python SDKs)

`sdks/typescript` (`@tracedecay/sdk`) and `sdks/python` (`tracedecay-sdk`) are not
managed by release-plz — their versions live in `package.json` and
`pyproject.toml` and are bumped by hand in a normal PR. Publishing runs through
the single `sdk-publish.yml` workflow, dispatched manually with an `sdk` input
of `typescript` or `python`. Each job runs typecheck, the fast unit suite, and
the real-daemon installed-package conformance suite (building the actual
`tracedecay` binary and driving it end-to-end) before publishing; a green fast
unit suite alone never authorizes a publish.

Both jobs publish via OIDC trusted publishing — no long-lived `NPM_TOKEN` or
PyPI API token is stored as a repository secret.

### npm Trusted Publishing bootstrap

npm cannot attach a trusted publisher to a package that has never been
published, so a brand-new package name needs a one-time, manually reviewed
bootstrap. Do not run third-party bootstrap tools such as
`npx setup-npm-trusted-publish` — that executes an unpinned, unscoped,
unaudited package while your local npm credentials are live, which is exactly
the kind of supply-chain exposure trusted publishing exists to eliminate. Use
the manual path instead:

1. On <https://www.npmjs.com/settings/~/tokens>, generate a short-lived,
   *scoped* (single-package, publish-only) npm access token.
2. From `sdks/typescript`, run `npm run build` followed by
   `NODE_AUTH_TOKEN=<token> npm publish --access public` once, using that
   token (never commit it or store it as a repository secret).
3. Immediately revoke the token on npmjs.com.

Once `@tracedecay/sdk` exists, configure its trusted publisher at
`https://www.npmjs.com/package/@tracedecay/sdk/access` → **Trusted
Publisher** → **GitHub Actions**:

- Organization or user: `ScriptedAlchemy`
- Repository: `tracedecay`
- Workflow filename: `sdk-publish.yml`
- Environment name: `npm-tracedecay-sdk`
- Allowed actions: **Allow npm publish** (staged publishing is not used here)

The `sdk-publish.yml` workflow's `publish-typescript` job already sets
`permissions: id-token: write` and runs `npm publish --provenance` with no
token, so publishes succeed the moment the trusted publisher exists. Before
relying on this for a real release, run one **protected, master-only OIDC
smoke publish**: dispatch `sdk-publish.yml` with `sdk: typescript` against an
unused prerelease version (e.g. bump to `0.1.0-oidc-smoke.1` in a throwaway
commit on a short-lived branch merged to `master`, publish, then deprecate
that version) to prove the trusted publisher and environment protections work
end-to-end, before ever dispatching against a real release version. A dry run
alone (`npm pack`) does not exercise the OIDC exchange and is insufficient.

### PyPI Trusted Publishing bootstrap

PyPI supports configuring a trusted publisher for a project name *before* it
has ever been published ("pending publisher"). A maintainer with rights to
claim `tracedecay-sdk` on PyPI adds a pending publisher at
<https://pypi.org/manage/account/publishing/> with owner
`ScriptedAlchemy`, repository `tracedecay`, workflow filename
`sdk-publish.yml`, and environment `pypi-tracedecay-sdk`. The first dispatch of
`sdk-publish.yml` with `sdk: python` then publishes via
`pypa/gh-action-pypi-publish` (OIDC, `id-token: write`) with no PyPI API token
required, and PyPI converts the pending publisher into a normal one after that
first publish.

If a pending publisher cannot be configured ahead of time, fall back to a
one-time manual `twine upload` from `sdks/python/dist` using a short-lived,
scoped PyPI API token, then configure the trusted publisher on the now-existing
project for all subsequent releases.

As with npm, run one **protected, master-only OIDC smoke publish** (dispatch
`sdk-publish.yml` with `sdk: python` against an unused prerelease version,
e.g. `0.1.0.dev1`, merged to `master` first) before ever dispatching against a
real release version, to prove PyPI accepts the OIDC exchange for this exact
repository/workflow/environment triple.

### Required GitHub environments

Create the `npm-tracedecay-sdk` and `pypi-tracedecay-sdk` GitHub Actions
environments (Settings → Environments) referenced by `sdk-publish.yml`. Unlike
`release-plz.yml`'s `crates-io` environment (which only ever runs from an
automated release PR merge), these environments guard a *manually dispatched*
workflow that can target any branch, so their protection rules are required,
not optional:

- **Deployment branches and tags**: restrict to the `master` branch only. The
  workflow's own `if: github.ref == 'refs/heads/master'` guard is defense in
  depth, not a substitute for this — treat both as required.
- **Required reviewers**: add at least one reviewer who is not the person
  expected to dispatch the workflow. GitHub does not block the dispatching
  actor from also being a required reviewer, so this only holds if the
  reviewer list is enforced by team convention as someone other than whoever
  ran `workflow_dispatch`.
- Leave **"Allow administrators to bypass configured protection rules"**
  disabled, and do not grant repository admins a standing bypass for these two
  environments.

## Normal Release Flow

1. Merge feature/fix PRs into `master`.
2. `Release-plz` opens or updates a release PR.
3. Review the generated version and changelog.
4. Merge the release PR.
5. `Release-plz` publishes the crate and creates the GitHub Release.
6. The GitHub Release triggers `release.yml`, which builds and uploads binaries and updates package-manager manifests.

## Manual Recovery

If release-plz publishes the crate but the binary artifact workflow does not run, check whether `RELEASE_PLZ_TOKEN` was configured. Then manually dispatch `Release` from the Actions tab against the release tag.
