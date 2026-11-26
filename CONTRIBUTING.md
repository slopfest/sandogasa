<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# Contributing to sandogasa

Thanks for your interest. This is a workspace of cross-distribution
packaging tools — primarily Fedora, CentOS and Debian — with the
activity-tracking crates useful more broadly. Library crates live in
`crates/`, binaries in `tools/`. Contributions are welcome under the
terms below.

Issues and pull requests: <https://github.com/michel-slm/sandogasa>.

## Work from a specific issue

A contribution should fix or implement **one identified thing**. Open an
issue first (or comment on an existing one) describing the bug or the
feature, and reference it from the pull request: `Fixes #123` for a bug,
`Resolves #123` for a feature, `Refs #123` for work toward one that does
not finish it. The issue forms label a new issue as a bug or a feature;
a maintainer adds the label for the tools or crates it touches.

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
fine to send directly without an issue. A maintainer marks such a pull
request `no-issue` so the contribution check lets it through.

## Issues and labels

**Writing one.** The title says what should change: `cpu-sig-tracker
jira: look up a draft's CVEs together, sharing one NVD client`. The body
opens with the problem as someone using the tool meets it, with a
concrete case (the command, the package or CVE, what came back), and
only then the cause or a proposed fix. Say where it came from when that
helps the next reader: "Found in review of #236", "Found while
triaging …". The bug and feature forms ask for these.

**Labels.** Every issue carries:

- one kind: `bug` or `enhancement`, or `documentation` or
  `dependencies` when that is all it is. The forms set it; a maintainer
  corrects it if needed. It decides the closing word, `Fixes #N` for a
  bug and `Resolves #N` for an enhancement;
- a `tool::<name>` or `crate::<name>` label for each tool or library
  crate it touches, added by a maintainer.

**Milestones** say which release an issue is planned for. Three roll
forward: `Next patch`, `Next minor`, and `1.0` (the API work tracked in
#216). An issue gets at most one, when it is filed if it is planned at
all. At a release, the milestone it cuts is renamed to the version and
closed, so closed milestones list what each release shipped. An issue
that is not planned carries one label saying why instead: `blocked` (it
waits on something outside this repository, which it names),
`exploratory` (too large or undecided to plan yet) or `priority::low`
(nothing waits on it).

Contributors only need the kind, and the forms set it. The rest is the
maintainers' bookkeeping, done when an issue is filed or triaged;
[`AGENTS.md`](AGENTS.md) has their procedure, and
[`RELEASING.md`](RELEASING.md) the sweep before each release.

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
hand-edited: run `make man` and commit the result. A test per tool fails
if a page stops documenting a flag, so this is not optional.
Prose for the man page's DESCRIPTION belongs in a doc comment on the
tool's `Cli` struct, where it also becomes the long `--help` output.

**A CHANGELOG entry** for anything user-visible. When a change breaks
something, the entry's heading ends with `(breaking)` — or a narrower
form like `(breaking CLI)` / `(breaking JSON)` — and the body enumerates
what broke, with a migration hint.

Before opening a pull request, please also run:

```sh
make check      # fmt, clippy, tests, spelling, packaging test, and more
make cov-diff   # line coverage of the files you changed
make man        # if you changed any CLI
```

`make help` lists every target — `make` on its own does the same. The
Makefile is a task runner over cargo and `scripts/`, not a build system:
building and installing go through cargo, as distro packaging does.
`make check` runs what a pull request should pass; the individual pieces
are `make fmt`, `make clippy`, `make test` and `make packaging-test`.

The packaging test re-runs the suite the way a packaging build does: no
external network, and the distro tools (`koji`, `fedrq`, `gbp`, `dput`,
…) replaced by failing stubs. Line coverage must stay at or above 85%.
`make cov-diff` lists the coverage of each file your change touches
since it left this repository's `main` and marks any under 85%: add the
missing tests, or say in the pull request why a file can't reach it. In
a fork's clone, add this repository as a remote and fetch it
(`git remote add upstream https://github.com/michel-slm/sandogasa`),
so the script compares against its `main` rather than your fork's. `make cov` measures
the whole workspace, as the release does.

Project text uses en-US spelling, checked by `make spell`
([typos](https://github.com/crate-ci/typos)).

## Pull requests

- **Link the issue** in the description (`Fixes #123`, `Resolves #123`),
  as above. The template asks for it.
- **Leave "Allow edits by maintainers" checked.** Review here often ends
  with a maintainer adding a commit or two to your branch, rather than a
  round of requests, so the pull request can land with its follow-ups
  fixed. GitHub cannot offer the option for a fork owned by an
  organization, so the check below lets such a pull request through;
  forking to your own account instead makes the review quicker.
- **Expect a wait on your first pull request.** GitHub runs CI for a
  first-time contributor only once a maintainer approves it; later pull
  requests run at once.

A check named *Contribution checks* reports the first two: it fails
while maintainer edits are off, or while the description links no issue
and the pull request is not marked `no-issue`. Fix either and it passes
on the next push or edit of the description; re-running the old check
reads the pull request as it was.

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
workspace. Its first section says what applies to an agent working for
an outside contributor: much of the rest is the maintainer's release and
tracker procedure. An agent can open the issue, link it, and run the
checks above as well as you can; `Assisted-by` goes on every commit it
helped with.

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
order, tagging) live in [`RELEASING.md`](RELEASING.md), and the
conventions this file summarizes in [`AGENTS.md`](AGENTS.md). Per-tool design decisions live in each tool's
`DEVELOPMENT.md`; scheduled removals are tracked in `DEPRECATIONS.md`.
