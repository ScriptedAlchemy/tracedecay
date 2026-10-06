## Summary

<!-- What does this PR do? 1-3 bullet points. -->

-

## Motivation

<!-- Why is this change needed? Link to an issue if applicable. -->

## Changes

<!-- Key files/areas modified. Highlight anything non-obvious. -->

-

## Test plan

- [ ] The affected Bazel targets pass
- [ ] `bazel build //...` passes when the change crosses package boundaries
- [ ] Tested manually (describe below if applicable)

<!-- For new language extractors: fixture file added, extraction test added. -->
<!-- For sync/DB changes: tested with `tracedecay sync --doctor` on a real project. -->

## Checklist

- [ ] `CHANGELOG.md` updated (under `[Unreleased]` if no version bump)
- [ ] No secrets, credentials, or `.env` files included
- [ ] Breaking changes documented (if any)
