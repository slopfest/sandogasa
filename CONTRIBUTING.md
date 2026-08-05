<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# Contributing to sandogasa

Thanks for your interest. This is a workspace of cross-distribution
packaging tools — primarily Fedora, CentOS and Debian — with the
activity-tracking crates useful more broadly. Library crates live in
`crates/`, binaries in `tools/`. Contributions are welcome under the
terms below.

Issues and pull requests: <https://github.com/slopfest/sandogasa>.

## Work from a specific issue

A contribution should fix or implement **one identified thing**. Open an
issue first (or comment on an existing one) describing the bug or the
feature, and reference it from the pull request — `Fixes #123` for a bug,
`Refs #123` otherwise.

This is not bureaucracy: these tools write to Bugzilla, Bodhi, dist-git,
GitLab and Debian archives on a maintainer's behalf, so a change needs a
stated purpose against which its behavior can be judged. Pull requests
that arrive without one are hard to review and slow to land.

In particular, please don't send:

- sweeping refactors, reformatting, or dependency churn that nobody
  asked for, mixed in with a fix;
- several unrelated changes in one pull request — split them;
- rewrites of a tool's approach without discussing the design first.

Small, obvious corrections (a typo, a broken link, a stale doc line) are
fine to send directly without an issue.

## What a contribution includes

**Tests that pass.** Add or update tests alongside the change, and make
sure `cargo test --workspace` is green. Tests must not reach the
external network: `cargo test` has to pass in a distro packaging sandbox
(Koji, Debian buildds) where external network is blocked but loopback is
up. Stand up a [`wiremock`](https://docs.rs/wiremock) server on
`127.0.0.1` for HTTP clients, or keep the logic in injectable functions
and test those against recorded data. If a test genuinely needs a live
remote endpoint, mark it `#[ignore]` so the default run stays offline.

**Formatting and lints.** Run `cargo fmt --all`, and don't introduce
`cargo clippy --workspace --all-targets` warnings — fix any in code you
touch. Every source file starts with
`// SPDX-License-Identifier: Apache-2.0 OR MIT`.

**Documentation, in the right place.** A crate's `README.md` describes
what it does *now*. The reasoning for a change goes in `CHANGELOG.md`
under `## Unreleased`; a rule that future work must follow goes in that
tool's `DEVELOPMENT.md`. If a change makes an existing doc section
untrue, fix that section in the same commit.

**Regenerated man pages** when you change a command-line interface.
Each tool's `man/<tool>.1` is generated from its clap definition, never
hand-edited: run `scripts/gen-man.sh` and commit the result. A test per
tool fails if a page stops documenting a flag, so this is not optional.
Prose for the man page's DESCRIPTION belongs in a doc comment on the
tool's `Cli` struct, where it also becomes the long `--help` output.

**A CHANGELOG entry** for anything user-visible. When a change breaks
something, the entry's heading ends with `(breaking)` — or a narrower
form like `(breaking CLI)` / `(breaking JSON)` — and the body enumerates
what broke, with a migration hint.

Before opening a pull request, please also run:

```sh
make check      # fmt --check, clippy, test, packaging-test
make man        # if you changed any CLI
```

`make help` lists every target — `make` on its own does the same. The
Makefile is a task runner over cargo and `scripts/`, not a build system:
building and installing go through cargo, as distro packaging does.
`make check` runs what a pull request should pass; the individual pieces
are `make fmt`, `make clippy`, `make test` and `make packaging-test`.

The packaging test re-runs the suite the way a packaging build does: no
external network, and the distro tools (`koji`, `fedrq`, `gbp`, `dput`,
…) replaced by failing stubs. Workspace coverage is expected to stay at
or above 80% line coverage, checked with `make cov` at release time.

## Commits

- Sign off every commit: `git commit -s`. This certifies the [Developer
  Certificate of Origin](https://developercertificate.org/).
- One logical change per commit.
- Subject line names the crate or tool it touches, then what it does:
  `fedora-cve-triage: skip advisories that name no version`.
- The body explains *why*, and any consequence a reader would otherwise
  have to discover — a changed error message, a behavior that is now
  refused, a caveat you could not resolve.

## AI-assisted contributions

You **may** use AI assistance. This project follows the [Fedora
Project's AI-Assisted Contributions
Policy](https://docs.fedoraproject.org/en-US/council/policy/ai-assisted-contributions/)
and the Linux kernel's [AI Coding
Assistants](https://docs.kernel.org/process/coding-assistants.html)
guidance, which come to the same three requirements.

**You are the author, and accountable.** Vouching for a contribution
means vouching for its quality, its license compliance and its
usefulness, whether you typed it or a model did. Review everything you
submit; "the model wrote it" is not an explanation for a defect.

**Disclose it with an `Assisted-by:` trailer.** Disclosure is required
when a significant part of the contribution came from a tool without
changes, and welcome otherwise where it would be useful to know.
Routine grammar, spelling and phrasing help needs no disclosure. The
format is the kernel's — agent, then the model actually used:

```
Assisted-by: Claude Code:claude-opus-5
```

Name the real model, not the family or a guess. Additional non-obvious
tools may follow, kernel style
(`Assisted-by: AGENT:MODEL [TOOL1] [TOOL2]`); everyday tooling like
git, cargo and your editor does not belong there.

Do **not** use `Co-Authored-By` for AI assistance. This project uses
`Assisted-by` exclusively — a model is a tool, not a co-author.

If you work with a coding agent, point it at [`AGENTS.md`](AGENTS.md)
(the same file as `.claude/CLAUDE.md`), which carries this project's
conventions in the form agents tend to read: commit and changelog
rules, code style, the offline-test requirement, and the layout of the
workspace.

**Only a human signs off.** An AI agent must not add a `Signed-off-by`
trailer: the DCO is a legal certification only a person can make. If you
direct an agent to commit on your behalf, the sign-off is yours and so
is the certification — so read the change before authorizing it, not
after. `AGENTS.md` tells agents in this repo to ask before every
commit for exactly this reason.

**AI does not decide.** A model may help review — analysis, suggestions,
spotting what a human missed — but must not be the sole or final arbiter
of whether a contribution is acceptable. Objective automated validation
(tests, linters, CI) is not affected by this.

## Licensing

The project is dual-licensed under Apache-2.0 OR MIT, and contributions
are accepted under the same terms. Don't paste code whose license you
cannot account for — including code a model reproduced from training
data — since accountability for license compliance sits with you.

## Maintainer notes

Release mechanics (version bumps, `cargo semver-checks`, publishing
order, tagging) live in [`AGENTS.md`](AGENTS.md), along with the
conventions this file summarizes. Per-tool design decisions live in each tool's
`DEVELOPMENT.md`; scheduled removals are tracked in `DEPRECATIONS.md`.
