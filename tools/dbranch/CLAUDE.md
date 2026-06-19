# dbranch — tool-specific guidelines

These apply in addition to the workspace `.claude/CLAUDE.md`.

## dbranch is a learning tool, not just an automator

dbranch automates a Debian/Ubuntu packaging workflow that a person
could do by hand. A core design goal is that it stays **transparent**:
a user should always be able to see, and learn, the exact commands it
runs so they can do it themselves later or sanity-check it.

- **Every externally-visible action must map to a real, copy-pasteable
  command.** Steps dbranch performs itself with no single shell
  equivalent (resolving the changelog conflict, normalizing the entry)
  are narrated by a plain-language heading. Do **not** add redundant
  "edit this file yourself" hints — dbranch is fixing it, and the
  heading already says what is happening.
- **`--explain` executes _and_ narrates, pausing for input.** Before
  running each command it prints the exact command, then waits for the
  user to press Enter (Ctrl-C aborts) — a step-through walkthrough for
  following along, learning, and sanity-checking. It is **not** a
  preview. A non-interactive stdin continues without blocking. After a
  step where dbranch edits a file itself (with no single shell
  equivalent — the changelog conflict/normalization, the gbp.conf /
  salsa-ci.yml tweaks), it runs `git diff` on that file and pauses, so
  the otherwise-opaque edit is still shown via a real command
  (`Ui::explain_diff`).
- **`--dry-run` narrates without executing.** It prints the same
  explanation + commands but runs nothing.
- The two are **orthogonal**: `--explain --dry-run` is a pure tutorial
  (full narration, no side effects).
- **Color-code the narration** for visibility (e.g. the explanation in
  one style, the command in another), but always respect `NO_COLOR`
  and non-terminal output (use `anstream`/`anstyle`, which handle this
  automatically). Never let color codes leak into piped output.

When adding a new subcommand or step, preserve this contract: if a
user runs it under `--explain`, the printed commands must be enough to
reproduce the result by hand.

## Workflow accuracy

The commands dbranch prints/runs must match the real Debian tooling
(`gbp`, `debuild`, `pbuilder-dist`, `git`). When the upstream tool's
behavior is version-dependent (e.g. `gbp dch --bpo`'s generated
version/message), prefer to **normalize the result deterministically**
afterward rather than depend on the exact upstream output — but still
show the real upstream command in the narration.

## External tools

dbranch shells out to `git`, `gbp`, `debuild`, and `pbuilder-dist`.
Per the workspace rule, check each is available before first use and
fail with a clear, actionable message (which package provides it) when
it is missing, rather than producing a confusing downstream error.
