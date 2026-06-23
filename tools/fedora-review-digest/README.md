<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# fedora-review-digest

Condense a `fedora-review` run of an **auto-generated** spec into a short
review comment.

A spec produced by a generator — `rust2rpm` for Rust crates (and, later,
`pyp2spec` for Python packages) — is mechanically correct by
construction, so most of `fedora-review`'s template is not
decision-relevant. Pasting the whole thing into Bugzilla buries the few
points that actually matter. `fedora-review-digest` reads a finished
`fedora-review` result and emits just the rust-sig-style summary: a short
checklist with a per-item verdict, the issues that genuinely need
attention, and the post-import task boilerplate.

It is an **assistant, not an oracle**: each checklist mark is
*preselected* from the evidence, but you confirm (or change) it before
the comment is produced — the human stays accountable for the verdict.

## Installation

```
cargo install fedora-review-digest
```

External tools:

- [`fedora-review`](https://pagure.io/FedoraReview) — you run it first
  (`fedora-review -b <bug>`) to produce the result directory this tool
  reads. (Running it for you is a planned follow-up.)
- `curl` — for the crates.io latest-version check; skip it with
  `--no-net`.

## Usage

```
fedora-review-digest <DIR-OR-BUGID> [--comment <TEXT>] [-y]
    [--reviews-dir <DIR>] [--no-net]
```

Point it at a finished review — either the directory `fedora-review`
created, or the bug id (resolved to `<id>-*` under `--reviews-dir`,
default the current directory):

```
$ fedora-review -b 2489102                       # produces 2489102-rust-trustfall_core/
$ fedora-review-digest 2489102-rust-trustfall_core
$ fedora-review-digest 2489102                    # same, by bug id
```

By default it walks the checklist interactively: it prints the full
inferred review first, then asks you to confirm each item — `+1` pass /
`0` caveat / `-1` fail, with the evidence shown inline and Enter
accepting the inferred mark. A caveat/fail prompts for the parenthetical
note (Enter keeps the suggested wording, or type your own). You're then
asked for an optional free-form comment for the top.

- `--comment <TEXT>` supplies that top comment non-interactively.
- `-y`/`--yes` accepts every inferred mark without prompting (and skips
  the comment prompt — pair with `--comment`). Required when stdin isn't
  a terminal.
- `--no-net` skips the crates.io check.
- `--reviews-dir <DIR>` is where a bare bug id is resolved.

The finished comment is written to **stdout**, ready to paste into
Bugzilla.

## What it generates

Three `===`-separated blocks, matching the rust-sig convention:

```
<your free-form comment, if any>

===

Package was generated with rust2rpm, simplifying the review.

✅ package contains only permissible content
✅ package builds and installs without errors on rawhide
✅ test suite is run and all unit tests pass
✅ latest version of the crate is packaged (spec & crates.io: 0.8.1)
✅ license matches upstream specification and is acceptable for Fedora (spec & Cargo.toml: Apache-2.0)
✅ license file is included with %license in %files
✅ package complies with Rust Packaging Guidelines

Package APPROVED.

===

Recommended post-import rust-sig tasks:
- set up package on release-monitoring.org: …
- add @rust-sig as co-maintainer …
- …
```

## How the marks are inferred

The tool reads the review directory (`review.json`, the spec, the unpacked
RPMs) and runs two live checks it owns; everything `fedora-review`
already decides is read from its output, never re-run.

- **builds and installs** — ✅ when a binary RPM is present in
  `results/`. `fedora-review` runs the installability check itself; a
  failure is surfaced as an issue, so it isn't re-tested here.
- **test suite** — ✅ when `%check` runs `%cargo_test` with no `--skip`;
  🫤 when some tests are skipped, or the suite is disabled (`%bcond
  check 0`).
- **latest version** — compares the spec `Version:` with the latest
  stable on **crates.io** (`spec & crates.io: <v>`); 🫤 when a newer
  version exists.
- **license matches** — reports both the spec `License:` and the crate's
  `Cargo.toml` `license`; 🫤 on a mismatch to reconcile.
- **license file** — ✅ when the crate ships it and it's `%license`'d;
  🫤 `included manually[, fix submitted to upstream]` when it's added as
  a `Source` (the "fix submitted" part only when a `…/pull/…` comment
  precedes that `Source`); 🫤 when no `%license` at all.
- **statically linked deps** — for a crate that ships a binary
  (`%{_bindir}`), an extra item checks that its statically linked
  dependencies' licenses are folded into `License:`.
- **issues** — `fedora-review`'s MUST failures are listed and hold
  approval, except the benign "File listed twice" that every rust2rpm
  package trips: a file under the crate instdir
  (`…/usr/share/cargo/registry/<crate>/`) that's also `%doc`/`%license`
  (LICENSE, README, …) is listed twice by design.

A package is **APPROVED** when nothing failed and there are no
outstanding issues; caveats (🫤) don't block.

## Scope

`rust2rpm` (Rust) today. `pyp2spec` (Python packages from PyPI) reuses
the same machinery with its own checklist and boilerplate — planned. The
generator is detected from the spec's `# Generated by …` marker; a
hand-written spec is rejected (there's nothing to simplify).
