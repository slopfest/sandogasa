<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# fedora-review-digest — development notes

Maintainer-facing notes on the design. User docs are in `README.md`.

## Don't replicate what fedora-review already does

This tool **summarizes** a `fedora-review` run for an auto-generated
spec — it does not re-implement the review. If fedora-review (or the
mock build it drives) already performs a check, **delegate to its
result; do not run the same test ourselves.**

- A failed check shows up in `review.json`'s `issues`; we surface those
  (the "Issues to address" section) and let them hold approval. That is
  the channel for anything fedora-review found wrong.
- A passed/absent check needs no independent verification from us.

Concrete example (the rule, learned the hard way): we briefly added a
`--install` flag that ran `mock install` on the built RPMs to verify
installability. But **fedora-review already runs the installability
check** as part of its mock build — a failure would already be an issue
we list. The extra code was redundant (and slow), so it was removed. The
"package builds and installs" item is inferred purely from local
signals: a binary RPM present in `results/` means the build (and thus
fedora-review's install step) succeeded.

When you're tempted to *run* a tool to check something, first ask
whether fedora-review's output already tells us. The things we *do*
compute ourselves are the ones fedora-review does **not** decide for a
generated spec:

- **crates.io latest version** — evidence for "latest version is
  packaged" (fedora-review doesn't compare against upstream).
- **license cross-check** — spec `License:` vs the crate's `Cargo.toml`
  `license`, reported as evidence.
- **the rust2rpm-specific suppressions/criteria** — e.g. dropping the
  benign "license file listed twice" duplicate, and adding the
  static-linked-deps check for binary crates.

Everything else is read straight out of the review directory.

## What the reviewer decides vs what we infer

Marks are *preselected* from the signals above, never asserted as final:
the reviewer confirms each (`+1/0/-1`) with the evidence shown inline,
or accepts the inferred set with `-y`. The tool stays a fast assistant,
not an oracle — the human remains accountable for the verdict.
