# poi-tracker

Package-of-interest tracker for Fedora, EPEL, and CentOS SIGs.

Manages TOML-based inventories of packages that an organization
tracks across distributions. Supports exporting to content-resolver
YAML and hs-relmon manifest formats.

poi-tracker is the **act** side of the inventory tooling: it
curates the inventory itself (sync, prune, add/remove) and takes
credentialed actions on Bugzilla (triaging, closing, and claiming
bugs). Its read-only counterpart is
[sandogasa-pkg-health](../sandogasa-pkg-health/), which **observes**
the same inventories — periodic, credential-free health checks with
persisted, incrementally-refreshed reports. Rule of thumb: anything
that writes (to Bugzilla or the inventory) belongs here; anything
that produces a report to watch over time belongs in pkg-health.

## Installation

```sh
cargo install poi-tracker
```

Some subcommands shell out to external tools: `prune-retired` and
`sync-distgit --mark-unshipped` need
[fedrq](https://src.fedoraproject.org/rpms/fedrq)
(`sudo dnf install fedrq`); `sync-gitlab --mark-unshipped` needs
`koji` configured with the `cbs` profile (from the
`centos-packager` package); `semver-audit` and `triage-updates`
use `koji` (`sudo dnf install koji`) to verify that a version is
actually tagged into a release before calling a bug stale — both
degrade with a warning when it's missing (unverifiable bugs are
left open / reported as up to date).

## Usage

### Add a package

```sh
poi-tracker add systemd -i inventory.toml \
    --poc "Team <team@example.com>" \
    --rpm systemd-networkd \
    --workload hyperscale \
    --track upstream
```

### Adopt orphaned packages

The action counterpart to
[sandogasa-pkg-health](../sandogasa-pkg-health/)'s orphaned flag:
walk the inventory, find packages whose dist-git owner is the
`orphan` sentinel user, show each one's orphaning reason, and take
ownership via the same API as the web UI's "Take" button. An
orphaned package is retired ~6 weeks after orphaning unless
someone adopts it.

```sh
poi-tracker -i inventory.toml adopt --dry-run   # list, no token needed
poi-tracker -i inventory.toml adopt             # prompt per package
poi-tracker -i inventory.toml adopt -y          # adopt all matches
```

Adoption is a per-package commitment, so interactive runs confirm
each package individually (default no) rather than batching one
yes/no over the list; `-y` adopts every match. `--pattern <glob>`
scopes the walk. Packages marked retired or unshipped in the
inventory are skipped — dist-git refuses to hand out retired
packages (those need a releng ticket).

Adopting needs a dist-git API token with the
"Modify an existing project" ACL (you must also be in the
`packager` group): generate one at
<https://src.fedoraproject.org/settings/token/new> and store it
with `poi-tracker config` (or pass `--api-token` / set
`PAGURE_API_TOKEN`). `--dry-run` works without a token.

### Configure (Bugzilla API key)

```sh
poi-tracker config
```

Prompts for a Bugzilla API key, validates it with a quick test
search, and saves it to `~/.config/poi-tracker/config.toml`.
Lookup order at runtime: `--api-key` flag → `BUGZILLA_API_KEY`
env var → config file.

Generate an API key at
<https://bugzilla.redhat.com/userprefs.cgi?tab=apikey>.

### Classify keeps by their dependents

The pruning report for shrinking a keep set over time — over the graph
a `deps --graph` run saved, classify every package of the given
inventories by who needs it:

```sh
poi-tracker -i essential.toml -i essential-rust.toml \
    dependents --graph fedora-build-deps-graph.json
```

**Leaves** have no dependent in the closure: one shipping real
binaries is presumably kept for its own sake, while a `[devel-only]`
leaf (every binary ends in `-devel` — the library shape) is kept for
nobody. **Carried** packages are needed by *other packages of the same
inventories* — if such an entry is only tracked as a dependency, stop
curating it: track the dependent and let the dependency walk carry it
in the derived inventory. The edge says whom the graph *could* carry
a package for, never why the entry exists, so carried lines wear the
same `[devel-only]` marker: a devel-only carried crate is almost
always dependency-only and safe to bulk-prune, while one shipping
real binaries (a tool whose satellite plugin is the dependent, say)
is likely kept on purpose — decide those per package. **Externally needed** ones are required only
by closure packages outside the inventories. Packages the graph never
saw are reported as unknown rather than guessed at (a stale graph — or
a stale keep: a package renamed upstream shows up as a leaf shipping
nothing). `--json` prints the report machine-readably.

### Inventory dependencies from other repos

Walk the transitive dependency graph of the inventory's
packages against a fedrq branch and repo stack, and collect what they
pull from repos of interest — e.g. which EPEL 9 packages a Hyperscale
inventory depends on:

```
$ poi-tracker -i hyperscale-packages.toml deps -b hs.el9 -r stack \
    -o hyperscale-epel9-deps.toml
1 runtime dependency from [epel] for 2 package(s) on hs.el9:
  python-zstandard (epel) — systemd-ukify requires python3dist(zstandard)
wrote hyperscale-epel9-deps.toml
```

A *capability* here is RPM's own term for the strings in `Provides:`
and `Requires:` — a package name, a versioned expression like
`python3dist(zstandard)`, a soname like `libhwloc.so.15()(64bit)`, or
a file path (`rpm -q --provides` lists a package's). The walk
repeatedly resolves the capabilities the current packages require
into the packages that provide them, the way dnf would.

Providers whose repo id matches `--base-repo` (a prefix, default
`fedrq-centos-stream-`) satisfy a dependency and end the walk there —
the base distro is a given. Every other provider is walked further,
and collected when its repo id is in `--from` (exact, default
`epel`). Those defaults describe an EPEL-on-CentOS-Stream stack; for
a Fedora walk there is no base distro beneath the branch, so the
default prefix never matches and the only setting needed is the
branch's own repo id, e.g. `-b rawhide --from rawhide`. The
output inventory records why each package was pulled in, in its
`reason` field. The walk also seeds the roots' own BuildRequires
(attributed `src:<name>`), so a kept application justifies its crate
graph — those edges exist only at build time. Build dependencies
beyond the roots are not walked: the closure keeps the roots working
and the roots rebuildable, not the world. `--runtime-only` skips the
BuildRequires seeding when the question is the deployment surface
rather than what must stay for the roots to survive.

`--fixpoint <inventory>` iterates to closure in one run: collected
packages found in that inventory (yours) become roots of their own,
their BuildRequires seeding further rounds until a round adds nothing
— because keeping a package means keeping it rebuildable, and its
test-only BuildRequires appear in no `-devel`'s runtime Requires.
Rounds reuse the walk's seen-sets, so each costs only the genuinely
new capabilities; the manual three-invocation equivalent measured ~90
minutes against rawhide where the in-process fixpoint is the first
walk plus small change. Conflicts with `--runtime-only`.

Whenever `-o` writes an inventory, the walk's full dependency graph
lands beside it as `<output>-graph.json` (`--graph` moves it) — every
requirer of every capability and every provider, where the report
keeps only first attributions. That is the input `unkeep` and
`dependents` answer from in milliseconds instead of a fresh walk, and
the periodic full walk is its refresh; a closure is never written
without it.

The shared walk filters apply: `--pattern` (glob, CSV or repeated),
`--start-from` and `--end-with` restrict which inventory packages are
used as roots — handy for a quick look at one package's pull before
walking a whole inventory.

Requires `fedrq`, with the branch configured — `hs.el9` and `hs.el10`
come from this repository's `configs/fedrq/`. Rich (boolean)
dependencies made only of conjunctions (`with`, `and` — the shape of
rust-packaging's crate version ranges) resolve by their leaves;
conditional ones (`or`, `if`, `unless`) are skipped with a warning.
File dependencies resolve
and classify normally, but appear under `unmatched` in `--json`
output, since tying a path to its provider would need file lists.

### Export to content-resolver YAML

```sh
# Export all workloads (one YAML per workload)
poi-tracker export content-resolver -i inventory.toml

# Export a single workload
poi-tracker export content-resolver -i inventory.toml \
    --workload hyperscale

# Custom output path (single workload only)
poi-tracker export content-resolver -i inventory.toml \
    --workload hyperscale -o custom.yaml
```

### Export to hs-relmon manifest

```sh
# Merge multiple inventories into one manifest
poi-tracker export hs-relmon \
    -i inv-cloud.toml -i inv-hw.toml -o manifest.toml

# Filter by workload
poi-tracker export hs-relmon -i inventory.toml \
    --workload hyperscale -o manifest.toml
```

Existing manifest entries and their per-package overrides are
preserved across re-exports. An `archived_builds` package gets
`archived = true` written so hs-relmon can prune its stale CBS
builds; an `unshipped` package (gone, no builds) is dropped from
the manifest entirely — hs-relmon has nothing to track for it
(the inventory keeps the tombstone). Both are reconciled every
export, so a revived package returns to normal tracking.

### Find a package

```sh
poi-tracker find systemd -i inv1.toml -i inv2.toml
```

### Import from legacy JSON

```sh
poi-tracker import old-inventory.json -o inventory.toml \
    --private-fields poc,reason,team,task \
    --workload hyperscale
```

### Intersect inventories

Keep only the main inventory's packages that also appear in the
`--with` inventories, optionally merging them into another file. The
motivating case: a `deps` closure lists everything a keep-set
needs — thousands of packages, most of them other people's — and the
durable fact is its intersection with the packages *you* maintain.
Entries come from the main (`-i`) side, so `deps`' reason chains
survive into the merge target, and the essentials file explains
itself:

```sh
poi-tracker -i fedora-build-deps.toml intersect --with personal.toml \
    -o my-essential-deps.toml
```

`-o` accumulates like kondo's: existing entries win, new ones are
added sorted.

### Triage cull candidates (kondo)

The set difference between the inventory ("packages I maintain") and
the union of *essential* inventories — work inventories, and the
dependency inventories `deps` writes — is the list of packages nothing
justifies. `kondo` walks that list with the shared keep/explain/remove
prompt, under this reading: the finding is "nothing essential needs
this package", so **keep** confirms it as a cull candidate, **explain**
files the package into another inventory (the explanation *is* the
inventory path; the file is created if needed), and **remove** drops a
false positive. Filing is immediate, so an interrupted triage loses
nothing already decided.

```sh
poi-tracker -i personal.toml kondo --user salimma \
    --essential inventory-hyperscale.toml \
    --essential hs-el9-deps.toml --essential hs-el10-deps.toml \
    -o cull.toml
```

Confirmed candidates are then classified by your own (direct) dist-git
access, because the level routes the action — owner: orphanable
directly; admin: you can remove your own ACL; commit, collaborator or
ticket: you have to ask. The report groups them accordingly and prints
ready-to-run `sandogasa-pkg-acl` command lines for the first two
groups, in a form fit for a mailing-list announcement — which comes
first: `kondo` itself never touches dist-git ACLs. Group-granted
access is deliberately ignored (pair with `sync-distgit --no-groups`);
a group grant is not yours to walk away from.

Access levels are looked up before the prompt (answers are cached for
a day under `~/.cache/poi-tracker/`, since ownership changes on human
timescales — `--refresh-acls` forces a fresh look), so each candidate
line carries its context — `old-toy (commit) — nothing essential needs it`
reads differently from `(owner)`.

`--explain-into PATH` sets a default: Enter at the explanation prompt
files the package there, so a pass that sorts many packages into one
inventory is two keystrokes each (`e`, Enter); an explicit path still
wins.

The cull file records why each package is condemned. The stock
`reason` is `kondo cull candidate (<level>)`; `--reason TEXT` replaces
the wording for packages culled this run (the sitting usually has one
shared story — "retiring my GNOME extensions"), and `k <note>` at the
prompt records this one package's own words instead. The access level
is always appended, entries already in the file keep their reason, and
only names are ever read back — so hand-editing a reason later is
safe. The natural multi-pass flow is one pass per destination, passing
the same file as `--essential` too — filed packages then never
reappear as candidates. Candidates are computed once, up front, so
filing into an essential inventory mid-run is safe; and when the
`--explain-into` file does not exist yet, it is tolerated as an empty
essential input rather than failing the load. Any *other* missing
essential path is still an error — treating a typo as an empty
inventory would make every package it names look cullable.

The shared walk filters (`--pattern`, `--start-from`, `--end-with`)
restrict the candidates, which is how a large triage gets eaten in
sittings: one themed pass at a time (`--pattern 'rust-*'
--explain-into keep-rust.toml`), with `--start-from` as the resume
point — candidates are sorted, and access lookups run a few at a time
so even hundreds classify in under a minute. `-o` merges rather than
overwrites, so every pass adds its slice of the verdict to one cull
inventory; packages already present are left untouched — and consulted:
a candidate a previous pass already culled is skipped, not re-asked, so
re-running with the same `-o` only ever prompts for what is genuinely
undecided. That makes the file itself the undo mechanism: delete an
entry (a mistaken keep, say) and the next run asks about that package
again. The reverse correction is automatic: a culled package that has
since become essential — after a `deps` run justified it, say
— is rescued from the file and reported, so the verdict never
contradicts the inputs. `remove` decisions are deliberately not persisted — a remove
is a temporary skip, and the dropped candidate returns on the next run
until whatever the analysis missed is fixed in the essential inputs. Sessions
running at the same time should still write distinct files — the merge
is load-modify-save, so simultaneous finishes can drop each other's
additions — and let a later pass fold them together.

Without a terminal, with `--json`, or with `-y`, no prompt fires and
every candidate stays a candidate — which acts on nothing.

### Mark packages no longer shipped anywhere

`prune-retired` finds inventory packages that are no longer
carried on **any** active branch — the dist-git project is gone
(404), it has no branch on an active release, or it carries a
`dead.package` marker on every active branch it has. The active
branch set is queried from Bodhi's active releases (plus
rawhide) or overridden with `--branch`:

```sh
poi-tracker -i inventory.toml prune-retired --dry-run
poi-tracker -i inventory.toml prune-retired
```

By default matches are *marked* with an `unshipped` reason in
the inventory rather than deleted: retired packages keep their
ACLs, so a deleted entry would come straight back on the next
`sync-distgit` run, and the marker is what lets the rest of the
tooling do the right thing. `triage-updates` and `semver-audit`
skip unshipped packages; `triage-retired` still processes them
so their remaining bugs get closed; the sync commands' `--prune`
preserves them. Markers are refreshed in both directions — a
revived package gets its marker cleared. Pass `--remove` to
delete the entries outright instead. Packages are checked
concurrently (`-j`/`--jobs`, default 8 in-flight dist-git
requests).

An entry with no `rpms/` dist-git project (404) is reported as
**invalid** rather than marked: the entry itself is wrong — a
non-RPM repo (module, container image, tests) imported under its
bare name by an older group sync (e.g. `modules/askalono-cli`
showing up as `askalono-cli`), a *binary subpackage* name
recorded instead of the source package, or a typo. The fix is
editing or removing the entry, which is a human call. A stale
`unshipped` marker on such an entry is cleared by the next run.

`sync-distgit --mark-unshipped` runs the same check on the
packages a sync adds, so a fresh inventory starts with its
`unshipped` markers in place instead of needing a follow-up
`prune-retired` run. This catches retired packages, which keep
their ACLs and so still appear in sync listings. A package whose
dist-git project was deleted outright never appears in a listing
at all — harmless for a fresh inventory (it simply isn't added),
but if an existing inventory recorded it before the project
vanished, only `prune-retired` notices the 404.

### Remove a package

```sh
poi-tracker remove systemd -i inventory.toml
poi-tracker remove systemd -i inventory.toml --rpm systemd-networkd
```

### Audit pending updates by semver impact

`semver-audit` looks at each maintained package's pending upstream
release notification (the open `upstream-release-monitoring@`
"X is available" bug) and classifies the version bump against the
version currently packaged in rawhide dist-git, so you can see
which updates are safe to push and which need care:

```sh
# All pending updates, grouped by impact
poi-tracker -i inventory.toml semver-audit

# Just the safe ones for your Rust packages
poi-tracker -i inventory.toml semver-audit --pattern 'rust-*' --non-breaking

# Machine-readable
poi-tracker -i inventory.toml semver-audit --json
```

Bumps are classified with Cargo's compatibility rule (the Rust
convention): a change at or before the version's leftmost non-zero
component is **breaking**. So `1.4 → 1.5` is non-breaking, but
`0.4 → 0.5` is breaking (pre-1.0 minor bumps can break), and
`0.0.3 → 0.0.4` is breaking too. Versions that aren't plain dotted
integers — pre-releases, dates, git snapshots — are reported as
**needs review** rather than guessed at. A package whose packaged
version already equals the "available" version is reported as
**up to date (stale bug)** — but only after verifying (via
`koji`) that a build with that version is actually in rawhide's
tag chain. A version merely committed to dist-git whose build
sits in a side tag or is still gating is reported as
**committed, awaiting release** instead: the bug isn't stale,
the update just hasn't shipped. A package that's retired on rawhide (a
`dead.package` marker — the same signal `triage-retired` uses) is
reported as **retired (update request invalid)**, since there's no
live package to update; run `triage-retired` to close those bugs.

`--pattern <glob>` (comma-separated or repeated, e.g. `rust-*`)
limits the audit to matching packages, and `--non-breaking` shows
only the safe updates. The audit makes a Bugzilla search and a
dist-git spec fetch per matching package, so scope it with
`--pattern` for a large inventory — or use `--batch [EMAIL]`,
which replaces the per-package searches with **one** Bugzilla
query for all open release-monitoring bugs assigned to or CC'ing
EMAIL (default: the email set via `poi-tracker config`), matched
against the inventory locally. Batch mode misses bugs where that
email is neither assignee nor CC'd, so it fits inventories of
packages you (co-)maintain or watch.

### Show inventory

```sh
poi-tracker show -i inventory.toml
poi-tracker show -i inventory.toml --workload hyperscale
poi-tracker show -i inventory.toml --json
```

### Sync from dist-git

Create or update an inventory from packages a user or group has
access to on Fedora dist-git (Pagure). Re-running merges new
packages without overwriting existing entries or annotations.

```sh
# All packages for a user
poi-tracker sync-distgit --user salimma -o my.toml

# All packages for a group
poi-tracker sync-distgit --group kde-sig -o kde.toml

# Exclude packages with only group-based access
poi-tracker sync-distgit --user salimma --no-groups

# Only packages from specific groups
poi-tracker sync-distgit --user salimma \
    --include-group rust-sig,python-packagers-sig

# Exclude specific groups
poi-tracker sync-distgit --user salimma \
    --exclude-group rust-sig

# Add workload tags to all imported packages
poi-tracker sync-distgit --group kde-sig \
    --workload kde -o kde.toml

# Remove packages no longer in dist-git results
poi-tracker sync-distgit --user salimma --prune -o my.toml

# Resume an interrupted sync from f*, stop before m*
poi-tracker sync-distgit --user salimma \
    --start-pattern f --end-pattern m
```

User syncs query Pagure one name prefix at a time (`a*`–`z*`,
`0*`–`9*`) by default: an unfiltered per-user project query is too
expensive for Pagure to answer within its gateway timeout, so it
returns HTTP 504. Splitting the query by name prefix keeps each
request small enough to succeed. (See
[`sandogasa-distgit`'s development notes](../../crates/sandogasa-distgit/DEVELOPMENT.md)
for the details — including why group syncs need no such
workaround.) `--pattern` switches to a single patterned query
instead, and `--no-auto-prefix` forces a single unfiltered query. `--start-pattern` / `--end-pattern` bound the prefix scan
(e.g. to resume an interrupted sync: start at this prefix / stop
before this prefix) and imply prefix mode, as does
`--auto-prefix` — which is how a group sync opts into scanning.
If both `--auto-prefix` and `--no-auto-prefix` are given, the
last one wins.

Packages where the user has both direct and group-based access
are always included, regardless of group filters.

Without `--prune`, packages in the inventory that are no longer
in the dist-git results are listed as a warning but kept.

Transient network failures are retried with backoff (both 5xx
responses and connection errors). If a fetch still fails, the
progress so far is saved to `<output>.partial` along with the
failed pattern in `<output>.partial.state` — re-running the same
command resumes from the failed pattern, and a completed run
replaces `<output>` and removes both files. Delete the
`.partial` to start over instead.

For user syncs, `--fast` replaces the whole prefix scan with one
request against Pagure's owner-alias dump. The trade-off: the
dump only records direct owner/admin/commit maintainers, so
collaborator- and ticket-level grants won't appear (and
`--prune --fast` would *remove* them from an inventory the full
scan had populated). It implies `--no-groups`; `--pattern` and
`--exclude` still apply, client-side.

A fast layout that covers nearly everything: keep one `--fast`
inventory for your own packages plus one group inventory per SIG
you're in (each a single cheap query) —

```sh
poi-tracker sync-distgit --user salimma --fast -o mine.toml
poi-tracker sync-distgit --group rust-sig -o rust-sig.toml
poi-tracker sync-distgit --group go-sig -o go-sig.toml
```

Together those cover everything except user-level
collaborator/ticket grants, which only the full prefix scan can
see — run one occasionally to true up. The full trade-off
analysis lives in
[`sandogasa-distgit`'s development notes](../../crates/sandogasa-distgit/DEVELOPMENT.md).

### Sync from GitLab

Create or update an inventory from a CentOS SIG's GitLab RPM
group. Presets cover the common groups:

```sh
poi-tracker sync-gitlab --preset hyperscale -o hyperscale.toml
poi-tracker sync-gitlab --preset proposed-updates -o pu.toml
poi-tracker sync-gitlab --url https://gitlab.com/CentOS/Hyperscale/rpms
```

`--mark-unshipped` cross-checks each project against CBS (CentOS
koji) and records archival state. A project's GitLab repo being
**archived** means upstream maintenance stopped; what happens
next depends on whether CBS still carries the package:

- **archived, no released CBS build** → marked `unshipped` (a
  tombstone, skipped by triage/audit like a retired Fedora
  package).
- **archived, but release builds remain** → marked
  `archived_builds`: it still ships, so it is *not* skipped, but
  its lingering builds are a cleanup candidate — the command
  prints a reminder to run `hs-relmon` to prune them.

"Released" follows each SIG's lifecycle: Hyperscale ships for
both RHEL `N` and CentOS Stream `Ns`, so a release build in
either `hyperscaleN-*-release` or `hyperscaleNs-*-release`
counts; Proposed Updates is Stream-only. `--centos-release` sets
which major releases count (default `9,10`). Requires `koji` with
the `cbs` profile. Markers are refreshed in both directions on
each run.

### Close retired packages' update bugs

When a package gets retired on a dist-git branch (a
`dead.package` file is committed), any open release-monitoring
bug for that branch is dead weight — there's no live spec to
update. `triage-retired` walks the inventory, checks dist-git
for retirement, and closes those bugs as `CLOSED/CANTFIX`:

```sh
poi-tracker -i inventory.toml triage-retired --dry-run
poi-tracker -i inventory.toml triage-retired
```

The `--branch` flag controls which dist-git branch(es) are
checked (default `rawhide`); each branch scopes its own Bugzilla
search, so an `epel9` retirement closes the
`Fedora EPEL`/`epel9` bug. Pass it more than once (or as a
comma-separated list) to check several branches in one run — a
package retired on some branches but live on others only has
its bugs closed for the branches where it's actually dead:

```sh
poi-tracker -i inventory.toml triage-retired --branch epel10
poi-tracker -i inventory.toml triage-retired --branch epel8,epel9
```

Note that retirement (a `dead.package` marker) is distinct from
"never existed": a bug filed against a branch the package was
never built for is *not* a retirement and is left untouched —
`triage-retired` only closes bugs on branches where a
`dead.package` is present.

By default only release-monitoring bugs (filed by the Anitya /
the-new-hotness bot) are closed — those are mechanical and safe
to bulk-close. Pass `--all-reporters` to instead close **every**
open bug on the retired branch, including human-filed ones (CVEs,
FTBFS, etc.). Use it deliberately: across a full inventory run it
closes a lot, and a CVE filed only against the retired branch
(with no live-branch counterpart) would be closed as CANTFIX too:

```sh
poi-tracker -i inventory.toml triage-retired \
    --branch epel8,epel9 --all-reporters
```

Bugs that are already `CLOSED` are skipped. Each closure adds a
short comment naming the package and the retired branch.

Interactive runs offer to claim ownership of each closed bug
(set `assigned_to` to your configured Bugzilla email). Pass
`--claim` to claim without prompting — under `-y` this is the
only way to opt in. The email is set via `poi-tracker config`.

Pass `--mark` (needs a single `-i` file; conflicts with
`--dry-run`) to record the run's findings in each package's
`retired_on` field — in both directions, so a branch found live
again is removed. `semver-audit` and `triage-updates` skip
packages marked retired on rawhide, saving their per-package
queries; re-running `triage-retired --mark` is how the markers
are refreshed.

Useful flags for big inventories (shared by all
inventory-walking commands — `semver-audit`, `triage-retired`,
and `triage-updates` — and freely combinable):

- `--pattern <glob>` — only process matching packages
  (comma-separated or repeated; a bare name matches exactly,
  e.g. `--pattern python-django3` to check a single package).
- `--start-from <name>` — resume from this package onwards in
  the inventory's iteration order, e.g. to continue an
  interrupted run.
- `--end-with <name>` — stop after this package (inclusive).
  Combine with `--start-from` to scope to a name-range, e.g.
  `--start-from rust-nu-cli --end-with rust-nu-utils` to test
  the change against every `rust-nu-*` package in one shot.
- `--batch [EMAIL]` — one Bugzilla query for everything assigned
  to or CC'ing EMAIL (default: the configured email) instead of
  one query per retired package per branch; with
  `--all-reporters` the batch query drops the reporter filter
  too.

Network reads (dist-git probes, Bugzilla searches) retry up to
3 times with exponential backoff, so a transient connection
hiccup against `src.fedoraproject.org` doesn't abort the whole
inventory.
### Triage update bugs

Some packages reliably need attention when a new upstream version
appears — `python-django*` updates almost always fix CVEs, for
instance. Mark them in the inventory with a `priority` field (or
a workload-level `default_priority`), then have poi-tracker
triage the auto-filed release-monitoring bugs by raising their
Bugzilla priority:

```sh
poi-tracker -i inventory.toml triage-updates --dry-run
poi-tracker -i inventory.toml triage-updates
```

For each inventoried package with a resolved priority, this
queries OPEN bugs reported by `upstream-release-monitoring@
fedoraproject.org` (against `Fedora` and `Fedora EPEL`) and
raises any whose priority is `unspecified`. Bugs already
triaged by a human are left alone.

Per-package `priority` wins over `default_priority`; if a
package is in multiple workloads, the highest workload
default applies. Set `priority = "unspecified"` on a package
to explicitly opt out of a workload default.

Independently of priorities, every open release-monitoring bug
is also checked against Bodhi for builds that already carry the
advertised version (or newer). When found, the latest addressing
build per release is recorded in the bug's **Fixed In Version**
field, and:

- stable in **every** active release the package has a branch
  for → the bug is closed as `ERRATA`, with a comment listing
  the Bodhi updates;
- any addressing update still in **testing** → the bug is moved
  to `MODIFIED` (a later run closes it once everything is
  stable);
- addressed only in **some** releases (commonly just rawhide,
  since stable branches often intentionally stay behind) → you
  are asked before closing. `--close-stale` closes these without
  asking; under `-y` they are skipped unless `--close-stale` is
  given.

Bodhi records updates per release, so a release can carry a build
Bodhi has no update for in that release — content inherited at
branching, or answers missing while Bodhi is degraded. For those,
the release's Koji tag chain is consulted (via `koji`, checking
`fXX-updates` + `fXX` and the EPEL equivalents through tag
inheritance), so years-old stale bugs close too — while a version
merely committed to dist-git and built into a side tag or
`-candidate`/`-testing` tag correctly stays pending. Without the
`koji` CLI this check is skipped (with a warning) and such bugs
are simply left open. Genuinely pending bugs are cheap: rawhide is
checked first, and since a stable release may never carry a newer
version than rawhide, a version absent from rawhide skips the
stable-release queries entirely (EPEL bugs, whose branches update
independently, are always checked in full). Pass `--skip-stale`
to disable the whole check (also restoring the cheaper
priority-only scan), and `--pattern <glob>` (e.g. `rust-*`) to
scope the run. `--batch [EMAIL]` works as in `semver-audit`: one
Bugzilla query for everything assigned to or CC'ing EMAIL
(default: the configured email) instead of one query per package.

As in `triage-retired`, interactive runs offer to claim ownership
of the bugs being closed (set `assigned_to` to your configured
Bugzilla email). Pass `--claim` to claim without prompting —
under `-y` this is the only way to opt in. Bugs moved to
`MODIFIED` keep their assignee: they stay open and belong to
whoever owns the in-flight update. The email is set via
`poi-tracker config`.

### Stop keeping packages (unkeep)

Remove packages from the keep inventories and learn, without re-walking
anything, what else stops being needed — over the dependency graph a
`deps --graph` run saved:

```sh
poi-tracker -i essential.toml -i essential-rust.toml \
    unkeep neovim GraphicsMagick \
    --graph fedora-build-deps-graph.json \
    --deps essential-deps.toml
```

The report names each freed package with its former requirers; adding
`--apply` removes the unkept packages from the `-i` keep inventories
and the freed ones from the `--deps` derived inventories, so the next
`kondo` run offers them all as candidates. A package the remaining
keeps still reach is a warning, not a free — culling it would be
rescued right back.

Reachability follows every provider the walk recorded, so nothing is
freed while any alternative chain still holds, and a derived package's
`src:` build-dependency edges lapse with it, so test-only build
dependencies cascade out the way the fixpoint pulled them in. The
graph is a snapshot: the periodic full `deps --graph` walk is its
refresh. `--json` prints the report machine-readably.

### Validate

```sh
poi-tracker validate -i inventory.toml
```


## Inventory format

```toml
[inventory]
name = "hyperscale-packages"
description = "CentOS Hyperscale SIG packages"
maintainer = "centos-hyperscale"
labels = ["eln-extras"]
private_fields = ["poc", "reason", "team", "task"]

[inventory.workloads.hyperscale]
name = "hs-packages"
description = "Hyperscale SIG workload"
labels = ["eln-extras"]

[inventory.workloads.epel]
name = "hs-epel-packages"
description = "Hyperscale EPEL workload"

[[package]]
name = "systemd"
poc = "Linux Userspace <team@example.com>"
reason = "Core init system"
rpms = ["systemd-networkd"]
workloads = ["hyperscale"]
track = "upstream"

[package.arch_rpms]
x86_64 = ["systemd-boot-unsigned"]
aarch64 = ["systemd-boot-unsigned"]

[[package]]
name = "fish"
rpms = ["fish"]
workloads = ["hyperscale", "epel"]
track = "upstream"
```

### Fields

| Field | Level | Description |
|-------|-------|-------------|
| `name` | inventory/package | Name (required) |
| `description` | inventory | Human-readable description |
| `maintainer` | inventory | Maintainer (person or team) |
| `labels` | inventory | Default labels for content-resolver |
| `workloads` | inventory | Workload definitions (map) |
| `workloads` | package | Workload membership (list) |
| `private_fields` | inventory | Fields stripped on export |
| `poc` | package | Point of contact |
| `reason` | package | Reason for tracking |
| `team` | package | Team responsible |
| `task` | package | Internal task/ticket |
| `rpms` | package | Binary RPMs to track |
| `arch_rpms` | package | Architecture-specific RPMs |
| `track` | package | hs-relmon tracking branch |
| `repology_name` | package | Repology name override |
| `distros` | package | hs-relmon distribution list |
| `file_issue` | package | File GitLab issues |
| `priority` | package | Bugzilla priority for `triage-updates` (`unspecified`/`low`/`medium`/`high`/`urgent`) |
| `retired_on` | package | Dist-git branches where the package is retired; written by `triage-retired --mark` |
| `unshipped` | package | Reason the package is no longer shipped on any active branch; written by `prune-retired`. Skipped by most operations, still processed by `triage-retired`, preserved by sync `--prune` |
| `archived_builds` | package | Reason an archived-upstream package still has CBS builds; written by `sync-gitlab --mark-unshipped`. Still ships (not skipped); a build-cleanup candidate for `hs-relmon` |
| `default_priority` | workload | Default Bugzilla priority for packages in this workload |

Each `[inventory.workloads.<key>]` section can override `name`,
`description`, `maintainer`, `labels`, and `default_priority`
for content-resolver export and `triage-updates`. Omitted
fields fall back to inventory-level values.

## System-wide configuration

Settings are read from `/etc/poi-tracker/config.toml` first, then
overridden per key by `~/.config/poi-tracker/config.toml`, with
command-line flags overriding both. A system file alone is enough — no
per-user file is required — and either may also carry a `[defaults]`
table pinning flag defaults (see the root `DEVELOPMENT.md`).

`poi-tracker config` writes the user file only, with 700 on the
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
