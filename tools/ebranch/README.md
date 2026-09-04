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
(`check-pkg-reviews`), tracks a packaging effort's progress into the
distro (`check-wip`), and checks whether a Koji side tag or
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

Responses from crates.io are cached under `~/.cache/ebranch/crates-io/`
— a published crate version never changes, so its dependency list and
feature map are kept for good, and only the versions list ages (a
day); `--refresh` re-fetches. Repeat runs are near-instant and hit
crates.io less, not more. A report saved with `--toml` can be
re-rendered later without touching the network or the repo:

```
ebranch check-crate --from rbw.toml
ebranch check-crate --from rbw.toml --copr > build.sh
```

### Useful flags

- `--transitive` / `-t` — expand missing `check-crate` deps transitively
  (includes phased build order)
- `--include-optional` — for an *application* root (a crate shipping
  binaries), also count optional dependencies its default features do
  not enable. A *library* root and every transitive crate count all
  features regardless, because that is how Fedora builds them
  (`%cargo_generate_buildrequires -a`): an optional dependency of a
  library is a hard requirement for packaging it
- `--features FEATURE,...` / `--no-default-features` — for an
  application root, the features its Fedora build enables, as
  `%cargo_generate_buildrequires -f` / `-n`. Without the flag, and when
  `rust-<crate>` or `<crate>` already exists in rawhide, check-crate
  reads that line from the spec on dist-git (so an update check of
  uutils-coreutils picks up its `-f feat_acl,…` by itself; a
  conditional `%global` is read as the union of its definitions)
- `--in-tree GLOB,...` — crates built from the root's own source tree
  (a workspace's members, `uu_*` for uutils-coreutils): not
  dependencies Fedora packages, so they leave the missing/unmet lists
  into a "Built in-tree" line, while *their* dependencies are checked
  as the workspace's and marked `via <member>`. Each is checked with
  the features the root's enabled set requests of it
  (`feat_selinux = ["uu_ls/selinux", …]`), and a member's own dev
  dependencies do not count: it is built as a dependency, not tested.
  The entry `@repository` also takes every dependency published from
  the root's own repository; it is not on by default, because sharing
  a repository does not make a crate in-tree — phf's workspace
  publishes phf_shared and phf_macros as crates Fedora packages on
  their own. The globs are trusted as written — only the spec knows
  what a package builds in-tree, and rawhide still carries stale
  `rust-uu_*` packages nobody retired — so keep them precise:
  `--in-tree 'uu_*,uucore*,uutests'` for uutils-coreutils, where a
  bare `uu*` would also swallow `uutils_term_grid`, a dependency in
  its own right. `--verbose` lists the glob matches the repo packages
  on its own — an over-broad glob, or packages something else still
  needs (nushell uses the old `rust-uu_*`; in the workspace build the
  in-tree members win regardless). The list can live in the config
  file per crate, see Configuration
- Dependencies limited to another target — `cfg(windows)`,
  `x86_64-pc-windows-msvc` — are not counted at any level; the
  crates.io `target` is evaluated for a Linux build the way
  `%cargo_generate_buildrequires` does, with architecture predicates
  treated as true
- `--staging-copr OWNER/PROJECT` — a staging COPR layered over the branch
  (`@rust/uutils-and-nushell`): whatever the branch does not satisfy
  is looked up in the COPR too, and a hit is reported under "Staged in
  COPR, not yet in the branch" rather than as missing — built, still
  in flight, and not expanded further; transitive crates the COPR
  provides are listed there too, with what pulled them. The branch picks the chroot
  (`-b rawhide` → `fedora-rawhide-*`), so a COPR building for several
  releases is checked one target at a time. The COPR's repository
  metadata is refetched on every run (a staging COPR changes by the
  minute; the branch's stays cached as usual), so a build that just
  finished is seen without `--refresh`. When the very version being
  checked is already built — in the branch, or only in the COPR — the
  report's header says so, since a build would then be a rebuild
- `--package NAME` — the Fedora package when it is not `rust-<crate>`
  (the `coreutils` crate is `uutils-coreutils`): used for the spec
  lookup and as the report's package name
- `--exclude-dev` — exclude dev deps from transitive expansion
- `--include-optional` — include optional deps in transitive expansion
- `--exclude-unmet` — exclude unmet-version deps (packaged but too old)
  from transitive expansion; they are included by default, since
  omitting them silently under-reports what needs rebuilding
- `--exclude CRATE,...` — ignore crates entirely, direct or transitive, as
  dependencies Fedora will not package; added to the config file's
  `[check-crate] exclude` list, or to the built-in benchmark set when
  no config sets one (see Configuration)
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
- `--copr` — generate a Copr batch build script, each package
  commented with why it is in it
- `--refresh` — clear fedrq repo metadata cache before querying (a
  `--source-repo @koji:<tag>` side tag is refetched every run anyway)
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

### Track packages on their way into the distro

A coordinated effort — a stack of new crates, a version bump that
drags its dependencies along — moves every package through the same
sequence: staged somewhere, reviewed or submitted as a pull request,
built for Rawhide, branched, built again, shipped in an update. Use
`check-wip` to see where each one is.

```console
$ ebranch check-wip uutils.toml --copr @rust/uutils-and-nushell
15 package(s) tracked in @rust/uutils-and-nushell
targets: rawhide

newer build in a COPR, not landed in rawhide (14)
  rust-exacl 0.13.0-1 (as of 2026-08-12)
    dist-git: rawhide, f44, f43 (as of 2026-08-12)
    rawhide: 0.12.0-7.fc45 (as of 2026-08-12)
  ...

newer build in a side tag, not landed in rawhide (1)
  rust-emojis
    dist-git: rawhide, f44, f43 (as of 2026-08-12)
    rawhide: 0.8.2-2.fc45, built 0.9.0-1.fc46 in f46-build-side-146944 (as of 2026-08-12)
  ...

review filed, awaiting approval (2)
  rust-sponge-cursor 0.1.0-1 (as of 2026-08-10)
    dist-git: no repository (as of 2026-08-10)
    review: rhbz#2498026 new (as of 2026-08-10)
```

An update in flight for an *earlier* version is reported too, because it
is what stands between a finished build and an update of its own: Bodhi
requires days in testing before an update can go stable, and editing that
update to carry the new build would restart the clock and discard its
karma. The branch line shows the older update, the alias, and how long it
has served — `0.19.1-1.fc44 in FEDORA-2026-2134e68e6e testing (7 of 7
days)` — and the state reads `waiting on an earlier update`, ranked below
`needs an update` since there is nothing to do but wait.

Where a newer build is waiting is part of the state, from one template
so the lines stay comparable: `newer build in a COPR, not landed in
rawhide` when Koji has no build of the staged version, and `newer build
in a side tag, not landed in rawhide` when it has one that no release tag
carries — nothing will pick that up on its own, which is why the branch
line names the side tag. A build in the release's own tags reads `built
for rawhide, not yet in the repos`, where a compose really is the only
wait.

Each heading names what stands in the way rather than the state
alone, and the versions are shown side by side so the comparison
behind the heading can be checked by eye: `rust-exacl` is staged at
0.13.0 while Rawhide still carries 0.12.0, so that work has not landed
yet. A package with no dist-git repository has not been imported, so
its review request is looked up — only for those, since a package
already in dist-git is past that stage. Approval means the
`fedora-review+` flag rather than the bug being closed: a review
closed without it was abandoned, not accepted.

The effort lives in a **ledger**, the TOML file named on the command
line, created on first run. It — not the COPR — is the source of truth
for which packages belong to the effort: a COPR shrinks as packages
graduate out of it, so a report rebuilt from one each run would lose
exactly the work that is finished. The ledger also records what no
service can report, such as which route a package is taking and which
review bug or pull request is landing it. It remembers its COPRs too,
so `--copr` is only needed to seed the ledger or add another.

Being absent from a branch's repositories has more than one cause, so
Koji and Bodhi are asked too. Koji says whether a build exists at all —
a build is tagged the moment it succeeds and only reaches repodata at
the next compose — and on a branched release Bodhi says which update is
carrying it. That separates work still to do from work already done and
waiting:

```console
  rust-exacl 0.13.0-1 (as of 2026-08-11)
    dist-git: f44, rawhide (as of 2026-08-11)
    f44: 0.12.0-6.fc44 in FEDORA-2026-62c8a3ebba stable (as of 2026-08-11)
    rawhide: 0.12.0-7.fc45 (as of 2026-08-11)
```

One line per branch, and a version is stated once: what Koji has and
what the repositories have are usually the same, so a build is only
called out when it is *ahead* of the repositories — which is the
outstanding work — and an update qualifies the version it carries. The
date is the oldest of the facts on the line, so it never claims more
freshness than its stalest part.

Both the candidate and testing tags are queried, since neither sees the
other: a build with no update yet sits in the candidate tag, one whose
update is in flight sits in the testing tag, and both inherit the stable
tag. Add `--side-tag f43-build-side-145899` for a side-tag build, which
is tagged only there until an update carries it — one query per side tag
covers every package in it, and the tag is recorded in the ledger so
later runs need not repeat it. A name that is not
`<branch>-build-side-<id>`, with a numeric Koji task id, is refused
rather than recorded, since it could only fail on every later run. Where no build is found the report says
so rather than claiming the package needs building: a build can exist in
a side tag the ledger has not been told about.

Each branch's Koji tags come from Bodhi's release list, so no release
number is hardcoded and Rawhide is followed as it moves. The RPM release
is picked by its id prefix, since F43, F43C and F43F all report branch
`f43` and the container and flatpak variants carry different tags. Only packages
whose staged version is ahead of what a branch ships are asked about,
since for anything already current the question is settled — and Koji
is one subprocess per package. Rawhide is not exempt from the Bodhi
step: its builds can be carried by an update too, automatically or
submitted from a side tag, and Bodhi files those under the release name
for whatever Rawhide currently is.

A retired package keeps its dist-git repository, so retirement is
reported ahead of anything about builds — otherwise "in dist-git, not
built for rawhide" would describe a dead package as work waiting to be
done. Whether unretiring needs a fresh review depends on how long the
package has been retired, which is policy rather than something the
tool decides; `--rescan-reviews` looks for a new review request where
one may have been filed.

`--package NAME,...` narrows the per-package lookups and the report to
what you name, which pairs with `--rescan-reviews` — you generally
know which package was retired, and rescanning all of them wastes a
Bugzilla query each. The COPR reconcile still covers the whole effort,
since skipping it would leave a departed package marked as staged. A
name the ledger does not track is called out rather than quietly
matching nothing.

Observations that cannot change are not re-fetched. A closed review of
an imported package is settled, so it is skipped — unless the package
is retired, where a returning package may need a new review and the
recorded bug is the old one.

Each run reconciles rather than rebuilds:

- in the COPR but not the ledger — new work, added;
- in both — its observations refreshed;
- in the ledger but no longer in the COPR — the entry stays and stops
  being counted as staged, since it has finished or moved on. Use
  `--prune` to forget those.

A registered side tag also seeds the ledger: packages it holds that the
ledger does not have are added, recorded as built for that branch rather
than staged, since Koji has already accepted the build. Additions are
reported per tag. This makes an effort with no COPR behind it — built
straight into a side tag — as self-populating as one staged in COPR.

`--forget NAME,...` is the counterpart to `--add`: it drops packages and
records the refusal in the ledger's `ignored` list, so neither a COPR nor
a side tag takes them up again — deleting alone would not last, since
whatever produced the package is usually still registered. `--add`
reverses it. This is separate from pruning, which acts only on packages
a COPR once staged.

A ledger holds two lists with different lifetimes: the packages an
effort tracks, which outlast many rollouts, and its side tags, which die
with the rollout they carried. `--prune` names which to forget —
`--prune packages` (the default when no value is given), `--prune
side-tags`, or both as `--prune packages,side-tags`. Because the value
is optional, give the ledger path before the flag, or write
`--prune=side-tags`.

Neither prunes on a question that was not answered, and neither touches
a package no COPR ever staged — one added by hand or found in a side tag
is not something a COPR's contents can be evidence about. A package is
forgotten only when a COPR the ledger follows once staged it, every COPR
answered, and the package was in none of them; a side tag only when Koji says the tag does
not exist. A timeout, an unreachable hub, a COPR that failed or an
`--offline` run all leave the lists alone and say why — during an
outage, "I could not ask" would otherwise erase exactly the records that
matter.

When Koji is unreachable — mass branching, an unplanned outage — the
first query gives up after 30 seconds (`SANDOGASA_KOJI_TIMEOUT` in
seconds overrides it), the rest are skipped, and the report comes from
the ledger with one warning rather than a query per tag each waiting its
own timeout.

Every observation is dated, so `--offline` can report from the ledger
without contacting anything and still be honest about the age of what
it shows. Refreshing writes the ledger back even though the command
reads like a query: the observations are facts, expensive to gather,
and discarding them would only make the next run pay again. Decisions
are never inferred — a package the tool cannot place is reported as
such rather than guessed at.

Add `--target epel9` to record which releases the effort is for; one
ledger holds them all, since a package's route and review bug do not
depend on the release while its branch and build state do. Rawhide is
the shared spine — everything lands there first and is branched from it
— so while Rawhide is behind, that is what the heading reports. Once it
is only *waiting* — an update in flight, a compose pending — the heading
becomes the *least advanced* state, naming every target in it: `needs a branch for epel9`, `needs an update for f44`,
`update in testing for f44, f43, epel9`. When every target is done the
heading says so without naming any, since picking between equals would
be arbitrary.

Passing `--side-tag` also records that tag's branch as a target:
building into a side tag is saying the branch is in scope. Targets are
matched against Bodhi's release list by branch or by name, because
neither alone is enough — F45's branch is `rawhide` and EPEL-10.3's is
`epel10` — and two targets that resolve to the same release are
collapsed to one, Rawhide winning since it is always examined. Fedora
names Rawhide's side tags after its version, so `f45-build-side-*`
builds are attributed to Rawhide rather than to a second target for the
same release.

Branches are listed newest release first — Rawhide, then Fedora by
version, then EPEL — and per package the ordering leads with what each
release ships, newest version first, so the releases already carrying
the new version group above the ones still to do. One ledger can
therefore follow two rollouts at once: a version reaching Rawhide and
EPEL 10 while the older branches wait for their updates to go stable
reads as two groups rather than as one interleaved list.

Whether a target already carries the version is settled before asking
whether dist-git has a branch of that name, since the names need not
agree: EPEL 10's minor releases all ship from the `epel10` branch, and a
package shipped for `epel10.3` does not need a branch called that. The
dist-git line lists every branch the repository has, not only the ones
this effort targets.

Which route a package takes is a decision, never inferred from a
lookup, so it is recorded explicitly:

```sh
ebranch check-wip uutils.toml --set rust-sponge-cursor=review:2498026
ebranch check-wip uutils.toml --set rust-fundu=pr:1234
ebranch check-wip uutils.toml --set rust-exacl=direct
```

That writes the ledger and needs no network. Switching route clears the
identifier the previous one carried, so a stale bug number cannot
outlive the route it belonged to, and `unknown` puts a package back to
undecided.

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
ebranch check-update FEDORA-EPEL-2026-f9eaa11e18
ebranch check-update https://bodhi.fedoraproject.org/updates/FEDORA-EPEL-2026-f9eaa11e18
```

An update built in a side tag is listed from Koji; when that tag has
since been deleted (as happens once the update goes stable), the
update's own build list is used and check-update says so.

…or a **COPR project** — for big coordinated updates staged in a COPR
before any side tag or Bodhi update exists. Pass an `owner/project`
spec (`@group` for group projects) or the project URL:

```sh
ebranch check-update @rust/uutils-and-nushell -b rawhide
ebranch check-update https://copr.fedorainfracloud.org/coprs/g/rust/uutils-and-nushell/ -b rawhide
ebranch check-update @rust/uutils-and-nushell -b epel9
```

The update contents come from COPR's monitor API (each package's
latest **succeeded** build in the chroot matching the branch; x86_64
preferred) and the new provides from fedrq's `@copr:` repo class —
COPR repos index source RPMs and regenerate their own repodata, so
there is no koji or regen-repo involvement. COPR input always
requires `-b` (a COPR builds for many chroots); `-b epel9` picks the
`epel-9-*` chroot and compares against al9 plus `@epel`, as below.
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
name of a side tag (`f43-build-side-*` uses `f43`, `epel9-build-side-*`
uses `epel9`). `--repo` defaults to the branch's stable base repos (the
correct comparison baseline).

**EPEL is checked against a base distro.** The `epelN` branch alone
can't resolve base-OS dependencies, so a plain EPEL branch — inferred
from a side tag or a Bodhi release, or given as `-b epel9` for a COPR —
is replaced by its base plus the EPEL repo: epel8 → `-b al8 -r @epel`,
epel9 → `-b al9 -r @epel`, epel10 → `-b c10s -r @epel`, with the EPEL
name kept as the `@testing`/chroot branch. The substitution is printed
on stderr on every run. Passing `-r` yourself turns it off, so
`-b c9s -r @epel` compares against CentOS Stream instead. The
minor-release branches (`epel10.1`) have no assumed base — c10s runs
ahead of a RHEL minor — and still require `-b` and `-r`.

For EPEL side tags, the testing branch is auto-detected from the
side tag name (e.g. `epel9-build-side-*` uses `epel9`). Use
`--testing-branch` to override if needed.

A side tag's or a COPR's repository metadata is refetched on every run
(both change as builds land, and are small); after an offered `koji
regen-repo`, only the side tag's cached metadata is dropped; and when
Bodhi says an update is in testing but the cached `@testing` metadata
does not show it, that repo is refetched and probed once more before
the reverse-dependency fallback. The
branch's own metadata stays cached until `--refresh` clears everything.

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
feedback like the Bodhi web UI. Update-request bugs
(`<pkg>-<version> is available`) are auto-voted `+1` when the
update delivers at least the requested version and `-1`
otherwise; the package is taken from the bug's Bugzilla component,
which names it outright, so a bug is never matched to a
similarly-named package. When the update builds nothing for a bug's
package it cannot be fixing it, so `-1` is suggested with the reason
shown.

FTBFS bugs are auto-voted `+1` when the update carries a build of the
package: the bug says it does not build on that release, and the build
is the artifact that says otherwise. FailsToInstall bugs are answered
from the check's own installability analysis — `+1` when the package's
requirements all resolve, `-1` naming the requirement that does not.
Both are recognized either by the release tracker the bug blocks or by
the fixed wording Fedora's bots use, which names the release itself —
so a bug filed against a different release is left alone. The bot
wording is what covers EPEL, which has no such trackers. A CVE or a
plain bug report gets no suggestion either way.

Review requests (`Review Request: <pkg> - ...`) are auto-voted `+1`
when the update builds the package under review — the usual case for
a `--type newpackage` update. For any other bug, including a review of
a package this update does not build, you are prompted
(`+1`/`-1`/`0`).

The full voting plan is shown for confirmation before anything is
posted; `--yes` skips the prompts, taking the suggested `-1` where
there is one and `0` where there is no verdict at all. The posted
comment is the full Markdown check report with a provenance footer
(ebranch version and the command invocation);
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

The bug list is settled before the plan is shown. First ebranch
proposes bugs the update looks like it closes, from two places: bugs
still open against a package it builds, and `rhbz#` references in the
changelog entries it introduces — the second matters because a bug
fixed in Rawhide is closed when that build lands, so a branch update
carrying the same fix would otherwise have nothing to attach. Only
bugs that would be voted `+1` are proposed, and each is shown with how
it was found. The open-bug search is not scoped to the update's
release: update requests are filed against Rawhide and package reviews
under Fedora, so an EPEL update's bugs are mostly not EPEL bugs.

```console
This update looks like it closes:
  #2504649 rust-fd-find: FTBFS in Fedora rawhide/f45
    open against rust-fd-find
Add these bugs to the update? [Y/n]: y

Submission plan for f45-build-side-146637:
  packages (1): rust-fd-find
  type: bugfix, severity: unspecified
  bugs (closed on stable): #2504649
  ...
```

The same bug then carries its reason into the karma comment, since
both come from one verdict:

```console
  bug feedback:
    +1 #2504649 rust-fd-find: FTBFS in Fedora rawhide/f45
       (rust-fd-find-10.4.2 built for this release, so it is
        not failing to build)
```

Listed bugs are then screened: a bug whose package the update builds
nothing for would be closed by an update that never touched it, so it
is reported and you are offered the chance to leave it off. Only
update requests and review requests are screened, since they name
their package; a CVE or FTBFS bug is never dropped. `--yes`
skips proposing entirely and keeps whatever you listed: attaching a bug
closes it when the update goes stable, and dropping one you asked for
is equally not something to do unprompted.

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
ebranch check-update epel9-build-side-134436 \
    --submit --type enhancement --bug 2482250 \
    --notes "Update uutils to 0.2 and rebuild dependent crates"
```

### Prune a staging COPR

A staging COPR such as `@rust/uutils-and-nushell` holds an update's
builds until they land in the real releases. `copr-prune` says which
of them have:

```sh
ebranch copr-prune @rust/uutils-and-nushell
```

For every package in the COPR and every release it builds for (the
chroot names the branch: `fedora-rawhide-*` is rawhide, `epel-9-*` is
epel9, `centos-stream-10-*` is c10s), the release's own version is
compared with the COPR's build:

```
COPR @rust/uutils-and-nushell: 42 package(s); target releases: epel9, rawhide

Caught up everywhere (3), safe to prune:
  - rust-phf_shared: epel9 0.14.0-1.el9 ≥ 0.14.0-1; rawhide 0.14.0-2.fc46 ≥ 0.14.0-1

Still in flight (39):
  - rust-phf: rawhide has 0.13.1-2.fc46, COPR 0.14.0-1 (ahead)
  - rust-uucore: epel9 absent, COPR 0.2.0-1; rawhide 0.2.0-1.fc46 ≥ 0.2.0-1
```

A package is prunable only when every release it builds for carries
the COPR's version or newer; a failed build, a release without the
package, or one still behind keeps it. On a terminal each prunable
package is offered for deletion (`copr-cli delete-package`, answered
one by one); `--yes` deletes them all without asking; `--json` prints
the plan as data and, like a non-terminal run, never deletes.

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

#### Resolving offline from a saved graph

`--graph PATH` takes a dependency graph that `poi-tracker deps` saved
for the source branch and answers the source side from it: providers
of capabilities the graph resolved, BuildRequires of packages it
walked as roots. Only the frontier — plus the target branch and the
base-distro guard, which are always live — goes to fedrq, and the
summary says how many lookups went each way. The graph is a snapshot
of the source branch; its periodic regeneration is the refresh.

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

## System-wide configuration

Settings are read from `/etc/ebranch/config.toml` first, then overridden
per key by `~/.config/ebranch/config.toml`, with command-line flags
overriding both. A system file alone is enough — no per-user file is
required — and either may also carry a `[defaults]` table pinning flag
defaults (see the root `DEVELOPMENT.md`).

A `[check-crate]` table lists crates to ignore in every run, direct
or transitive, as if they were not dependencies — Fedora almost
always drops benchmark harnesses and the like, and this keeps each
report honest about what will actually be packaged. Without a list,
the built-in benchmark set applies: `codspeed`,
`codspeed-bencher-compat`, `codspeed-criterion-compat`,
`codspeed-divan-compat`, `count_instructions`, `criterion`,
`criterion2`, `divan`, `iai`, `iai-callgrind`. A list in the file
*replaces* that set rather than adding to it, so a run that should
count criterion — someone packaging it — lists the others without it,
and `exclude = []` excludes nothing. The entry `"@default"` stands for
the built-in set, which is how a list adds to it (TOML has no `+=`):

```toml
[check-crate]
exclude = ["@default", "pretty_assertions"]
```

`--exclude` adds to whichever list is in force; all of them ignore the
crate outright, and `--verbose` says when the built-in set is the one
applying.

A `[check-crate.in-tree]` table carries the `--in-tree` list per crate,
keyed by crate name, so a workspace checked repeatedly needs no flag:

```toml
[check-crate.in-tree]
coreutils = ["uu_*", "uucore*", "uutests"]
```

`--in-tree` adds to the entry for the crate being checked.

A `[check-crate.staging-copr]` table names the staging COPR per crate,
so `--staging-copr` need not be typed for a crate whose update lives
there; the flag wins when given:

```toml
[check-crate.staging-copr]
coreutils = "@rust/uutils-and-nushell"
phf = "@rust/uutils-and-nushell"
```

`ebranch config` writes the user file only, with 700 on the
directory and 600 on the file. Nothing writes under `/etc`: a system
file is admin-authored, holds shared non-secret settings, and is
normally shipped `root:root 0644`.

Credentials belong in the per-user file, which is 600, or in an
environment variable — a token under `/etc` is readable by every local
user. For an unattended machine, give the job its own user and its own
600 config.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
