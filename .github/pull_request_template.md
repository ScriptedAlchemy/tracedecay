## Summary

<!-- Summarize what this PR does in 1-3 bullet points. -->

-

## Motivation

<!-- Why is this change needed? Link to an issue if applicable. -->

## Changes

<!-- List the main changes and explain any that need context. -->

-

## Test plan

- [ ] The affected Bazel targets pass
- [ ] `bazel build //...` passes when the change crosses package boundaries
- [ ] Tested manually (describe below if applicable)

<!-- For new language extractors, add a fixture file and an extraction test. -->
<!-- For sync/DB changes, test with `tracedecay sync --doctor` on a real project. -->

## Checklist

- [ ] `CHANGELOG.md` updated (under `[Unreleased]` if no version bump)
- [ ] No secrets, credentials, or `.env` files included
- [ ] Breaking changes documented (if any)
