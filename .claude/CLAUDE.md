# Project Guidelines

## Working for an outside contributor
Most of this file is the maintainer's procedure: releases, publishing, the issue sweep, milestones and labels. If you are an agent helping someone send a pull request from their fork, follow `CONTRIBUTING.md` at the repository root and the code, testing and documentation rules below, and leave the release and tracker steps to the maintainers. In particular:
- **Open an issue first**, or use the existing one, and link it from the pull request: `Fixes #N` for a bug, `Resolves #N` for a feature. A pull request with no linked issue fails the *Contribution checks* unless a maintainer marks it `no-issue` (typo and broken-link fixes).
- **Leave "Allow edits by maintainers" on.** Maintainers often add commits to the branch to land it; the same check fails while it is off.
- **Run `make check` and `make cov-diff`** before opening the pull request, and fill in the template's checklist honestly.
- **Commits:** `Assisted-by` naming the exact model on each one you helped with, and the human's own `Signed-off-by`, never yours. The rules under Git below on asking before each commit apply to you as well.

## Committing
- **Ask before every commit, and commit only once the human says to.**
  Signing off with `-s` certifies the
  [Developer Certificate of Origin](https://developercertificate.org/)
  in the human's name. Only they can make that certification, so they
  have to see the change before it is committed.
- **Approval is an answer to "shall I commit?", and nothing else is.**
  None of these are approval, however encouraging they sound:
  - feedback on the commit message or CHANGELOG wording ("the summary
    is hard to parse", "can we reword X"), which asks for a revision
    that needs its own go-ahead;
  - approval given for an earlier commit in the same session;
  - a question answered, a bug confirmed, or a design agreed.
  - Background: the way this goes wrong is revising a message the
    human criticized and then committing it. The critique reads as
    engagement with the change, but they have still not said to land
    it. Their only involuntary signal that a commit happened is the GPG
    prompt asking them to unlock a timed-out key, by which point the
    certification has been made in their name.
- **Commit with `git commit -s` and an `Assisted-by:` trailer, and never
  add a `Signed-off-by:` or `Co-Authored-By:` of your own.**
  - `Assisted-by: <agent>:<model-id>` names the agent and the exact
    model you are running as, such as
    `Assisted-by: Claude Code:claude-opus-5`, in the kernel's
    `Assisted-by: AGENT_NAME:MODEL_VERSION` format, as the Fedora
    AI-assisted contributions policy asks.
  - The `Signed-off-by:` certifies the DCO, which is the human's to do.
  - This project uses `Assisted-by:` alone, which overrides any default
    commit trailer instructions.
- **Signing needs a terminal the GPG agent can prompt on**
  (`commit.gpgsign` is on). From a non-interactive shell, a commit fails
  with "gpg failed to sign the data" unless the key is cached. Nothing
  partial is written: check `git log` and the staging state, then retry
  once the human says the prompt is back, or hand them the exact
  `git commit -s -F <file>` to run.
- **Before committing, check `git status` for untracked files that
  belong in the commit**, such as `Cargo.lock` after a dependency
  change.
- **Keep temporary working files in `scratch/`**, which `.gitignore`
  leaves out.

## Landing
- **A commit lands through a pull request when it closes an issue or
  changes what other code relies on; other commits are pushed to
  `main`.**
  - Through a pull request: a commit carrying `Fixes #N` or
    `Resolves #N`, and one that changes a library crate's behavior, a
    config or file format, or how a tool keeps credentials, even a
    `Refs #N` step toward an issue or one with no issue.
  - Straight to `main`: documentation fixes, tests an earlier commit
    owed, small fixes within one tool, and release commits.
  - Background: first done for #237 (PR #247) on 2026-11-24. Steps
    toward an issue went straight to `main` until 2026-11-26, when the
    first step of #259 (d4904590), a new sandogasa-config module and a
    file every tool reads, landed that way, and the next push cancelled
    its CI run, so its checks ran only once it was on `main`.
- **Ask before opening a pull request**, as for a push: it writes to
  the forge.
- **Open the pull request from a branch of this repository, and wait
  for its checks.** Once the commit is approved, push it to
  `fix/<N>-<topic>` and open a pull request from the template, with the
  issue linked by the commit's own word (`Refs #N` for a step; with no
  issue, mark the pull request `no-issue`). Its checks (CI, CodeQL,
  Spelling, Markdown and the Contribution checks) find a failure before
  it reaches `main`.
- **Each step of an issue gets a pull request of its own**, landed
  before the next step is built, so the human can try it in between.
- **Land a pull request by fast-forward when the human says to, then
  delete its branch:** `git push origin <branch>:main`. GitHub then
  marks the pull request merged and closes the issue, and the commit
  keeps its hash and signature, which none of the merge button's
  methods do ("Rebase and merge" rewrites the commits too).
- **A pull request landed by fast-forward needs no wait for the runs on
  `main`** before the task is called done, when all of these hold. Look
  at the runs when starting the next piece of work instead, and treat a
  failure as a flake to fix or file:
  - `main` got the pull request's head commit: the same full SHA as
    `gh pr view <n> --json headRefOid`;
  - every check in `gh pr checks <n>` passed, with none skipped and
    none older than the branch's last push;
  - the push to `main` runs the same workflows as the pull request did
    (CI, CodeQL, Spelling and Markdown, with the same path filters),
    and nothing that deploys, publishes or releases.
- **A commit pushed straight to `main`, and a release push, still get
  the full wait** for the runs the push starts.

## Commit messages, CHANGELOG and documentation
- **A commit subject says what changed.** That is what a reader
  scanning `git log`, release notes or `git blame` wants.
  - Examples:
    - "retire now offers to take the issue it closes"
    - "replace its hand-rolled XML-RPC with the shared Koji client"
    - "annotate the Copr script with why each package is in it"
  - A subject that describes the problem instead, such as "--copr
    generated a build script with no reason for any package", leaves
    the reader to work out what was done about it.
- **A commit body's first paragraph, and a CHANGELOG entry, open with
  the symptom as an outsider would meet it**: what the tool did or
  failed to do, ideally with a concrete observation, and only then the
  mechanism. Both must read for someone who has never seen this code.
  There is room there to describe the problem properly, which is why
  the subject does not have to.
- **Internal names and terms the code invents go further down**, in
  the paragraphs that explain the cause, where detail is welcome and
  depth is worth having. Internal names are functions, fields and
  struct members.
  - Tells that an entry is written for insiders:
    - the subject names a function or a field;
    - the first paragraph uses a phrase that only exists in the source
      ("the version of interest", "the recorded build");
    - the reader has to already know what a component does before the
      sentence parses.
  - Compare "check-wip missed builds that were already in Koji" against
    "a recorded build must not settle whether to look for a newer one":
    the second is accurate and means nothing to a stranger.
- **Never edit a released version's CHANGELOG entries.** Add
  significant changes to an `## Unreleased` section at the top of
  CHANGELOG.md, and rename it to the version number at release time.
- **A CHANGELOG entry for a breaking change ends its heading with
  `(breaking)`**, or `(breaking JSON)`, `(breaking config)` and so on
  when the surface is narrower, so consumers can grep them out of a
  release at a glance.
  - The body lists what broke, not just the new behavior: renamed or
    removed items, changed function signatures, altered JSON shape,
    dropped CLI flags or config keys.
  - Add a one-line migration hint when the path forward isn't obvious.
  - This applies whatever the size of the bump: a pre-1.0 minor bump
    may still be the right vehicle, but consumers shouldn't have to
    read the source diff to find out what they need to update.
- **A README describes the tool as it is now.** Keep out of it:
  rationale for recent changes, comparisons to how something used to
  behave, and arguments for why a default or a number is what it is.
  Those belong in `CHANGELOG.md` (what changed and why) and the tool's
  `DEVELOPMENT.md` (design decisions and rules future work must
  follow).
  - Tells that a README has drifted into justifying rather than
    describing: "used to", "previously", "which matters because", "is
    the point", "not something to do".
- **Don't open a README with the one narrow use case that prompted the
  work.** Describe the general problem the tool addresses, then let
  the specific cases follow: a tool that handles five kinds of
  misfiled CVE shouldn't read as if it only handles bundled JavaScript.
- **When a change lands, its reasoning goes in the CHANGELOG entry,
  any rule future work must follow in `DEVELOPMENT.md`, and only the
  resulting behavior in the README.** Documentation that survived a
  behavior change unedited is worse than none: check whether an
  existing section now describes something that is no longer true.
- **Keep rules here, and how-tos where human contributors read them.**
  A procedure, design note or reason a contributor also needs goes in
  DEVELOPMENT.md, RELEASING.md or CONTRIBUTING.md, and the rule here
  points to it instead of repeating it, so the two cannot drift apart.
- **Project text uses en-US spelling**: documentation, comments,
  messages, test names, commit messages, CHANGELOG entries and issue
  text (behavior, color, license, organization, summarize).
  - `make spell`, part of `make check` and its own CI workflow, runs
    typos-cli with the en-US locale, which reports en-GB spellings as
    well as typos.
  - Words that must keep another spelling, such as file names matched
    literally, a public identifier that waits for a breaking release,
    or a JSON field name, are listed in `_typos.toml` with the reason.
  - Released CHANGELOG entries are left as they are; the script checks
    the `Unreleased` section.

## Code style
- **Run `cargo fmt` before every commit.**
- **A commit introduces no `cargo clippy --workspace` warnings or
  errors**, and fixes any clippy issues in the code it touches.
  `make clippy` passes `-D warnings`, so a warning fails the gate
  rather than scrolling past.
  - Background: before that (2026-10-01), a complex-type warning rode
    along in a commit and was only noticed days later.
- **Every source file starts with
  `// SPDX-License-Identifier: Apache-2.0 OR MIT`.**
- **CLI help text (`-h` and `--help`) stays within 80 characters per
  line.**
- **A crate's `description` is 80 characters or fewer, with a few to
  spare.** debcargo turns it into the Debian package synopsis and
  refuses anything longer (`summary_check_len`: `<= 80`), leaving a
  FIXME for a packager to override by hand. The cut is exact, so a
  one-word reword at 79 trips it.
  - For a tool crate, the description is also clap's `about`, so
    changing one means `make man` and a regenerated page, or
    `man_page_matches_cli` fails the next gate.
  - Background: three of ours were found through that FIXME. Fedora
    never caught them: the spec carries one hand-written `Summary` for
    the whole workspace, so per-crate descriptions were only ever seen
    on crates.io.
- **Keep the `Command` enum variants in `main.rs` sorted
  alphabetically**: they set the order of the `--help` output.
- **Group a subcommand's flags by topic, and sort within a group**
  (issue #73), once it has more than six options, not counting `-h`.
  Six options or fewer stay one alphabetical list.
  - Each option goes under a `help_heading`, declared group by group
    and alphabetically by long name within each, so the source reads
    like `--help`. A group may lead with its one clearly main flag
    (dbranch's `--stage`).
  - Reuse the shared headings, and name others after the tool's own
    vocabulary (fedora-cve-triage's `Checks`):
    - `Selection`: what to work on;
    - `Writing`: `--apply`, `--dry-run`, `--yes`, `--claim`;
    - `Output`: `--json`, `--quiet`, `--explain`.
  - A flattened shared group (`sandogasa_cli::NotesArgs`) takes its
    heading with `next_help_heading`.
  - Positional arguments keep their typed order, and the global flags
    on `Cli` (`-v`, `--refresh`) get no heading:
    `sandogasa_cli::parse_with_defaults` lists them last under
    `Global options`.
  - `make man` turns each heading into a man page section.
- **Describe a tool's subcommands in its README.md in the same
  alphabetical order as the `Command` enum.** In the root README.md,
  list tools alphabetically, then library crates alphabetically.
- **Order definitions in a source file top-down, and review the order
  before committing.** Within each group below, define callees before
  callers, so a reader meets helpers before the functions that use
  them:
  - module docs and imports;
  - public types (structs, enums, traits);
  - public functions;
  - trait impls, grouped by type;
  - private helpers;
  - `#[cfg(test)] mod tests`.

## CLI behavior
- **A non-interactive subcommand (`show`, `search`) supports
  `--json`**, which prints pretty-printed, machine-readable JSON
  instead of human-readable text.
- **Each tool supports `--version`, and its `--help` header shows its
  name, version and short description** (the `Cargo.toml`
  `description`). DEVELOPMENT.md, "Every tool's `--version`, help
  header and man page", has the clap attribute and `build.rs` they
  need, and why the 80-column limit depends on clap's `wrap_help`
  feature.
- **Every tool ships a committed man page generated from its clap
  definition, and nobody edits a page by hand.** After changing any
  flag, subcommand, doc comment or `about` string, run `make man` and
  commit the result; a `man_page_matches_cli` test fails otherwise.
  The same DEVELOPMENT.md section has where the page's prose goes and
  how a new tool is wired.
- **Don't throw away the result of expensive computation.** When a
  tool detects a fixable problem mid-run (stale metadata, missing
  credentials or session, a transient failure), it offers to fix it on
  the user's behalf and continues with the state it already computed,
  rather than printing "here's how to fix it, then rerun" and
  discarding the work. It follows that:
  - cheap preconditions (external tools, auth sessions, output-path
    writability) are checked *before* expensive work starts, so
    failures surface in seconds, not minutes;
  - a long run that fails partway persists its partial state and can
    resume from it, retrying the failed step first, and replaces the
    final output only on success;
  - an interactive fix-it prompt defaults to the safe productive choice
    ("fix and continue" defaults to yes, "continue with bad data" to
    no), and never fires in `--json` mode or when stdin isn't a
    terminal, where runs keep warning and continuing, or failing with
    the remedy.
- **A field the tool can derive from evidence it already has is filled
  from that evidence, and an explicit value that disagrees is
  confirmed.**
  - When no flag sets it, the derived value fills it and the preview
    names where it came from.
  - An explicit value that disagrees with the evidence is confirmed at
    a terminal, defaulting to the evidence.
  - Under `--yes` or without a terminal, the explicit value is kept and
    the mismatch is reported, never silently replaced.
  - Examples:
    - `ebranch check-update --submit` offering `--type security` and
      the attached CVE trackers' highest severity (#76);
    - a Jira issue's priority taken from its CVE's severity.
- **Before filing somewhere the author cannot edit afterwards, preview
  the exact payload.** The preview shows every field and the full
  description as they will appear, not a summary, and filing needs a
  confirmation at a terminal.
  - `--yes` alone does not file into such a place: it also needs its
    own explicit flag, so a scripted run files only when it was told to
    file. The reasoning is the same as for `-y` alone not reassigning
    bugs.
  - Background: Red Hat's Jira accepts only comments once an issue
    exists, unlike Bugzilla, where the reporter can edit the
    description.
- **Offer dbranch's `--explain` where a person could do the steps by
  hand.** When a tool's steps map to commands a person could run
  (`git`, `gbp`, `cbs`, `koji`, `fedpkg`, `mock`), give it an opt-in
  `--explain` that narrates each step and shows the exact command
  before running it, pausing so the user can follow along. `--dry-run`
  shows the same steps without running anything, and a plain run just
  does the work.
  - This has been the default for such tools since dbranch: it is how
    someone learns the workflow, or checks the tool, and the printed
    commands must be enough to do it by hand (see
    `tools/dbranch/CLAUDE.md`).
  - Don't add it where the steps are API requests nobody would replay
    by hand, such as a JQL search or a JSON body sent to Jira or Bodhi:
    narrating those buries the decisions under transport. There,
    `--verbose` names each request in a line, and anything written is
    previewed in full (the rule above).
- **A tool that closes bugs also offers to reassign them
  (`assigned_to`) to the person running it.** Triaging is a benefit in
  itself, and whoever cleans up stale bugs may want the credit.
  - Bugzilla tools use `sandogasa_bugzilla::claim` (`resolve_claim` and
    `apply_claim`) instead of reimplementing the decision:
    - an explicit `--claim` flag claims without prompting;
    - `-y` without the flag declines, since non-interactive runs must
      not reassign unasked;
    - no configured email skips it silently;
    - otherwise it prompts.

## Workspace layout
- **Library crates go in `crates/`, and tools in `tools/`.** A tool
  crate may carry a `src/lib.rs` so its binary and tests share code;
  that library target is internal to the tool, not a public API
  (DEVELOPMENT.md, "A tool crate's library target is internal, not
  semver surface").
- **Every crate, library or tool, has its own README.md.**
- **hs-relmon and cpu-sig-tracker do the same job for different SIGs,
  so a feature added to one belongs in both.** They serve Hyperscale
  and Proposed Updates; converging them is #206.
  - Put the logic in sandogasa-cbs, with each tool a thin caller, as
    `review`, `notify` and `unbuilt` are.
  - When the other tool can't take it yet, file an issue for it there.
- **A public struct that derives `Deserialize` in a library crate is
  `#[non_exhaustive]`**, so a field the API grows is not a breaking
  release. `scripts/check-non-exhaustive.sh` (`make check`) enforces
  it, listing the exemptions: formats the workspace defines itself.
  Test fixtures for a model deserialize `serde_json::json!({…})`
  instead of using a literal. See DEVELOPMENT.md, "A response model is
  `#[non_exhaustive]`".
- **A tool's README.md has an Installation section** with
  `cargo install <crate>`, and any external tools it needs, such as
  `fedrq` or `koji`.
  - Mention `cargo binstall` only for a tool that ships prebuilt
    binaries, one carrying `[package.metadata.dist] dist = true`
    (DEVELOPMENT.md, "Only dbranch ships binaries"). Every other tool
    has no release artifact for binstall to find, so the command would
    send the reader to a 404.
- **Never hand-edit `.github/workflows/release.yml`**: cargo-dist
  generates it from `[workspace.metadata.dist]` in the root
  `Cargo.toml`, the way the man pages are generated from clap. After
  changing any dist config, run `dist generate` and commit the result.
  DEVELOPMENT.md, "Only dbranch ships binaries", has how to install
  cargo-dist and check what a release would produce.
- **Symlink the root LICENSE file into each crate's directory**, so it
  is included when the crate is published to crates.io.
- **Declare every dependency, external or internal, in
  `[workspace.dependencies]` in the root `Cargo.toml`**, and reference
  it as `{ workspace = true }` in member crates.
- **The root `Makefile` is a discoverable task runner over cargo and
  `scripts/`, not a build system.** `make help` lists everything,
  `make check` is the pre-PR gate, and `make release-checks` adds
  audit, semver and coverage.
  - Cargo stays the build system, since distro packaging drives it
    directly, so build logic stays out of the Makefile.
  - A new check goes in a script or a cargo alias, with a one-line
    target wrapping it so it shows up in `make help`.

## Files and external tools

### Per-user directories (XDG)
- **Our own per-user files follow the
  [XDG Base Directory spec](https://specifications.freedesktop.org/basedir/latest/),
  through the `dirs` crate.** Never read `$XDG_*` by hand (the spec
  requires ignoring relative values, which `dirs` does), and never
  hardcode `~/.config`, `~/.cache` or `~/.local/state`.
  - Config: `sandogasa_config::ConfigFile::for_tool`
    (`dirs::config_dir()`, so `$XDG_CONFIG_HOME`, by default
    `~/.config/<tool>/config.toml`, with modes 700 and 600). It is
    layered over an optional `/etc/<tool>/config.toml`: DEVELOPMENT.md,
    "Config files layer: /etc, then ~/.config, then the command line",
    has how, and why a `config` command loads with `load_user`.
  - Cache: `dirs::cache_dir()` (`$XDG_CACHE_HOME`, by default
    `~/.cache/<tool>/`).
  - State that persists between runs but isn't config, such as
    fesco-chair's saved agenda: `dirs::state_dir()` (`$XDG_STATE_HOME`,
    by default `~/.local/state/<tool>/`).
- **When no base directory can be found, neither from the `$XDG_*`
  variable nor from `$HOME`, fail loudly with a message naming both.**
  Never fall back to a literal `~/...` path: an unexpanded tilde
  creates `./~/...` in the current directory, without a warning.
- **Files that belong to external tools stay where that tool looks for
  them**, such as `~/.gbp.conf`, `~/pbuilder`, `~/.fedora.upn`, and
  bodhi-client's and debusine-client's config. Don't "fix" them to our
  conventions.

### External tool dependencies
- **A crate that shells out to an external tool (`fedrq`, `koji`)
  checks that it is available**, at startup or before its first use,
  and fails with a clear message if it is not, rather than silently
  returning empty results.

### fedrq in this workspace
General fedrq pitfalls (side tags, `@koji-src:`, `-F source`, `(none)`, `--forcearch`, EPEL 10 `@testing`) are in review-prompts, `rpm/dependencies.md`, "fedrq Pitfalls". What is specific to the tools here:
- **ebranch's resolve command queries `@koji-src:<tag>` for source RPMs
  on its own** when given `--source-repo @koji:<tag>`.
- **To list a side tag's own binaries without the inherited ones,
  resolve binary RPM names with `koji buildinfo <nvr>`**, not
  `fedrq pkgs -r @koji:<tag> '*'`.
- **For EPEL 10 and later, ebranch's `@testing` probe fails silently**,
  and falls back to side tag or reverse dependency listing.

## Dependencies and MSRV
- **Run `cargo audit` before starting feature work, and address any
  reported vulnerability first**, with a patch bump or
  `cargo update -p <crate> --precise <version>`.
- **For each advisory `cargo audit` reports, warnings included, read
  the advisory and the fixed release's changelog for the
  mitigation**, not just the version: a "patched" release may only
  deprecate the vulnerable API. RELEASING.md, "Advisories and
  dependency ranges", says how to check that nothing we ship calls it.
- **A security advisory whose fix isn't packaged in Fedora yet is
  decided by actual exposure, not the paper severity.** With real
  exposure, bump to the fixed version at once; when it is not
  exploitable for us, prefer a short-lived version range up to the
  fix. RELEASING.md, "Advisories and dependency ranges", has the full
  policy, and the re-test every release owes a range.
- **A dependency bump has to clear Fedora's and Debian's archives**
  (dbranch is headed for Debian). Before taking it, check that both
  carry the new version and that `rust-version` stays at or under
  Debian's rustc, and record the answers in TODO.md. DEVELOPMENT.md,
  "Dependency bumps clear Fedora and Debian first", has how to check
  each.
- **A routine `cargo update` (semver-compatible bumps) is a commit of
  its own**, separate from feature work, so it is easy to revert if
  something regresses.
- **After any dependency change, run
  `cargo clippy --workspace && cargo cov`** to check nothing broke.
- **The workspace declares its minimum supported Rust in
  `[workspace.package] rust-version`, and `make msrv-check` builds with
  exactly that toolchain.** Every member inherits it. `make msrv-check`,
  part of `make release-checks`, builds the whole workspace, tests
  included; install the toolchain once with
  `rustup toolchain install <msrv>`.
  - It is the only check that catches syntax newer than the MSRV:
    clippy's `incompatible_msrv` lint reads `rust-version` and flags std
    APIs, but a language feature such as an `if let` guard passes every
    lint and fails only under the old compiler.
  - Background: v0.24.1 shipped with one and failed to build for EPEL 9
    that way.
- **The MSRV is set by where the code has to build, not by the
  developer's toolchain**: Fedora rawhide, the released RHEL 9 and
  RHEL 10 minors, and Debian's rustc for dbranch. Before adopting a
  language feature or std API, look up the release that stabilized
  it. Raising `rust-version` above any of them is a deliberate
  decision, recorded in CHANGELOG.md and TODO.md. DEVELOPMENT.md, "The
  MSRV is 1.95, and EPEL waits for RHEL's rust to catch up", has how
  to read each.

## Performance
- **Batch or parallelize remote work from the first version, not after
  it proves slow.** Before writing a loop that calls a remote service
  or spawns a CLI once per item (build, RPM, bug, package, CVE), look
  for a batch form and use it. Where none exists, run the calls
  concurrently with a bound.
  - Batch forms here:
    - Koji's `multiCall` through `sandogasa-kojihub`, with existing
      batched calls such as `builds_rpm_nvras`, `rpms_deps` and
      `latest_builds`; add another the same way;
    - Bugzilla's id list (`BzClient::bugs`);
    - Bodhi's multi-build queries (`updates_for_builds`);
    - one fedrq query over many names.
  - A `koji` subprocess costs about a second of Python startup before
    it asks anything, so one per item is the slowest way to get an
    answer.
  - Background, 2026-11-10: ebranch check-update ran one `koji`
    subprocess per subpackage (59 s for asterisk, 1.8 s batched, #111).
- **Reusing a helper reuses its cost.** Before calling existing code
  from a new loop or over a bigger input, check whether it does one
  remote call per item; if it does, batch it as part of the change.
  - Background, 2026-11-10: `update-bugs` reused a per-build helper
    over 39 builds (4 minutes, #114).
- **Anything slow enough to notice says what it is doing**, in one line
  without `-v` and per request with it, so a long step never reads as
  a hang.
- **The per-issue `/code-review` looks for these, the same as for
  correctness**: a per-item remote call in a loop is a finding.

## Testing and coverage
- **Write tests with every feature added or changed.**
- **Tests must not reach the external network**: `cargo test` has to
  pass in a distro packaging sandbox (Koji, Debian buildds), where
  external network is blocked but loopback is up. Use a `wiremock`
  server on `127.0.0.1` for HTTP clients, and don't depend on domain
  tools such as `gbp`, `koji` or `fedrq`. DEVELOPMENT.md, "Tests run
  offline, the way a packaging build runs them", has how to fake each
  kind of dependency, and `scripts/packaging-test.sh` reruns the tests
  that way.
- **Tests must not assume the host's architecture.** Fedora runs
  `%check` on s390x, ppc64le and i686 too, which CI never sees. Derive
  a foreign architecture from the host's, and compare architectures
  only through `sandogasa_cbs::builds::host_rpm_arch`; the same
  DEVELOPMENT.md section has why.
- **Per commit, run `cargo fmt` and `cargo clippy --workspace`**, and
  `cargo test` for code you touched. Fast prototyping commits don't
  need full coverage checks.
- **Run `cargo cov` at stability points: before tagging, and when
  catching a feature up for its README or CHANGELOG entry.** Line
  coverage must stay at or above 85%, a gate ratcheted up as coverage
  grows with a little headroom left (DEVELOPMENT.md, "The coverage
  gate, and how it got to 85%"). Binary `src/main.rs` files are
  excluded from the measurement (`.cargo/config.toml`).
- **Check coverage of the files a change touches when it closes an issue**, in the same sitting as the per-issue `/code-review`: `make cov-diff` lists the line coverage of each changed file against `origin/main` (`BASE=<ref>` for another), marking any under the 85% gate; `make cov-crate CRATE=<name>` gives a whole crate. A file the change leaves below the gate gets the missing tests in that commit, or a stated reason it can't (code that only drives an external tool with no seam to fake it). Reviews check correctness, not coverage: v0.28.0's release gate came up about 950 lines short, all in code that had gone through an issue's commit

## Issues and releases
- **Label every issue when filing it** (CONTRIBUTING.md, "Issues and labels", is the contributor-facing summary of this and the milestone rule below; keep the two in step): one kind (`bug` or `enhancement`; `documentation` or `dependencies` when that is all it is) and a `tool::<name>` or `crate::<name>` label for each tool or library crate it touches. A new tool or crate gets its label when it lands. The kind matches the closing commit's word, `Fixes #N` for a bug and `Resolves #N` for an enhancement. The issue sweep also looks for unlabeled ones (`gh issue list --state all --json number,labels -q '[.[]|select(.labels|length==0)|.number]'`): on 2026-11-10, 35 issues from #1 to #123 had none
- **Give a new issue a milestone when it is filed** (`Next patch`, `Next minor` or `1.0`), if it is planned for a release; leave it off otherwise, and give it one of `blocked`, `exploratory` or `priority::low` saying why. An issue has at most one milestone. What happens to them at release time is in `RELEASING.md`
- **When a commit implements or obsoletes a tracked issue that its own `Fixes #N` or `Resolves #N` does not close, close the issue in the same sitting**, naming the commit and the first release tag that shipped it (`git tag --contains <sha> -l 'v*' | sort -V | head -1`; "not released yet" before there is one), rather than leaving it for the next sweep. An issue closed by a landed commit's own keyword needs no comment: the commit and its pull request are already on the issue's timeline
- **Releases follow `RELEASING.md`** at the repository root: the sweeps, gates, publishing, tagging and release-asset checks. Read it in full before cutting one
