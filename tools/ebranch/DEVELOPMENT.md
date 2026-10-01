# ebranch — development notes

Design decisions and rules future work must follow. The README
describes the tool as it is; this file says why some of it is that way.

## check-update's local-repo fallback stands alone, like a side-tag repo

A pending Bodhi update — or one pushed to testing that the mirrors do
not carry yet — used to get the reduced report ("Provides can't be
compared"), because neither `@testing` nor a side tag nor a COPR had
its builds. Its builds are in koji, though, so `check-update` downloads
them and indexes a local repository as the "new" side. Rules that fall
out of how the pieces behave, each checked on a live update
(FEDORA-2026-a67d0ddb5f, uxplay, 2026-09-18):

- **`-r @baseurl:file://<dir>` replaces the branch's repositories**, it
  does not add to them: `fedrq pkgs -b f45 -r @baseurl:… -F nevr uxplay`
  returned only the downloaded 1.73.7, while the same query without `-r`
  returned the stable 1.73.3. So the local repo is queried with no
  branch, exactly like `@koji:<side-tag>`, and the Provides diff cannot
  be polluted by the old version. Had it merged, every old Provide would
  still have been "present" and no update would ever have been detected.
- **Download with `bodhi updates download --updateid <alias>` and no
  `--arch`.** bodhi's client then passes `--arch=noarch --arch=<host
  machine>` to `koji download-build`; an explicit `--arch x86_64` passes
  only that and silently drops noarch subpackages. It also keys the
  download by the alias `check-update` already holds, so there is no
  per-NVR loop to get wrong.
- **Reuse the side-tag arm's comparison, skip its staleness check.**
  `compute_changed_provides_via_koji` takes binary names from `koji
  buildinfo` and Provides from whatever repo it is handed; a repo built
  from koji's own RPMs cannot lag koji, so the regen-repo machinery does
  not apply.
- **Cache under `$XDG_CACHE_HOME/ebranch/update-repos/<alias>/`** (the
  `dirs` crate, per the workspace rule), with the sorted NVR list beside
  the repodata: same list, reuse; different list, remove and rebuild so
  no RPM from a superseded build lingers. The alias is validated to the
  characters a Bodhi alias has before it becomes a path component.
- **Both tools are preconditions**, checked with `require_tools` before
  anything is downloaded; a missing tool or a failed download prints why
  and falls through to the reverse-deps-only report rather than aborting
  the run.

## check-crate follows every real dev dependency; benchmarks are excluded, not capped

`check-crate --transitive` expands the dependencies of every crate that
would have to be packaged, including their dev dependencies, and their
dev dependencies' dependencies in turn. That is intentional: a dev
dependency is what the crate's tests need, Fedora runs those tests in
`%check`, so a real dev dependency is something to package.

It also means one benchmarking harness can dominate a report. The
uutils-coreutils check once listed 483 transitive-missing crates, 473
of them behind `codspeed-criterion-compat` — parse_datetime's dev
dependency — through `smol`, `surf`, `plotters` and their own dev
dependencies. The fix for that is the `[check-crate] exclude` list in
the config file (criterion, `codspeed-*`, divan, iai,
count_instructions, …): Fedora drops benchmark machinery from the
build, so check-crate should not count it at any level.

Do not "fix" this with a depth or kind rule — dev dependencies only for
the root, or only one level down. When a report balloons, find the
entry point (the `transitive_edges` in the saved TOML give the paths)
and suggest an exclude entry for it.

The common harnesses are built in (`config::DEFAULT_EXCLUDES`) so a
fresh install gets a sane report. A configured list *replaces* the
built-in one rather than merging with it — the same "user wins per
key" rule the /etc-beneath-~/.config layering follows — because the
one person who wants to package criterion must be able to un-exclude
it, and a merge would leave them no way to. Adding to the set is done
inside the same list, with the `"@default"` entry standing for it —
TOML has no `+=`. So: keep the built-in list to crates Fedora never
packages as dependencies, and never add a second, merged list on top
of it.

## A branch request is matched by its summary, not by who filed it

`file-requests` adopts an open request it did not file. The match is
`Please branch and build <package> in <branch>` parsed out of the
summary, with the release compared by family so a request naming
`epel10` answers a report targeting `epel10.4`, and with the package
required to be the component the bug was filed against — a request
mentioned in another package's bug is not that package's request.

Two rules follow for anything built on this:

- **Never key adoption on the filer, the creator, or a marker this tool
  writes.** The requests worth adopting are exactly the ones nobody here
  filed: rhbz#2368920 for `et` and rhbz#2367248 for `rust-tiny-dfr` were
  both opened by hand, months before the tool saw those packages.
- **An adopted request is somebody else's bug.** Its `depends_on` may
  already carry links a human added, so `link_requests` adds to that
  graph (`{"depends_on": {"add": …}}`) and never sets it. `escalate`
  dates a request from `bug.creation_time`, so adopting an old request
  makes it immediately escalatable; that is correct, and it is why
  adoption asks before it happens when a terminal is there to ask.

## A resolver test states a package universe, not an expected answer

`testrepo` describes repositories — names, sources, versions, Provides
and files — and lets the resolver query them. Prefer it to the older
`MockResolver`, which maps a dependency string straight to the source
package that answers it.

The distinction is not stylistic. Three bugs lived precisely in what
that mapping cannot express (#15, #16, #21), and every one of them
passed the old tests:

- fedrq answers a **batch** with the union of the providers it found,
  saying nothing about which dependency each came back for. Attribution
  happens afterwards, in our code, and that is where the bugs were.
- A package's **Provides carry versions but never file paths**, so a
  path can only be answered by the file list and can never be
  attributed from a batch.
- A capability's version is **its own**, not the package's:
  freetype-devel 2.13.2 provides `pkgconfig(freetype2) = 26.1.20`.

So the fixture matches dependencies itself rather than calling
`PkgInfo::satisfies`, which is one of the things under test — a fixture
agreeing with the code by construction passes whatever the code does.
Keep it that way when extending it, and check a new test fails with the
fix reverted before trusting it.

## An external query goes behind a trait, so the flow around it can be tested

`file-requests` asks a `SourceProbe` what a branch has, rather than
calling fedrq itself. A run passes the fedrq-backed probe; a test passes
a table of which branch holds which package, at which version, in which
repo class — the repo class included, because a package waiting in
updates-testing is branched and a pre-flight that misses that files a
duplicate.

That is what makes the whole filing batch testable: one run over a real
closure shape, with Bugzilla on loopback, asserting that a package the
base distro owns, one already branched, and one nobody has packaged all
stay out of Bugzilla, while an open request is adopted and the one
remaining package is filed. Mount exactly the writes you expect, so an
unexpected one fails the test rather than passing quietly.

Apply the same shape to the queries still called directly — check-update
drives fedrq, Koji and Bodhi, check-crate drives crates.io — when their
flows need covering. The decision code is worth testing; the transport
is not.

## Only dist-git's own access list decides whether to act on a package

`file-requests` offers to branch a package when the project records
the asker as owner, admin or commit — or as a collaborator whose
branch scope reaches the branch in question. Collaborator is in that
list deliberately: it is how EPEL access is usually granted, and
refusing it would send the common EPEL case back to filing a bug for
work the asker could do. But the scope is a branch pattern and is not
always set up to reach the branch at hand, so it is checked rather
than assumed: the project endpoint names collaborators without their
scope, and `/contributors` carries it. Match it the way Pagure does
(`is_repo_collaborator` in `pagure/utils.py`): split on commas, trim,
and glob each pattern against the branch name. A scope that cannot be
read is not an offer.

Do not reach for `/hascommit?user=&branch=` here, however exactly it
seems to phrase the question. It answers for a provenpackager on every
package in Fedora, so it cannot tell access on this project from the
blanket right below — it returns true for `kernel` and `bash`.

Two things that look like access are not, and must not be folded in:

- **Provenpackager** is the right to *build* any package, not to
  branch one, so it cannot answer this question at all. It is declared
  in the config rather than looked up, and all it ever does is add a
  line to a request for a build of an already-branched package saying
  the asker could do that build in an emergency. Fedora's convention
  is that this is the exception; do not make it something the tool
  offers, prompts for, or acts on.
- **Group membership.** A group on the project (`rust-sig` holds commit
  on rust-tokio) does grant its members access, but reading it needs
  the asker's group list, which this does not fetch. Such a package is
  offered no branch and gets a request instead — the safe direction.

Acting is offered, never assumed. The branch is requested only by an
interactive run, from a prompt defaulting to no, and the bug is a
second question with its own prompt — a dry run says both questions
are coming rather than answering them, and a run with nothing on stdin
files the bug as the command always did. `fedpkg` is probed with
`help`: `fedpkg --version` exits 2 with a usage message.

A request the asker files about a package they could have branched is
assigned to them as it is filed (`[bugzilla] email`), never reassigned
afterwards: a second write would mail everybody watching the package
to say the asker had claimed their own bookkeeping. Same reason the
`depends_on` links are sent with `minor_update`.

## Drawing the request graph must not be able to lose a filed bug

`file_batch` writes the bug IDs it filed back to the report only after
`link_requests` returns, so a link failure that propagated would
discard them — bugs are on a public tracker by then, and the report no
longer knows about them. Linking therefore reports a refusal and
carries on: each bug gets one update carrying all its edges, with
`minor_update` so a graph edge does not mail the package's watchers,
and Bugzilla is unreliable enough under a batch of writes that one
refusal says nothing about the next. The writes are additive and
idempotent, so re-running `file-requests` fills in what is missing.
