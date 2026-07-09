# ebranch

Build dependency resolver for cross-branch package porting. Given a set
of source packages, a source branch (e.g. `rawhide`), and a target branch
or repository (e.g. `epel10`, a Koji side tag), ebranch discovers which
BuildRequires are missing on the target, computes the full transitive
closure, detects dependency cycles, and produces a phased build order
for parallel execution. It can also verify that all subpackages will
be installable after building, expanding the closure as needed.

Beyond dependency resolution, ebranch files and escalates branch
requests (`branch-request`), analyzes crates.io dependencies
(`check-crate`), links Bugzilla package review requests
(`check-pkg-reviews`), and checks whether a Koji side tag or
Bodhi update would break reverse dependencies (`check-update`) —
optionally casting Bodhi karma with per-bug feedback based on the
result (`--give-karma`), or submitting a passing side tag to Bodhi
as a new update (`--submit`).

Shells out to [fedrq](https://src.fedoraproject.org/rpms/fedrq) for
repository queries.

## Installation

```
cargo install ebranch
```

Requires `fedrq` to be installed and available in `$PATH`.

## Usage

At least one of `--source` / `--source-repo` and one of `--target` /
`--target-repo` is required. When both a branch and a repo are given,
fedrq combines them (e.g. `--target c10s --target-repo @epel` queries
CentOS Stream 10 base repos plus EPEL).

Koji repos can be used as source (`--source-repo @koji:f45-build`).
Since `@koji:` repos only index binary RPMs, ebranch automatically
uses `@koji-src:` for source RPM queries (BuildRequires, subpackage
Requires) while keeping `@koji:` for binary RPM resolution.

### Analyze a crates.io crate's dependencies

Use `check-crate` to check which dependencies of a Rust crate are
available in a target RPM repo, which have no matching version, and
which are missing entirely:

```console
$ ebranch check-crate semcode -b rawhide
Checking crate: semcode 0.14.0
Branch: rawhide

Dependencies (35 normal, 0 build, 16 dev):

Missing (8):
  - semcode-core ^0.14.0 (normal)
  ...

No matching version (8):
  - tree-sitter ^0.26 (normal)
    available: 0.25.10, need: ^0.26
  ...

Satisfied (35):
  - libc ^0.2 (normal) — 0.2.182
  ...

Summary: 8 missing, 8 unmet, 35 satisfied.
```

Specify a version (defaults to latest):

```sh
ebranch check-crate tokio 1.51.0 -b epel9 -r @epel
```

Use `--transitive` / `-t` to expand missing dependencies transitively,
showing the full set of crates that need to be packaged:

```sh
ebranch check-crate arrow 57.3.0 -b rawhide -t -v
```

By default, normal, build, and dev dependencies are expanded
(matching Fedora's `%check`-enabled builds). Add `--exclude-dev`
to skip dev deps, or `--include-optional` to also expand optional
deps. The output includes a phased build order showing which
`rust-*` packages to build first.

### Useful flags

- `--transitive` / `-t` — expand missing `check-crate` deps transitively
  (includes phased build order)
- `--exclude-dev` — exclude dev deps from transitive expansion
- `--include-optional` — include optional deps in transitive expansion
- `--exclude-unmet` — exclude unmet-version deps (packaged but too old)
  from transitive expansion; they are included by default, since
  omitting them silently under-reports what needs rebuilding
- `--exclude CRATE,...` — skip specific crates in transitive expansion
- `--dot` — output dependency graph in Graphviz DOT format
- `--toml PATH` — save full analysis to a TOML file for reuse
- `--verbose` / `-v` — print progress to stderr as packages are resolved
- `--max-depth N` — limit recursion depth (useful for exploring large
  dependency trees incrementally)
- `--check-install` — verify subpackage installability and expand the
  closure with any additionally needed packages
- `--exclude-install PKG,...` — exclude source packages from
  installability checks (deps they provide are treated as satisfied)
- `--no-auto-exclude-install` — disable automatic exclusion of solib symbol
  version deps (e.g. `libc.so.6(GLIBC_2.38)(64bit)`) from
  installability checks
- `-j N` / `--jobs N` — number of parallel fedrq queries
  (0 = number of CPUs, the default)
- `--koji` — output as a Koji chain build string
- `--copr` — generate a Copr batch build script
- `--refresh` — clear fedrq repo metadata cache before querying
- `--json` — machine-readable JSON output

With `--koji`, `--copr`, or `--dot`, the machine output goes to stdout
and the human-readable report (what needs building, at which versions)
goes to stderr — so `ebranch check-crate … --koji > build.sh` writes a
clean script while you still see the report, and `… --koji | sh` works.

### Link Bugzilla review requests

Use `check-pkg-reviews` to find and link Bugzilla package review
requests based on the dependency graph from `check-crate --toml`:

```sh
# 1. Run analysis and save to TOML
ebranch check-crate arrow 57 -b rawhide -t --toml arrow.toml

# 2. Find review bugs and show proposed Depends On changes
ebranch check-pkg-reviews arrow.toml --dry-run -v

# 3. Apply the changes (requires Bugzilla API key)
export BUGZILLA_API_KEY=your-key
ebranch check-pkg-reviews arrow.toml -v
```

Found bug IDs are cached in the TOML file under `[review_bugs]`,
so subsequent runs skip the Bugzilla search for already-found bugs.

The tool only links missing packages (not unmet-version deps that
already exist in the repo). It preserves existing Depends On links
to bugs outside the dependency set.

### Check if an update would break reverse dependencies

Use `check-update` to verify that a Koji side tag or Bodhi update
won't break packages that depend on the updated packages. It compares
old vs new subpackage Provides, classifying each as updated (version
bump) or removed, then finds reverse dependencies that would break:

```console
$ ebranch check-update epel9-build-side-134436 \
    -b c9s -r @epel -v
[check-update] updated packages: rust-tokio, rust-tokio-macros
[check-update] using @testing for new provides
[check-update] 4 changed provides (4 updated, 0 removed)
...
### Updated Provides (4)

- `crate(tokio-macros)` (2.6.1 → 2.7.0)
- `crate(tokio-macros/default)` (2.6.1 → 2.7.0)
  ...

No packages depend on the changed Provides. No breakage expected.
```

The output is Markdown, so it can be pasted directly into Bodhi
comments.

Reverse dependencies are checked on two axes, with full rich-dep
(boolean) semantics and RPM version comparison against the update's
new Provides:

- **FTI** (fails to install) — a binary subpackage's install-time
  Requires stops resolving once the update ships
- **FTBFS** (fails to build from source) — the source package's
  BuildRequires stops resolving for its next rebuild, e.g. a
  versioned pin like `(crate(ctor/default) >= 0.6.0 with
  crate(ctor/default) < 0.7.0~)` when ctor moves to 1.x

The summary counts each kind and every broken requirement is labeled
`[FTI]`/`[FTBFS]`. (If a touched capability is also provided by an
unrelated package, the check can over-report — the safe direction.)

The input can also be a Bodhi update alias or URL:

```sh
ebranch check-update FEDORA-EPEL-2026-f9eaa11e18 -b c9s -r @epel
ebranch check-update https://bodhi.fedoraproject.org/updates/FEDORA-EPEL-2026-f9eaa11e18
```

…or a **COPR project** — for big coordinated updates staged in a COPR
before any side tag or Bodhi update exists. Pass an `owner/project`
spec (`@group` for group projects) or the project URL:

```sh
ebranch check-update @rust/uutils-and-nushell -b rawhide
ebranch check-update https://copr.fedorainfracloud.org/coprs/g/rust/uutils-and-nushell/ -b rawhide
ebranch check-update @rust/uutils-and-nushell -b al9 -r @epel --testing-branch epel9
```

The update contents come from COPR's monitor API (each package's
latest **succeeded** build in the chroot matching the branch; x86_64
preferred) and the new provides from fedrq's `@copr:` repo class —
COPR repos index source RPMs and regenerate their own repodata, so
there is no koji or regen-repo involvement. COPR input always
requires `-b` (a COPR builds for many chroots); when `-b` is a base
branch like `al9`, add `--testing-branch epel9` to name the chroot.
`--give-karma` and `--submit` don't apply to COPRs.

For new provides, ebranch checks these sources in order:
1. **@testing** — preferred when the update has been pushed
   there, since the rendered repodata is authoritative. Two
   gates protect against a stale snapshot:
   - For Bodhi-alias input, the update's status must be
     `testing` (a `pending` update is in koji but has not
     reached `updates-testing` yet, so @testing would still
     return the previous V-R).
   - `@testing` must report at least one subpackage whose
     `(version, release)` matches one of the input NVRs.
2. **Side tag** — via `koji buildinfo` + `fedrq pkg_provides`.
   Cross-checks each build against the V-R the side-tag repodata
   actually serves: one batched
   `fedrq -F line:source,version,release` query maps every
   (non-debug) binary back to its source, and
   a build is fresh if any of its binaries resolves to the
   expected version-release. If they disagree (typically
   because `koji regen-repo` hasn't run yet), ebranch offers to
   run `koji regen-repo --wait <side-tag>` on your behalf
   (default yes), clears both the fedrq smartcache and the
   libdnf5 metadata cache so the re-check sees the regenerated
   repodata, and re-checks before continuing. Declining the
   regen prompts whether to continue with stale data (default
   no — the check aborts). In `--json` mode or when stdin isn't
   a terminal there are no prompts; the report opens with a
   banner listing the stale sources instead, and the remedy is
   a manual `koji regen-repo <side-tag>` followed by a rerun
   with `--refresh` (which clears both caches).
3. **Reverse deps only** — lists affected packages for manual review

`-b`/`--branch` and `-r`/`--repo` are override-only. The branch is
inferred from the input: the Bodhi release for an update alias, or the
name of a **Fedora** side tag (`f43-build-side-*` uses `f43`). `--repo`
defaults to the branch's stable base repos (the correct comparison
baseline).

**EPEL is not auto-resolved.** The `epelN` branch alone can't resolve
base-OS dependencies, so an EPEL input without `--branch` errors out —
both an `epel*-build-side-*` side tag and a Bodhi EPEL update (whose
release derives to `epelN`). Pass a RHEL-compatible base branch plus the
EPEL repo, e.g. `-b al9 -r @epel` (epel9) or `-b c10s -r @epel`
(epel10). The choice of base distribution (AlmaLinux, CentOS Stream, …)
is yours, which is why it isn't guessed.

For EPEL side tags, the testing branch is auto-detected from the
side tag name (e.g. `epel9-build-side-*` uses `epel9`). Use
`--testing-branch` to override if needed.

After the check, `--give-karma` casts karma on the update.

Interactively (a TTY, without `--yes`), you first **curate the blocking
findings** — installability issues and reverse-dependency breakage
(grouped by the changed Provide that causes it). For each, choose
**(k)eep** (real, still counts against the update), **(e)xplain** (real
but acceptable — you record a one-line justification), or **(r)emove** (a
false positive). The decisions feed both the suggested karma and the
posted comment: explained findings move to an "Issues addressed by the
reviewer" section (with your reasons) and removed ones are dropped, so
explaining or removing the only blocking finding lets the suggested karma
rise from `-1` to `0`/`+1` — no silent override needed. Under `--yes` or
non-interactively, every finding is kept (the prior behavior).

The (possibly curated) check result then suggests the karma value — `+1`
when no issues remain, `-1` when reverse deps break or the updated
packages have unsatisfied deps, `0` when the analysis was incomplete —
and you are prompted with that suggestion as the default (Enter accepts,
or override with `+1`/`-1`/`0`). Listed bugs get per-bug
feedback like the Bodhi web UI: update-request bugs
(`<pkg>-<version> is available`) are auto-voted `+1` when the
update delivers at least the requested version and `-1`
otherwise; for any other bug you are prompted (`+1`/`-1`/`0`).
The full voting plan is shown for confirmation before anything
is posted; `--yes` skips the prompts (non-update bugs then get
`0`). The posted comment is the full Markdown check report with
a provenance footer (ebranch version and the command invocation);
`--comment <TEXT>` adds reviewer notes as a section near the top,
and you are prompted for notes interactively when the flag is
omitted. When the update is your own,
the overall karma is skipped (Bodhi ignores submitter karma) but
per-bug feedback is still posted. Voting
requires a Bodhi update (not a bare side tag) and reuses the
`bodhi` CLI's login session. The session is validated before the
analysis starts: if there is none, an interactive login is run
for you (via `bodhi overrides query --mine`), and expired tokens
are refreshed automatically.

```sh
ebranch check-update FEDORA-2026-94cb04410a --give-karma \
    --comment "no broken reverse deps; works for me"
```

For a side tag that hasn't been submitted yet, `--submit` turns
check-update into a pre-flighted `bodhi updates new --from-tag`: the
check runs first, and only a passing result is submitted — catching a
subpackage update that is accidentally missing a package *before*
anything is published. Update notes are required, either inline with
`--notes <text>` or from a file with `--notes-file <path>` for longer
descriptions (the two are mutually exclusive). Optional fields mirror
the bodhi CLI: `--type` (bugfix/enhancement/security/newpackage,
default bugfix), `--severity` (required for `--type security`),
`--bug <ID,...>` (repeated or CSV; associated bugs are closed when the
update goes stable), and `--stable-karma`/`--unstable-karma`/
`--disable-autokarma` for the autopush thresholds.

The pass gate reuses the karma derivation: a clean `+1` check submits
after showing the plan (packages, type, bugs, thresholds, notes
preview) for confirmation. A non-passing check first goes through the
same interactive keep/explain/remove curation as `--give-karma`; if
blocking findings remain you are asked whether to submit anyway
(default **no**). Non-interactive runs and `--yes` never submit a
failing update. Notes, the bodhi session, and cheap flag validation
(e.g. `--type security` without a severity) are all checked *before*
the analysis, so mistakes fail in seconds rather than after minutes of
fedrq queries. Like voting, submission reuses the `bodhi` CLI's login
session and prints the new update's URL when Bodhi accepts it.

After submitting, the check report is posted on the new update as a
review comment via the same flow as `--give-karma`: per-bug feedback
records whether each listed bug is addressed by the delivered versions
(Bodhi zeroes the submitter's *overall* karma on their own update, but
per-bug feedback still counts), `--comment <TEXT>` adds reviewer notes
near the top, and the comment plan is confirmed before posting (`--yes`
skips the prompts). So the Bodhi page ends up with both the update and
its review checklist in one pass.

```sh
ebranch check-update epel9-build-side-134436 -b al9 -r @epel \
    --submit --type enhancement --bug 2482250 \
    --notes "Update uutils to 0.2 and rebuild dependent crates"
```

### Detect dependency cycles

```sh
ebranch find-cycles systemd util-linux \
    --source rawhide --target c10s --target-repo '@epel'
```

### Resolve the full dependency closure

```sh
ebranch resolve systemd --source rawhide --target c10s --target-repo '@epel'
ebranch resolve systemd --source rawhide --target c10s --json
```

The output groups packages into parallel build phases:

```console
$ ebranch resolve rust-base64-simd \
    --source rawhide \
    --target-repo '@koji:epel10.3-build-side-133542'
Build order from rawhide to @koji:epel10.3-build-side-133542:

  Phase 1:
    - rust-const-str
    - rust-outref
  Phase 2:
    - rust-vsimd
  Phase 3:
    - rust-base64-simd

4 package(s) in 3 phase(s).
```

Add `--koji` for Koji chain-build output or `--copr` for a Copr
batch build script:

```sh
ebranch resolve --koji rust-base64-simd \
    --source rawhide --target-repo '@koji:epel10.3-build-side-133542'

ebranch resolve --copr rust-base64-simd \
    --source rawhide --target-repo '@koji:epel10.3-build-side-133542' \
    > build.sh
```

The same `--koji` and `--copr` flags work with `check-crate -t`:

```sh
ebranch check-crate arrow 57 -b rawhide -t --koji
ebranch check-crate arrow 57 -b rawhide -t --copr > build.sh
```

Use `--check-install` to verify that every subpackage in the closure
will be installable after building:

#### Base-distro guard (EPEL targets)

EPEL packages must not replace base-distro (RHEL / CentOS Stream)
packages. For EPEL targets, `resolve` probes the base distro behind the
target — `epel10` uses `c10s`; `epel9` uses `al9`, because fedrq's
`c9s` layers epel9 + epel9-next on top of CentOS Stream 9 and UBI's
package set is incomplete, so AlmaLinux stands in for RHEL 9 — and a
dependency whose provider exists there at a version that doesn't
satisfy the constraint is **blocked**, not treated as missing: the
closure is pruned at that point and the report explains the situation
(this is what a branch request like rhbz#2482250 gets closed CANTFIX
for):

```console
Blocked by base distro (c10s) — EPEL must not replace these packages:
  - python-setuptools: needs python3-setuptools >= 77 (python-django6);
    c10s has 69.0.3-9.el10

Options for blocked packages: introduce an alternate,
non-conflicting package (rerun with --override <pkg>; an
alternate needs a NEW package review, not a branch request),
or lower the depending package's requirement to the
base-distro version.
```

On a terminal, `resolve` asks per blocked package whether to descend
into it as a deliberate override (default no); non-interactively it
never descends. `--override PKG,...` pre-approves packages you intend
to ship as alternates — the analysis then continues through them and
they're annotated `(override — needs new package review)` in the
output and marked in the report so `file-requests` refuses to file
branch requests for them. `--base-branch` overrides the inferred base
(or enables the guard for branches without a mapping, e.g. epel8). A
dep the base actually *satisfies* is treated as satisfied — useful when
the target repo is `@epel`-only and doesn't see the base at all.

### File and escalate EPEL branch requests

Once you know which packages need branching (from
`check-crate --toml` / `resolve`), file Bugzilla branch
requests and chase the ones that go unanswered.

File a single request:

```sh
# Requires a Bugzilla API key (BUGZILLA_API_KEY env var or
# `ebranch config`).
ebranch file-request foo epel9
ebranch file-request foo epel9 --fas alice          # offer to co-maintain
ebranch file-request foo epel9 --fas alice --sig rust-sig
```

The request is filed against `Fedora EPEL`/`<branch>`, falling
back to `Fedora`/`rawhide` when the component isn't in EPEL. The
request blocks nothing by default — pass `--blocked` with tracking
bugs/aliases to block, and `--dependson` for prerequisite bugs.
(Earlier versions blocked the `EPELPackagersSIG` tracker
automatically; that SIG is defunct.) Pass `--report <file>` to
record the new bug ID in a resolve report.

To file for a whole dependency closure, first capture it with
`resolve --report`, then file requests for every package and
link them along the dependency graph (a package's request
`depends_on` the requests for the packages it needs):

```sh
ebranch resolve python-django6 --source rawhide \
    --target c10s --target-repo @epel --report django.toml
ebranch file-requests django.toml epel9 --fas alice --dry-run
ebranch file-requests django.toml epel9 --fas alice
ebranch file-requests django.toml epel9 --blocked 2482250   # block a tracker
```

`--blocked` applies to every request the batch files.

Bug IDs and a `pinged` flag are stored in the report under
`[branch_requests]`, so re-runs skip already-filed packages.

Before filing, both `file-request` and `file-requests` run a
base-distro pre-flight: packages that exist as source packages in the
base distro behind the branch (epel10 → c10s, epel9 → al9; override
with `--base-branch`) are refused/skipped — a branch request for a
base-distro package is always CANTFIX, and report packages marked as
overrides are skipped too (an alternate package needs a **new package
review**, not a branch request). The pre-flight re-checks the base
itself, so stale or pre-guard reports can't slip one through.

Escalate requests that have sat in NEW for at least a week —
adds a `needinfo?` ping and marks them so they're not pinged
again:

```sh
ebranch escalate django.toml epel9 --dry-run
ebranch escalate django.toml epel9
```

All three accept `--dry-run` and `--verbose`; `--dry-run`
previews without contacting Bugzilla (escalate still reads bug
state to decide what it would ping).

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
