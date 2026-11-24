<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->
<!--
Thanks for contributing. CONTRIBUTING.md explains each item below.
The "Contribution checks" check fails until the issue is linked and
"Allow edits by maintainers" is checked.
-->

<!-- Fixes #N for a bug, Resolves #N for a feature, Refs #N for part of one -->
Fixes #

## What this changes

<!-- What a user of the tool sees differently, then how. -->

## Checklist

- [ ] "Allow edits by maintainers" is checked (GitHub does not offer it
      for an organization's fork)
- [ ] `make check` passes
- [ ] `make cov-diff` shows every changed file at 85% or above, or the
      description says why one can't be
- [ ] Tests do not reach the external network
- [ ] A `CHANGELOG.md` entry under `## Unreleased`, for anything a user
      would notice
- [ ] `make man` run and the pages committed, if a command line changed
- [ ] Every commit is signed off (`git commit -s`), and carries
      `Assisted-by: AGENT:MODEL` if an AI tool helped write it
