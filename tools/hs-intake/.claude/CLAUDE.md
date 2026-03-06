# Project Guidelines

## Git
- Always use `git commit -s` (sign-off) when committing
- Always use `git tag -s` (GPG sign) when tagging
- Before tagging a release, update CHANGELOG.md and README.md to reflect the new version's changes. Use the tag message identical to the new CHANGELOG.md entry
- Before tagging, verify there are no uncommitted changes (`git status` must be clean)

## Code Style
- Every source file must start with `// SPDX-License-Identifier: MPL-2.0`

## Testing
- Always write corresponding tests when adding or modifying features
- Run `cargo cov` before committing to verify tests pass and coverage does not regress
- Coverage must stay at or above 80% line coverage
