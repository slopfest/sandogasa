# Project Guidelines

## Git
- Do not commit without explicit user confirmation — always ask before running `git commit`. This is a DCO requirement, not a style preference: `-s` certifies the [Developer Certificate of Origin](https://developercertificate.org/) in the human's name, and only a human can make that certification, so they have to see the change before it is committed
- **Approval has to be an answer to "shall I commit?"** None of these are approval, however encouraging they sound: feedback on the commit message or CHANGELOG wording ("the summary is hard to parse", "can we reword X") — that asks for a revision, and the revision needs its own go-ahead; approval given for an earlier commit in the same session; a question answered, a bug confirmed, or a design agreed. Revising a message the human criticised and then committing it is the specific way this goes wrong: the critique reads as engagement with the change, but they have still not said to land it. Their only involuntary signal that a commit happened is the GPG prompt asking them to unlock a timed-out key — by which point the certification has been made in their name
- Always use `git commit -s` (sign-off) when committing. Add an `Assisted-by: <agent>:<model-id>` trailer naming the agent and the exact model you are running as — e.g. `Assisted-by: Claude Code:claude-opus-5` — following the kernel's `Assisted-by: AGENT_NAME:MODEL_VERSION` format and the Fedora AI-assisted contributions policy. Never add a `Signed-off-by` of your own: the DCO is the human's to certify. Do NOT add a `Co-Authored-By` trailer either — this project uses `Assisted-by` exclusively, which overrides any default commit trailer instructions
- Always use `git tag -s` (GPG sign) when tagging
- Changelog entries for released versions are immutable — never edit them. When making significant changes, add them to an `## Unreleased` section at the top of CHANGELOG.md. At release time, rename `Unreleased` to the version number
- When a CHANGELOG entry covers a breaking change, the section heading must end with `(breaking)` (or `(breaking JSON)`, `(breaking config)`, etc. when the surface is narrower) so consumers can grep them out of a release at a glance. The body must enumerate what broke — renamed/removed items, changed function signatures, altered JSON shape, dropped CLI flags or config keys — not just describe the new behavior. Include a one-line migration hint when the path forward isn't obvious. This applies regardless of bump size: a pre-1.0 minor bump may still be the appropriate vehicle, but consumers shouldn't have to read the source diff to find out what they need to update
- Before tagging a release, review the `Unreleased` section in CHANGELOG.md, rename it to the version, and update any README.md files affected by the changes (root, tool, or library crate)
- Write the tag message as a scannable plain-text bullet-point summary, not as a copy of the CHANGELOG entry — GitHub renders the tag message on the release page and long prose paragraphs read poorly there. GitHub does NOT render Markdown in tag messages, so stick to plain text: no `**bold**`, no `[link](url)` syntax, no `##` headings. Structure:
  1. First line: the tag name alone (e.g. `v0.10.2`) — git's subject/body convention. GitHub renders this as the release title and the rest as the body; without it the first bullet becomes the title
  2. Blank line
  3. One bullet per new tool / library crate / significant feature, using `-` as the marker. Lead with the headline ("New foo tool"), em-dash, then a one-line description and optionally a short inline list of key subcommands
  4. Blank line, then a plain URL linking back to the anchored CHANGELOG entry at the tag version (e.g. `Full details:\nhttps://github.com/slopfest/sandogasa/blob/vX.Y.Z/CHANGELOG.md#vxyz`)
- Pass the message via `git tag -s -F <file>` with a pre-written file so the formatting survives — avoid lines starting with `#` (git strips them as comments unless you add `--cleanup=whitespace`)
- Before tagging, verify there are no uncommitted changes (`git status` must be clean)
- Before tagging, if any workspace dependency uses a version range, re-test both the floor and the ceiling (see the Dependencies section's range policy)
- **Regenerate the man pages after bumping the version, not before** — each page's `.TH` line carries `sandogasa <version>` (the footer, as `man bash` shows "GNU Bash 5.3"), so the pages must be rewritten once the new version is in `Cargo.toml`. The `man_page_matches_cli` tests fail while a page still names the old version, so a forgotten regeneration is a test failure rather than a stale footer shipped to users. Order at release time: bump versions → `make man` → run the test suite → commit → tag
- Before bumping versions, run `cargo semver-checks` on each library crate to determine the correct version bump. Pre-1.0: patch bump is fine for anything non-breaking (including new tools, new crates, and new public items); bump the minor (e.g. 0.10.x → 0.11.0) only for actual breaking changes. Post-1.0: follow strict semver — new public surface (new crates, new tools, new modules, new items) requires at least a minor bump; breaking changes require a major bump
- Before tagging, publish all crates to crates.io with `cargo ws publish --publish-as-is --publish-interval 30 --no-git-commit --allow-dirty` (from [cargo-workspaces](https://github.com/pksunkara/cargo-workspaces); install via `cargo install cargo-workspaces`). `--publish-as-is` uses existing `Cargo.toml` versions (no bump, no prompt); it automatically skips crates already on crates.io at that version, so re-running after a partial failure is safe. `--publish-interval 30` throttles to stay under crates.io rate limits; `--no-git-commit` skips the version-bump commit cargo-workspaces would otherwise make; `--allow-dirty` tolerates uncommitted `Cargo.lock` edits. Raw `cargo publish --workspace` also works for the initial attempt but fails hard on the first already-published crate, so don't use it for retries
  - **Picking `--publish-interval` (compute it; the workspace is past the burst).** crates.io's defaults: new *versions* of existing crates get a burst of **30** refilling **1/min**; brand-new crate *names* get a burst of **5** refilling **1 per 10 min** (source: crates.io `rate_limiter.rs`). The binding constraint is the version-bump burst of 30. The bucket refills to full after ~30 min idle, so on a normal release it starts at 30. If the number of crates getting a version bump (`N`) is ≤ 30, the interval can be near zero; once `N` exceeds 30 you need roughly `interval ≥ 60 × (N − 30) / N` seconds so the tail doesn't 429. A 429 is **not fatal** — re-run the same command and `--publish-as-is` skips the already-published crates, waiting out the ~1/min refill.
    - The workspace now bumps **every** crate on a release (they share `version.workspace = true`), so `N` is the full member count and is **well past 30** — the interval can no longer be driven to zero no matter how idle the bucket is, and there is a hard floor of about `N − 30` minutes on the run regardless of what you pass. Compute the interval each time rather than reusing a number: `N` grows with every new crate.
    - Worked examples: **v0.15.2** — ~36 crates, ~34 version bumps + 2 new → floor ≈ 60×4/34 ≈ 7s, used `10` (~6 min). **v0.19.1** — 41 crates all bumping → floor ≈ 60×11/41 ≈ 16s, used `20` (~14 min), no 429s. The `30` in the command above is a conservative fallback, not the right answer for a workspace this size (~21 min for 41 crates)
- **After publishing, verify every crate actually landed — before tagging.** Run `make check-published` (`scripts/check-published.sh`, optionally with a version argument). `cargo ws publish` can stop partway on a 429, an interrupted upload, or a crate whose packaging fails, and a tag pointing at a half-published workspace is worse than a tag that arrives late. The script lists what is missing and prints the re-run command; `--publish-as-is` skips whatever already landed. Note that **crates.io requires a User-Agent** under its [API data access policy](https://crates.io/data-access) and answers `200` with an error body when one is absent — so an ad-hoc `curl` check without a UA reports every crate as missing, which looks exactly like a failed publish. Use the script rather than hand-rolling the query
- **Running the release gates: never pipe a gate whose verdict is its exit status.** `cargo semver-checks`, `cargo audit`, `cargo test`, `scripts/check-published.sh` and friends answer with an exit code, and `cmd | tail` reports the *pipe's* status — so a failing gate reads as a pass. Redirect to a log and record the status explicitly (`cargo semver-checks --workspace > /tmp/semver.log 2>&1; echo "EXIT=$?"`), then grep the log. Piping is fine only when the status is also captured (`${PIPESTATUS[0]}`)
- **Check free disk before starting, and budget tens of gigabytes.** The gates are what fill `target/`, not day-to-day work: `cargo clippy --workspace --all-targets` and `cargo test --workspace` build every test binary and dev-dependency for ~41 crates, `cargo cov` writes a separately instrumented copy of the whole workspace, and `cargo semver-checks` builds rustdoc for the current *and* baseline version of every library crate. On 2026-08-26 that took a 237G disk to under 1G free, and the low-disk warning was noticed by luck while `cargo ws publish` was already in flight — running out mid-publish would have left the workspace half-published. Run `df -h .` first; if the margin is thin, clear space *before* the gates rather than during them
- **`make sweep` will not save you from this** — it deliberately keeps `target/debug` (see the comment in `scripts/sweep.sh`), which is the tree the gates inflate, and it only becomes useful *after* publishing, when the semver-checks baseline it clears has been invalidated. When `target/debug` itself has to go, `cargo clean` is the honest tool and reports what it removed; the script's "1.3GB" figure for it describes a plain dev build, not the state after a full gate run
- **Start the slow gates in the background, not by letting them time out into it.** `cargo semver-checks --workspace` takes well over 10 minutes on this workspace (it builds rustdoc for both the current and the baseline version of every library crate); coverage and a full `cargo test --workspace` are minutes each. A foreground call that exceeds its timeout is moved to the background, but everything it printed *before* the move is lost, so a run that was 90% done reports on its last two crates. Launch those with `run_in_background` from the start
- **Do not mutate `Cargo.toml` while a background gate is running.** Bumping the version under a running `cargo semver-checks` invalidates it — the baseline it is resolving disappears (`package ID specification sandogasa-x@<old> did not match any packages`) and the whole run has to be repeated. Order: audit and semver-checks first, *then* bump, then man pages, then the test/clippy/fmt/coverage gates
- **Publishing rewrites `Cargo.lock`** — packaging each crate resolves it without the workspace's dev-dependencies, and `--allow-dirty` lets that be written back, so `git status` afterwards shows a lockfile with entries like `tempfile` and `wiremock` removed. Discard it (`git checkout Cargo.lock`) and confirm a plain `cargo build --workspace` leaves the tree clean; do not commit the stripped version
- After publishing and tagging, push with `git push --follow-tags`
- **Pushing the tag starts the prebuilt-binary workflow, which needs a draft GitHub release waiting for it.** `.github/workflows/release.yml` (generated by cargo-dist; see `[workspace.metadata.dist]` in the root `Cargo.toml`) builds dbranch for x86_64 and aarch64 musl and uploads the archives, then undrafts the release. It is configured `create-release = false`, so it uploads into a release that already exists rather than writing its own title and body — which is what keeps the tag-message convention above on the release page. So right after `git push --follow-tags`, create the release as a draft from the same message file: `gh release create <tag> --draft --title <tag> --notes-file <file>`. The build takes minutes and the upload job runs last, so the draft only has to exist before that job reaches it. If it is missing the upload fails; create it and re-run the job
- **Give Actions a few minutes before concluding the tag push did not fire the workflow, and never delete a pushed tag on that suspicion.** On the v0.22.0 release the run was absent from `gh run list` and from `actions/workflows/<id>/runs` (`total_count: 0`) for a good minute after the push, which read as a trigger that had not matched — the tag was deleted and re-pushed to force a fresh event, and the run then appeared, very possibly the one the *first* push had already queued. Registration simply lags. Before touching the tag, confirm the real causes: that the pattern matches (`git ls-tree -r <tag> --name-only | grep .github` proves the workflow exists at the tagged commit), that Actions is enabled (`gh api repos/<o>/<r>/actions/permissions`), and that the workflow is `active`. If all three hold, wait
- **Then check the binaries landed, the way `make check-published` checks the crates.** `gh release view <tag>` should list four assets — `dbranch-x86_64-unknown-linux-musl.tar.xz` and the aarch64 one, each with a `.sha256` — and the release should no longer say Draft. A half-uploaded release is the same failure as a half-published workspace, and it is silent: `cargo binstall dbranch` simply falls back to building from source, so nobody reports it. Smoke-test that command afterwards, since it is the thing the release exists to make work
- **After pushing, run `make sweep`.** A just-published version makes the baseline `cargo semver-checks` cached useless, so the gate trees (19GB and 6.5GB at 0.19.3) are provably disposable at exactly that moment. It leaves `target/debug` alone and costs nothing — the next build is still incremental. See DEVELOPMENT.md, "The release gates fill target/, not day-to-day builds", for when *not* to run it
- Before committing, check `git status` for untracked files that should be staged (e.g. `Cargo.lock` after dependency changes). Use `scratch/` for temporary working files — it is in `.gitignore`

## Code Style
- Always run `cargo fmt` before committing
- Commits must not introduce `cargo clippy --workspace` warnings or errors. Fix any clippy issues in code you touch
- Every source file must start with `// SPDX-License-Identifier: Apache-2.0 OR MIT`
- CLI help text (`-h` and `--help`) must not exceed 80 characters per line
- Keep the `Command` enum variants in `main.rs` sorted alphabetically (this determines the order in `--help` output)
- In each tool's README.md, describe subcommands in the same alphabetical order as the `Command` enum. In the root README.md, list tools alphabetically, then library crates alphabetically
- Order definitions in source files top-down: module docs and imports, public types (structs/enums/traits), public functions, trait impls (grouped by type), private helpers, `#[cfg(test)] mod tests`. Within each group, define callees before callers so a reader encounters helpers before the functions that use them. Review file order before committing

- Any tool that closes bugs must also offer to reassign them
  (`assigned_to`) to the person running the command — triaging is a
  benefit in itself, and the person cleaning up stale bugs may want
  the credit. For Bugzilla tools, use `sandogasa_bugzilla::claim`
  (`resolve_claim` + `apply_claim`) instead of reimplementing the
  decision matrix: an explicit `--claim` flag claims without
  prompting, `-y` without the flag declines (non-interactive runs
  must not reassign unasked), no configured email skips silently,
  otherwise prompt interactively

## Per-user directories (XDG)
- Our own per-user storage follows the [XDG Base Directory spec](https://specifications.freedesktop.org/basedir/latest/), resolved via the `dirs` crate — never hand-roll `$XDG_*` env reading (the spec requires *ignoring relative values*, which `dirs` implements) and never hardcode `~/.config`/`~/.cache`/`~/.local/state`:
  - config → `sandogasa_config::ConfigFile::for_tool` (`dirs::config_dir()`, i.e. `$XDG_CONFIG_HOME`, default `~/.config/<tool>/config.toml`, perms 700/600). Reads are layered: an optional system-wide `/etc/<tool>/config.toml` is merged beneath the user file (user wins per key, recursively; CLI flags override both); `save` only writes the user file
  - cache → `dirs::cache_dir()` (`$XDG_CACHE_HOME`, default `~/.cache/<tool>/`)
  - state that persists between runs but isn't config (e.g. fesco-chair's saved agenda) → `dirs::state_dir()` (`$XDG_STATE_HOME`, default `~/.local/state/<tool>/`)
- When no base directory can be determined (neither the `$XDG_*` var nor `$HOME`), fail loudly with a message naming both variables — never fall back to a literal `~/...` path (an unexpanded tilde silently creates `./~/...` in the CWD)
- Files owned by *external* tools (`~/.gbp.conf`, `~/pbuilder`, `~/.fedora.upn`, bodhi-client's / debusine-client's config) must match wherever that tool actually looks — do not "fix" them to our conventions

## External tool dependencies
- When a crate shells out to an external tool (e.g. `fedrq`, `koji`), it must check that the tool is available at startup (or before first use) and produce a clear error message if not found, rather than silently failing with empty results

## CLI behavior
- Non-interactive subcommands (e.g. `show`, `search`) must support a `--json` flag that outputs pretty-printed, machine-readable JSON instead of human-readable text
- Each tool must support `--version` and display its name, version, and short description (matching `Cargo.toml` `description`) in the `--help` header. In clap, use `#[command(version, about, long_about = None, max_term_width = 80, before_help = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")))]`. The `max_term_width = 80` is what enforces the 80-column help rule — it needs clap's `wrap_help` cargo feature (set in the workspace `Cargo.toml`), without which clap compiles out all wrapping and the attribute is silently inert. Clap never wraps the `Usage:` line itself, so a subcommand with several long positional value names can still exceed 80 — keep value names short (`PATH`, not `OUTPUT_FILE_PATH`)
- Every tool ships a committed man page at `tools/<tool>/man/<tool>.1`, generated from its clap definition by `sandogasa_cli::man` — one page per tool, with each subcommand as a `.SS` subsection. **Never hand-edit a page**: after changing any flag, subcommand, doc comment or `about` string, run `make man` (`scripts/gen-man.sh`) and commit the result. A `man_page_matches_cli` test in each tool's `main.rs` fails when a page stops documenting a visible flag or subcommand, so the pages cannot silently drift from `--help`. Prose that belongs in the man page (a real DESCRIPTION, not the one-line `about`) goes in a doc comment on the tool's `Cli` struct, where it also becomes `--help`'s long output — never in the roff. A new tool must be wired the same way: `sandogasa-cli = { workspace = true, features = ["man"] }` under `[dev-dependencies]`, plus the test
- Don't throw away the result of expensive computation. When a tool
  detects a fixable problem mid-run (stale metadata, missing
  credentials/session, a transient failure), it should offer to fix
  it on the user's behalf and continue with the already-computed
  state — not print "here's how to fix it, then rerun" and discard
  the work. Corollaries:
  - Validate preconditions that are cheap to check (external tools,
    auth sessions, output-path writability) *before* starting
    expensive work, so failures surface in seconds, not minutes
  - When a long run does fail partway, persist partial state and
    support resuming from it (retrying the failed step first),
    replacing the final output only on success
  - Interactive fix-it prompts should default to the safe productive
    choice (e.g. "fix and continue" default yes; "continue with bad
    data" default no), and must not fire in `--json` mode or when
    stdin isn't a terminal — non-interactive runs keep the
    warn-and-continue or fail-with-remedy behavior


## Workspace layout
- Library crates go in `crates/`, binary crates go in `tools/`
- Each crate (library or tool) must have its own README.md
- Tool README.md files must include an Installation section with `cargo install <crate>` (and any required external tools like `fedrq` or `koji`). Mention `cargo binstall` **only** for a tool that actually ships prebuilt binaries — that is, one carrying `[package.metadata.dist] dist = true`, which is `dbranch` alone today. Every other tool has no release artefact for binstall to find, so offering the command would send the reader to a 404
- **Never hand-edit `.github/workflows/release.yml`** — it is generated from `[workspace.metadata.dist]` in the root `Cargo.toml` by cargo-dist, the way the man pages are generated from clap. After changing any dist config, run `dist generate` and commit the result (cargo-dist installs in seconds with `cargo binstall cargo-dist`, which is also a fair demonstration of what this ships); `dist plan` shows what a release would produce and `dist build --artifacts=local --target=<triple>` builds one for real
- Symlink the root LICENSE file into each crate subdirectory so it is included when publishing to crates.io
- All dependencies (external and internal) are declared in `[workspace.dependencies]` in the root `Cargo.toml`, then referenced as `{ workspace = true }` in member crates
- The root `Makefile` is a discoverable task runner over cargo and `scripts/` — `make help` lists everything, `make check` is the pre-PR gate, `make release-checks` adds audit/semver/coverage. Cargo stays the build system (distro packaging drives it directly), so don't put build logic in the Makefile: a new check belongs in a script or a cargo alias, with a one-line target wrapping it so it shows up in `make help`

## Documentation
- **A commit subject says what changed. A commit body and a CHANGELOG entry
  open with the symptom.** Both must read for someone who has never seen this
  code, but they answer different questions:
  - The **subject** names the change, because that is what a reader scanning
    `git log`, release notes or `git blame` wants: "retire now offers to take
    the issue it closes", "replace its hand-rolled XML-RPC with the shared Koji
    client", "annotate the Copr script with why each package is in it". A
    subject that describes the problem instead — "--copr generated a build
    script with no reason for any package" — leaves them to work out what was
    done about it.
  - The **body's first paragraph, and a CHANGELOG entry's**, lead with the
    symptom as an outsider would meet it: what the tool did or failed to do,
    ideally with a concrete observation, and only then the mechanism. There is
    room there to describe the problem properly, which is why the subject does
    not have to.

  Internal names (functions, fields, struct members) and terms the code invents
  belong further down, in the paragraphs that explain the cause; that is where
  detail is welcome and depth is worth having. Tells that an entry is written for
  insiders: the subject names a function or a field; the first paragraph uses a
  phrase that only exists in the source ("the version of interest", "the recorded
  build"); the reader has to already know what a component does before the
  sentence parses. Compare "check-wip missed builds that were already in Koji"
  against "a recorded build must not settle whether to look for a newer one" —
  the second is accurate and means nothing to a stranger
- A README describes the tool **as it is now**. Keep out of it: rationale for
  recent changes, comparisons to how something used to behave, and arguments for
  why a default or a number is what it is. Those belong in `CHANGELOG.md` (what
  changed and why) and the tool's `DEVELOPMENT.md` (design decisions and rules
  future work must follow). Tells that a README has drifted into justifying
  rather than describing: "used to", "previously", "which matters because", "is
  the point", "not something to do"
- Don't open a README with the one narrow use case that prompted the work.
  Describe the general problem the tool addresses, then let the specific cases
  follow — a tool that handles five kinds of misfiled CVE shouldn't read as if
  it only handles bundled JavaScript
- When a change lands, put the reasoning in the CHANGELOG entry, put any rule
  future work must follow in `DEVELOPMENT.md`, and give the README only the
  resulting behavior. Documentation that survived a behavior change unedited is
  worse than none: check whether an existing section now describes something
  that is no longer true

## fedrq quirks
- Koji side tag repos (`-r @koji:<tag>`) only index binary RPMs, not source RPMs. `fedrq subpkgs -S` returns nothing for side tags. To query side tag contents, use `fedrq pkgs -r @koji:<tag> '*'` (but this includes inherited packages) or resolve binary RPM names via `koji buildinfo <nvr>` first
- Use `@koji-src:<tag>` to query source RPMs in a Koji repo (e.g. BuildRequires). ebranch's resolve command does this automatically when given `--source-repo @koji:<tag>`
- Side tag repos are standalone — do not pass `-b` with `-r @koji:<tag>`
- `fedrq whatrequires` requires `-F source` (not `-F source_name`)
- `fedrq` may return `(none)` as a result — always filter it out
- EPEL 10+ `@testing` repos are not yet supported by fedrq (the metalink URLs don't exist for `epel10`). The `@testing` probe will fail silently and fall back to side tag or reverse dep listing

## Dependencies
- Before starting feature work, run `cargo audit` and address any reported vulnerabilities first (patch bump or `cargo update -p <crate> --precise <version>`)
- **Security advisories whose fix isn't packaged in Fedora yet** — decide by actual exposure, not the paper severity:
  - **Real exposure** (we parse/receive attacker-controllable input on the affected path): bump the requirement to the fixed version immediately and flag the missing Fedora package in TODO.md as a prerequisite for Fedora packaging work.
  - **Not exploitable for us** (e.g. DoS-only on data from trusted TLS endpoints, or the vulnerable API isn't the one we use): prefer a **version range** spanning Fedora's version up to the fixed series, e.g. `quick-xml = ">=0.40, <0.42"`. Cargo's resolver pins the max (the fixed version) in `Cargo.lock`, so our builds, `cargo install` users, and `cargo audit` all see the fixed version with no ignore entries — while Fedora builds still resolve against the version it has. **Requirements:** verify the floor actually works (`cargo update -p <crate> --precise <floor>` then build + test the affected crates, then restore), and record in TODO.md to tighten the floor once Fedora catches up. While any range is in effect, **every release must re-test both ends** — `cargo update -p <crate> --precise <floor>`, build + test the affected crates, restore to the ceiling, test again — since either end can drift (a new patch release at the ceiling, code changes that silently break the floor). Ranges are meant to be **short-lived**: tighten back to a single requirement as soon as Fedora ships the fixed version on all relevant branches. Fall back to an `audit.toml` ignore with a written rationale only when a range is impossible (API breakage across the range)
- Before a semver-breaking release, check for deps that themselves require a major version bump and consider bundling those upgrades with your own breaking release. Use `cargo update --dry-run --verbose` and look for `Unchanged <crate> (available: <new major>)` lines — do NOT use `cargo outdated`, which doesn't understand `[workspace.dependencies]` inheritance and falsely reports "All dependencies are up to date" for this workspace
- For every candidate major dep bump (whether taken or deferred), check Fedora availability of the new version so we don't get blocked later: `fedrq pkgs -b <branch> -F nev rust-<crate>` across rawhide, the active Fedora branches, and epel9/epel10. Record the result in TODO.md next to the bump entry, and explicitly flag any dep whose new version is not packaged at all or has not reached stable on every branch (e.g. still in Bodhi testing — check `https://bodhi.fedoraproject.org/updates/?packages=rust-<crate>`) so the Fedora packaging work can be done first
- Routine `cargo update` (semver-compatible bumps) should be a separate commit from feature work so it is easy to revert if something regresses
- After any dependency change, run `cargo clippy --workspace && cargo cov` to verify nothing broke

## Testing
- Always write corresponding tests when adding or modifying features
- **Tests must not reach the external network.** `cargo test` has to pass in a distro packaging sandbox (Koji, Debian buildds) where *external* network is blocked but **loopback is up**. So a test that calls a real remote service makes the crate unpackageable, but **localhost is fine**. Two established patterns here, both packaging-safe:
  - HTTP-client crates: stand up a **`wiremock` mock server on `127.0.0.1`** and point the client at it (see `cpu-sig-tracker`'s `*_end_to_end` tests). It's loopback-only — no external call.
  - Pure logic: keep parsing/decision code in injectable functions (a fetcher closure, a trait, canned fixtures) and unit-test those against recorded data.
  Shelling out to **`git`** for a local fixture repo is acceptable (dbranch does this), but git is **not** guaranteed in a minimal build sandbox — it's absent from Fedora's rust buildroot (Debian's buildd is unverified; don't assume either way). So a crate whose tests run git **must declare it as a build dependency** wherever it's packaged (Fedora: `BuildRequires: git-core`; Debian: add to `Build-Depends` if a build there turns out to need it). The `%check` runs in the unpacked source tarball, not a git checkout, so `git init`/`commit` in a temp dir is fine — but the binary has to exist. dbranch 0.15.0 failed Koji's `%check` for exactly this — 26 fixture tests panicked with `NotFound` spawning `git` — fixed by adding the BuildRequires, not by changing the tests. Do **not** depend on a *remote* service or on domain tools that may be absent there (`ubuntu-distro-info`/`debian-distro-info`, `gbp`, `dput`, `koji`, `fedrq`, `curl`, …); dbranch keeps those out of its tests via pure helpers + `--dry-run`. If a test genuinely needs a live remote endpoint, gate it behind `#[ignore]` so the default `cargo test` stays offline. To verify locally, run **`scripts/packaging-test.sh`** (optionally `-p <crate>`) — it builds with network, then re-runs under `unshare -rn` with loopback up **and** the distro tools (gbp/dput/koji/fedrq/distro-info/…) shadowed by failing stubs, mirroring a packaging build's %check (no external network, no distro tools, only base toolchain + git). (Don't hand-run a bare `unshare -rn`: it leaves loopback down and gives false wiremock failures.)
- Per commit, run `cargo fmt` and `cargo clippy --workspace`; `cargo test` is recommended for code you touched. Fast prototyping commits don't need full coverage checks
- Run `cargo cov` at stability points — before release tagging, and when catching a feature up for its README/CHANGELOG entry. Coverage must stay at or above 80% line coverage at those gates (binary `src/main.rs` files are excluded from the measurement; see `.cargo/config.toml`)
