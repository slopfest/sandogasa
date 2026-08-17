# Changelog

## Unreleased

### koji-lag: `report --csv` and `reports --csv`, a file per table

The statistics a report computes are now available as CSV, which is what
the Fedora Data WG wants to work with. One file per table —
`all-builds.csv`, `srpm-rebuild.csv`, `multi-arch.csv`, `single-arch.csv`,
`noarch-by-host.csv`, plus `official.csv` and `scratch.csv` where the split
applies — because a CSV holds one table, while `report.txt` and
`report.json` carry every table for a period together. They are written
beside those, not instead of them, and `reports --csv` does the same for
every period it pools.

Three deliberate differences from the printed tables. Durations are plain
seconds, to the millisecond: `2.6m` is for a person, and a column mixing
minutes and hours cannot be summed, while microseconds are finer than a
build queue is measured. Nothing is withheld for having few samples —
`--min-samples` stops a *reader* over-reading three tasks, whereas a
consumer wants the three and the count beside them. And every row repeats
its instance and period, so a year of daily files concatenates into
something that still knows which day each row belongs to.

Empty tables are written rather than skipped: "no noarch builds this week"
is a finding, and a missing file cannot be told apart from a run that
failed halfway. Asking for `--csv` over a tree that already has
`report.txt` and `report.json` writes the CSVs rather than reporting the
period as already present, which would be true in a sense nobody asked
about.

While there: a report over a store recorded no period at all in its JSON,
because the store had already applied it. It now reports the period it
covers whatever set it.

### koji-lag: a store can be copied while a sync is writing to it

`scripts/backup-store.sh SOURCE DEST` (also `make backup-store STORE=…
DEST=…`) copies a store through SQLite's `VACUUM INTO` rather than `cp`.

The distinction is not pedantry. The store is in WAL mode, so at any moment
committed data lives partly in `lag.sqlite` and partly in
`lag.sqlite-wal`: copying the main file alone can capture a database
missing its most recent transactions, and copying all three while a writer
is mid-commit can capture an inconsistent set. Neither failure announces
itself — the copy opens and queries fine, with rows quietly missing — and
the thing being copied is hours of hub time that cannot be reconstructed
from anywhere except Koji.

`VACUUM INTO` reads one consistent snapshot under ordinary read locking,
writes a fully checkpointed file with no sidecars, and rebuilds it
compactly. The script refuses to overwrite an existing destination, checks
there is room before spending minutes finding out there is not, and then
verifies the copy with `PRAGMA integrity_check` and reports what it holds —
a copy nobody checked is a hope rather than a backup. Measured on a live
store mid-sync: 581MB in, 545MB out, eight seconds.

### koji-lag: `export` writes CSV, and partial days are never analysed

`koji-lag export --store lag.sqlite --since ... -o DIR` writes the store's
own tables as CSV — `builds.csv`, `tasks.csv`, `hosts.csv`,
`channels.csv` — which is what the Fedora Data WG asked for. A day of
Fedora takes under a second. Hosts and channels are easy to overlook and go
out regardless: without them a `host_id` is a number, and the arch a
`noarch` build actually ran on cannot be recovered at all.

**Whole days only, everywhere.** A day that was listed but has not had its
child tasks fetched holds builds with no arch tasks, and statistics over
those do not read as incomplete — they read as a quiet day. Reporting
2026-04-26 to 30 mid-sync used to say 36,812 of 36,816 builds had no arch
tasks swept, and warn about nothing.

So no path analyses a partial day now. `report`, `reports` and `export` all
take their rows from one place in the store, which yields only whole days
and names the ones it left out; a range with nothing complete in it is
refused, with the days to sync in the message, rather than answered with an
empty file or a report of a tenth of a week. The same range as above now
reports its three complete days, says which four it excluded, and shows 4
builds without arch tasks rather than 36,812 — builds that genuinely failed
before starting one.

`reports` keeps the stricter form of the same rule: a day, week or month is
written only when the store holds all of it, so a weekly report never
appears from a partly known week.

CSV rather than JSON, and there is no JSON export. A store already travels
as itself — one SQLite file — so re-encoding it to import it again bought
nothing, and it carried a hazard: a dataset's coverage windows are a promise
a later import acts on, so exporting half-swept days could tell a future
sweep to skip creations nobody had listed.

Fields are quoted when they would otherwise break a row, which Koji's data
does not currently need and a writer should not assume: a CSV that parses
wrongly is worse than one that fails.

Two bugs stood behind that silence. A report over a store recorded the
*requested* period as its coverage, so a half-synced month claimed to be
complete; and coverage was only ever checked *between* windows, so a period
uncovered at its edges, or not covered at all, produced no warning
whatsoever. Coverage is now what the store holds whole, compared against the
period asked for, and the report names dates rather than raw unix seconds.

Library API: `Store::dataset_for` takes the creation-grace margin, since
that is what decides which days count as whole; `Store::whole_days` and
`Store::analysable` are the primitives behind all three commands; and
`ReportOpts` gains `period`, the
period a report is about as distinct from the row filter — a store query has
already selected the period, and filtering again would split a build that
finished minutes before midnight.


### koji-lag: the store's schema is versioned, migrated and committed

Adding a column used to have no path: the store recorded a schema version
and refused anything that did not match it exactly, so a new field would
have meant every existing store being rebuilt from the hub — days of
queries for a column.

The schema is now a list of migration steps, one per version, applied in
order to whatever a store is missing and each in its own transaction. A
store from an older version is migrated up rather than refused (tested,
rows intact); one from a newer version is still refused, since rows written
by a newer binary may mean something an older one would misread.

`data/store-schema.sql` is generated from a fresh store and checked by a
test, the same way the man pages and the dataset's JSON schema are. A
schema change therefore shows up as a diff of the schema, and the file
doubles as documentation for anyone querying a store directly.

What this does *not* do is fill a new column in for rows already stored;
that is what the two generation constants are for, and DEVELOPMENT.md now
sets out which bump costs an hour (a field from the build listing, re-list)
and which costs days (a field from the child queries, re-fetch).

### koji-lag: the slow paths are gone (breaking CLI and API)

`fetch`, `backfill` and `merge` are removed, along with everything that
existed to make paging by offset bearable. What they did, `sync` does
without the parts that hurt: no walking from today to reach a window in
the past, no re-listing three days for every day of a backfill, no
collating raw data between grains to keep it from filling the disk, and no
sweep that costs the same when the data is already in hand.

Migration: `koji-lag sync --store lag.sqlite --since X --until Y` in place
of `fetch` and `backfill`, and `koji-lag import` in place of `merge` —
datasets union into a store the same way they unioned into each other.
Existing JSON keeps its value: import it once and it is queryable by
period without being re-fetched.

`report` and `reports` read the store. `report --store FILE` takes the
period from `--since`/`--until` (JSON files still work, given instead of
`--store`), and `reports --store FILE --reports-root DIR` replaces
`--root DIR`: it writes a report for every day, week and month the store
holds whole, rather than one per dataset file it finds. A period counts as
whole only when its creation span is listed *and* every build in it has
its children — so a weekly report appears when its last day lands, and
never from a week the store only partly knows.

Breaking API for anyone using the library: `fetch::run`, `run_with_builds`,
`walk_builds`, `walk_builds_below`, `WalkProgress` and `FetchReport` are
gone, `FetchOpts` loses `owner` and `packages`, and the `backfill` module
is gone with the subcommand — its calendar helpers (`Grain`, `Chunk`,
`week_of`, `month_of`, `weeks_of_month`) live in the new `periods` module,
and `pool` holds the coverage-driven report writing.

Sweep-time filtering goes with them: everything the hub reports is stored,
and narrowing is `report --owner`/`--package` over the store. A store
mixing filtered and unfiltered coverage would under-report silently, with
nothing in a row to say which sweep put it there.

### koji-lag: `sync` fetches only what the store is missing

Sweeping a window used to cost the same whether or not the data was
already on disk, and windows more than about six weeks old could not be
swept at all: the walk positioned itself by paging from the newest task
and gave up past half a million rows. `koji-lag sync --store` replaces it
with a walk that positions itself by creation time — the hub answers such
a page in the same 7–10 seconds wherever in history it points — and that
asks only for spans the store has never listed.

```sh
koji-lag sync --store lag.sqlite --days 7 -v
koji-lag sync --store lag.sqlite --since 2026-04-01 --until 2026-04-30
```

A day of April, into a store that held nothing older than June, took 18
minutes 52 seconds: 31 listing pages for the four days of creations a
one-day window needs, then 8,147 builds' children. Syncing the day before
it listed one day rather than four — the other three were already
covered, by the margin the first run walked through and kept.

Re-running a covered window costs two calls, for the hub's current hosts
and channels; a window inside a covered stretch costs the same even if it
was never asked for by name. An interrupted run keeps everything it
fetched: the listing records each page's span as it lands, and the
children stage marks each batch of parents as it is stored, so a sync
resumed after a crash asks only for the remainder. The two stages are
tracked apart, so children can be fetched for a window an earlier run
listed and never finished.

A sync takes no `--owner` or `--package`: everything the hub reports is
stored, and narrowing is a query over the store rather than a decision
made while fetching. See the tool's DEVELOPMENT.md for why coverage is
recorded as what was *listed* rather than what was kept.

Two details of the cursor worth recording. It steps five seconds past the
oldest creation a page returned rather than one, because pages come back
in task-id order and Koji assigns ids and creation times together but not
atomically — a row committed a moment late would otherwise be stepped
over. And a page whose rows span less than that margin is drained by
offset instead of by cursor: a mass rebuild submits hundreds of builds at
once, and moving the cursor five seconds back would return the same rows
for ever.

### koji-lag: a day's child tasks in a minute rather than eight

Child tasks are fetched 200 builds to a query instead of 40. Almost all of
a query's cost is the round trip — measured against Fedora's hub, 40
parents cost 28ms each, 100 cost 5.5ms and 200 cost 3.8ms — and since a
build has about four children, the flat part dominates until a batch is in
the hundreds. This is the expensive half of any sweep, so a day of Fedora
went from around eight minutes of it to about one.

The row limit for those queries is now its own number rather than
`--page-size`, which sizes the build listing: a batch answers with several
times as many rows as it has parents, and tying the two together meant a
larger listing page silently raised the threshold at which a batch is
judged to have overflowed. Batches that do overflow are still split and
refetched.

The measurements behind this, and the rest of what the hub costs, are now
recorded in the tool's DEVELOPMENT.md.

### koji-lag: a SQLite store, and `import` to fill it from existing datasets

Backfilling a month cost far more than the month: a sweep for May reached
only as far back as June 8th before giving up on positioning, then spent
minutes re-listing June, which a previous run had already fetched — and
did it again for every week. Nothing remembered what had been asked for
across runs, because a run's memory was the JSON file it wrote and those
files were per-window.

The store is that memory. `builds`, `tasks`, `hosts` and `channels` in one
SQLite database, plus two records that let a sweep skip work honestly: the
creation spans it has *listed* in full, and per build whether its children
have been asked for. A record of what was *stored* cannot serve, which is
the lesson of the bound removed in 0.20.0 — builds are kept by completion,
so the oldest in a window sits inside the previous window's margin, and
sweeping 2026-08-01 against such a bound lost 32 builds created in its
last few minutes.

Everything the hub reports is kept, whether or not the window being swept
wants it, because a round trip costs minutes and a row costs bytes. A day
of Fedora is about 33,000 rows and six minutes of hub time.

`koji-lag import <path> --store <file>` reads the JSON datasets sweeps
wrote before this, so nothing already collected has to be fetched again.
Measured on 676MB of existing data: 517,233 builds and 1,801,758 tasks in
22 seconds, into a 402MB store — 40% smaller than the JSON and queryable.
An import claims only the *inner* window of each dataset as listed, never
its three-day margin: a build created in the margin and finishing after
the window is absent, so claiming it would tell a later sweep those
creations were enumerated when they were not. A scoped sweep's dataset
contributes rows but no coverage at all.

Both coverage records carry the generation of the code that wrote them.
Adding a field taken from the build listing then needs only a re-list, an
hour for a year's rows; a field from the child queries needs those queries
again, which is days. Keeping the two apart is what makes a new field a
refresh rather than a rescan, and it is why a dataset lacking a collected
method — an older sweep's, or one from a database dump — records its
builds as never having had children asked for, rather than as current.

The database is not for version control: it is rewritten whole on every
commit and diffs to noise. `*.sqlite` is ignored, and what gets published
stays the reports.

## v0.20.0

### dbranch: an upload could send a package built from an older tree (breaking CLI and API)

Uploading is its own stage, so `dbranch rebuild --stage upload` sends
whatever `.changes` is sitting next to the repository — including one
built before the last few commits. The archive then takes the old
contents under the new version's name, and since a version can only be
uploaded once, fixing that means another changelog entry.

Breaking in the library too, for anyone using dbranch's crate rather than
its command: `Stages` gains a `source` field, and `plan::debuild_argv`
takes an argument where it took none.

Making the source package is now its own stage too, `source`, split out
of `build` the way `fedpkg srpm` is separate from `fedpkg mockbuild`.
`source` runs `debuild -S`; `build` is now only the `pbuilder-dist`
scratch build of the resulting `.dsc`. `all` runs both, so the everyday
path is unchanged, but **`--stage build` on its own no longer produces
the source package first** — pass `--stage source,build` (or `all`) if
that is what you meant.

With a stage that produces it, `build` and `upload` can check what they
are about to consume when they run without it. A source package built
before the current commit is a refresh: dbranch offers to rerun
`source` (default yes) and carries on with what is on disk if you
decline, or if nothing is on a terminal to ask. A *missing* one is a
sequencing mistake rather than a stale artifact, so it asks separately,
defaults to no, and a non-interactive run fails outright naming the
stage to run — an automated workflow that reaches `upload` without a
source package should have its stages fixed, not silently patched up.
`--yes` accepts either offer, and `--dry-run` skips the check since it
built nothing.

Uncertainty never triggers a rebuild: an unreadable timestamp or a
repository whose HEAD won't resolve read as fresh. The comparison uses
the committer date, so an amend or a rebase doesn't make a genuinely
current build look stale.

### dbranch: uploads to a PPA no longer risk being rejected for a missing tarball

The source package was always built `debuild -S -sa`, forcing the
upstream tarball into every upload. That is required for a PPA — the
rebuild versions dbranch generates reuse the upstream version, so
dpkg's own rule would leave the tarball out and Launchpad would reject
the upload — but it is redundant for the Debian archive, which already
has the tarball for anything past the first revision, and a tarball
that doesn't match the archive's byte for byte is itself grounds for
rejection.

The source stage now picks per destination, on whether the destination
can fall back on the Debian archive's pool: dak resolves a file the
`.dsc` names but the `.changes` doesn't offer by searching that pool,
so `update` to unstable, proposed-updates and backports — one archive
between them — are built `-si` and let dpkg decide from the changelog.
A PPA, a dput host such as `mentors` and a Debusine personal repository
have no such fallback and are built `-sa`. The flag appears in the
narrated command either way, so a run says which it chose.

An upload that runs without the source stage checks the `.changes` for
the tarball when the destination needs one, and offers to rebuild the
source package if it isn't there.

### koji-lag: backfill a long window a day at a time

`koji-lag backfill --since D --until D --root DIR` sweeps a range one UTC
day at a time, writing `daily/YYYY/MM/DD/<instance>.json` and collating
finished periods: a complete week becomes `weekly/YYYY/MM/DD/` dated from
its first day and its dailies are removed, a complete month becomes
`monthly/YYYY/MM/` and its weeklies are removed.

A day at a time rather than one wide sweep, because the per-parent
children queries dominate and scale with builds, not windows — so days
cost the same in total, while an interruption loses the day in flight
instead of five hours. Re-running resumes
from what is on disk, and `--if-exists merge|replace|ask` says what to do
about a day already there; `ask` is the default and merges when there is
nobody to ask, since merging cannot lose data.

Weeks run Monday to Sunday but never cross a month boundary, so a month's
figures are exactly the sum of its weeks with no week counted against two
months. A clipped week keeps its first day as its name: August 2026 opens
on a Saturday, so `weekly/2026/08/01` covers the 1st and 2nd. Collation
writes the merged file before deleting the parts, so an interruption
leaves both rather than neither, and it prunes the dated directories it
empties — an empty `daily/2026/08/03/` reads as coverage that is not
there.

`--reports-root` writes a report for each period as it completes. Reports
are deliberately *not* collated away: they are kilobytes against the
datasets' megabytes, and a daily report answers questions a monthly one
has already averaged out.

`koji-lag reports --root DIR --reports-root DIR` renders reports for a
tree already swept, since changing what a report says should not mean
asking Koji for the data again. It leaves existing reports alone unless
`--force` is given.

`report --out DIR` writes `report.txt` and `report.json` together in one
pass. Getting both previously meant running the report twice, reading the
dataset twice, and a report that exists in one form but not the other is
the kind of difference nobody notices until a script needs the missing
one.

### koji-lag: a backfill no longer walks from today to reach last month

Fetching a past window paged through everything newer first — for a June
window in August, three hundred requests before reaching any wanted data,
and a day-at-a-time backfill paid that over again for every day. Task ids
only grow, so a handful of one-row probes can find where the window begins
instead: galloping outwards from one page, then bisecting until the bracket
is within a page or two, since landing early costs nothing (the walk
filters client-side regardless). A one-day fetch spends four probes; a
window six weeks back, fourteen.

A count query would have been the obvious way to size the job and is not
affordable — `listTasks` with `countOnly` over a three-day creation window
measured 83 seconds — and offsets themselves stop being cheap past a few
hundred thousand rows (2.8s at 300,000, 81s at a million), so the search is
bounded and falls back to plain paging beyond that.

Requests are now paced by how long the hub takes. `--duty-cycle` (default
50) names the share of one connection to aim for, and each pause is scaled
to the last request's latency, so a loaded hub is asked less often and one
that recovers is asked more, down to the `--sleep-ms` floor. The fixed
pause it replaces paced backwards under load: 500ms between half-second
queries used half a connection, but the same 500ms between eight-second
queries used 94% of one — leaning hardest exactly when the hub could bear
it least.

Also corrected: the source-rebuild stage has two names, `rebuildSRPM` for a
scratch build submitted as an SRPM and `buildSRPMFromSCM` for a build from
dist-git. Only the first was collected, which looked like near-complete
coverage while missing every dist-git build. And the parent `build` task's
own host is now recorded: it coordinates rather than compiles, but it holds
a slot on a real machine while it waits — one was seen occupying an s390x
builder for five minutes to produce a noarch package whose work all ran on
x86.



### koji-lag: the SRPM rebuild, and each shape of build, reported separately (breaking API)

Every Koji build starts by rebuilding the source package on a host the hub
picks — independently of what the build targets, so a package can queue
behind a machine it does not build for. Those `rebuildSRPM` tasks were
never collected, so that part of a build's wall clock was invisible. They
are now swept alongside the per-arch builds (one query per batch either
way, since the method filter was what excluded them), and `TaskRecord`
records which method a task is.

Attribution deliberately ignores them. The source rebuild runs *before*
the per-arch builds rather than racing them, so counting it as an arch
would make every real arch look like the bottleneck by a margin including
work nothing waited on in parallel.

Per-arch wait and run times are now reported for each shape of build,
because the questions differ:

- `Multi-arch builds` — two or more arches raced, so one finished last and
  a delay can be attributed to it. These are the only builds attribution
  applies to.
- `Single-arch builds` — nothing to be slower than, so wait and run time
  stand alone. This is where July 3rd's single-arch s390x p90 wait of
  three hours became visible.
- `noarch builds` — a portable payload built once on a machine that is not
  portable at all, so these are keyed by the arch of the *host* that took
  them.
- `SRPM rebuild` — likewise keyed by host arch.

Each section carries median and p90 for both the queue wait and the run,
so a slow queue stays distinguishable from a slow machine.

Host arches come from `listHosts` and are stored per instance, because the
hub's host names do not carry them reliably — Fedora has builders called
`xenbuilder3` and `ppc1`. Datasets swept before this still report: their
`noarch` rows read `noarch (host unknown)` rather than guessing an arch or
dropping the tasks.

New in `sandogasa-kojihub`: `list_hosts_with_arches`. The dataset schema
gains `TaskRecord::method` (defaulting to `buildArch`, so older files load
unchanged) and a `host_arches` map; the report JSON gains `srpm`,
`multi_arch`, `single_arch` and `noarch_by_host`.

Breaking for anyone constructing `koji-lag`'s library types in Rust, which
is part of why this release takes a minor bump: `Dataset` gains
`host_arches`, `TaskRecord` gains `method`, and `FetchOpts` gains
`duty_percent`, so struct literals over them need the new fields. Datasets
written by earlier versions still load — every added field has a serde
default — and the command line is unchanged.

### koji-lag: progress through the children sweep, and what a bottleneck count is out of

The per-parent sweep of `buildArch` children reported `batch 47 (40
parent(s), 812 task(s))`, with no sense of the total — though the total
is known, since the walk counted the parents. It now reports parents
finished against parents found. Batches would not do: a batch whose
answer fills a page is split in half and retried, so the batch count can
rise while nothing finishes, and the line now says `splitting` in that
case rather than implying progress.

A `report`'s header said `Bottlenecked builds: 5082` with nothing to read
it against — five thousand out of what? It now gives the day's builds and
accounts for the difference, with the figures summing to the total:

```
Builds completed: 7377; with an arch on the critical path: 5082 (69%);
single-arch, failed or untimed: 2252; no per-arch tasks swept: 43;
unattributed tasks: 0
```

The 2252 are not a data problem: attribution needs two arches to compare,
and a failed or untimed task offers no completion. A `Header legend`
section defines each figure, and `builds_in_window` and
`builds_with_tasks` join the JSON.

Counting those builds turned up a fault in the obvious implementation:
the parents named by tasks are not the same set as the builds in the
window, because a task whose parent was never swept names a build that
is not there. Those are already reported as unattributed tasks, and are
no longer counted as builds too.

The README now explains every figure in both progress lines, and why one
wide window costs far less than many narrow ones: the walk starts at now
whatever the window, so fetching a month a day at a time walks the same
history thirty times.

### koji-lag: a fetch says how far back it has got

`fetch` walks every `build` task newest-first — `listTasks` has no
completion filter that survives a loaded hub, so the window is applied
client-side afterwards — and reported its position as a bare page number.
A busy month runs to hundreds of pages, and "page 219 (1000 task(s))"
says nothing about whether that is nearly done.

The line now reports what the walk is doing, which is marching backwards
in time: tasks seen, the creation time reached, how much of the window
that covers, and roughly how many pages remain.

```
[koji-lag] build walk: page 3 (1000 task(s), 3000 so far),
    back to 2026-08-13 00:53 — 11% of the window, ~24 page(s) to go
```

Remaining pages are estimated from the density observed so far rather
than from a count query. A filtered count is not affordable: `listTasks`
with `countOnly` over a three-day creation window measured 83 seconds
against Fedora's hub, the same index problem that rules out server-side
completion filtering, while an unfiltered count answers in 3 seconds and
says only that the hub holds 24,047,128 build tasks in all. So the
estimate costs nothing, is marked `~`, and refines every page. It assumes
an even spread, which weekends and mass rebuilds break — a jump is the
walk learning, not a fault.

The reached time includes the hour, because inside a short window the
date never changes, and a line that never changes reads as a walk that is
stuck.

### ebranch: check-wip's summary hid work left on the target branches

Rawhide is the shared spine — packages land there first and every other
branch is cut from it — so the summary reported Rawhide's state and left
the targets to the detail lines. That is right while Rawhide has work
outstanding, and wrong once it does not. A ledger read `built for
rawhide, update pushed` while four branches were waiting for updates to
be submitted, which is the only thing anyone could have acted on.

Rawhide now keeps the summary only when something is to be *done* there —
nothing built, or a build sitting in a side tag with no update. When it
is merely waiting, with an update in flight or a compose pending, the
targets speak instead, and Rawhide speaks again once every target is
finished. Note that "waiting" is not the same as "current": an update can
be pushed to stable while the repositories still show the older version,
and there is nothing left to do about it.

Reporting the targets promptly exposed a claim that had been hiding
behind the spine: `needs a branch for epel10.3`, when dist-git has an
`epel10` branch and EPEL 10's minor releases all build from it. Bodhi
records which branch each release uses, so that is now kept on the ledger
(`"epel10.3" = "epel10"`) and the branch check consults it rather than
assuming a target and its branch share a name. A version already seen in
a target's repositories also counts as proof of branching, since a
package cannot be built for a release it has no branch in — which covers
targets Bodhi has no release for.

### ebranch: check-wip says what a build is waiting behind

A branch can have a new build ready and still have nothing to do, because
the previous version is serving out its time in testing. Bodhi requires
seven days for a branched Fedora release, and editing the pending update
to carry the new build restarts that clock and discards the karma it has
collected — so the wait is real and worth seeing.

`check-wip` was hiding it. An update was recorded only when it carried
the newest build; an update for an older version was discarded as
irrelevant. It is not irrelevant, it is the thing in the way. On a live
ledger, three branches showed `built 0.19.4-1.fc44 in
f44-build-side-146827` and nothing else, which reads as ready to submit,
while an update carrying 0.19.1 sat in testing on each of them.

An in-flight update is now kept whatever version it carries, and the
version is recorded with it so the two can be told apart. The branch line
shows both, with how long the older one has served:

```
f44: 0.18.1-1.fc44, built 0.19.4-1.fc44 in f44-build-side-146827;
     0.19.1-1.fc44 in FEDORA-2026-2134e68e6e testing (7 of 7 days)
```

The state says it too — `waiting on an earlier update` — and ranks below
`needs an update`, because there is nothing to do but wait. A finished
update for an older version is still dropped: that one really has nothing
to say.

The days come from the date the update entered testing, which the ledger
stores, with the elapsed time worked out when the report is written. A
stored count would go stale; a date cannot. The requirement comes from
Bodhi per update rather than being assumed to be seven, since a release
sets its own. Where Bodhi reports no requirement the count is still
shown.


### sandogasa-koji: a repo regeneration is not a hung hub

The 30-second bound added in 0.19.3 was applied to every koji call,
including the ones whose whole purpose is to block. `koji regen-repo
--wait` runs for minutes on a large tag — one measured against Fedora's
hub took 4m43s — so `ebranch check-update` offered to regenerate a stale
side tag repo, started, and then aborted itself half a minute later with
"the hub may be down". The hub was fine. The code even said so: the
comment above the call read "repo regeneration can take several minutes
on large tags".

Waiting operations now have their own bound, 30 minutes, overridable
with `SANDOGASA_KOJI_WAIT_TIMEOUT`. Queries keep the 30-second one,
because a hub that has not answered a question in that time is not going
to.

A cap alone would trade a wrong abort for a long hang when the hub
really is down, so before starting a wait the hub is asked `koji
version` — a sub-second round trip needing no authentication. An
unreachable hub costs the query timeout, not the wait bound. And a wait
that outruns its bound no longer marks the hub unresponsive for the rest
of the run: slow work is not a dead hub.

New in `sandogasa-koji`: `hub_responds`, `WAIT_TIMEOUT_ENV`.

## v0.19.4

### ebranch: check-wip missed builds that were already in Koji

`check-wip` follows a set of packages on their way into Fedora and keeps
what it learns in a TOML file, so each run only has to look up what may
have changed. It stopped noticing new builds. On a real run,
`rust-emojis 0.9.0` had been built into five Koji side tags and was
reported from one of them; `sandogasa 0.19.3` sat in two side tags
without a mention.

This affects every `check-wip` released so far — 0.19.2, where the Koji
lookup arrived, and 0.19.3. No cleanup is needed: the stale records those
versions wrote are corrected on the next run, because the branches they
had written off are asked about again.

The cause was a shortcut that fed on itself. To avoid asking Koji about
work that is finished, the tool skipped any branch whose repositories
already carry the version being pushed. But with no COPR behind an
effort, it had no independent idea of which version that was, and used
the newest build it had *itself recorded earlier*. So as soon as that
record matched what the repositories ship, the branch looked finished, it
was never asked about again, and the record could never change. Deleting
one such record made the very same run find the build it had been blind
to.

Only a fact from outside the file can close a branch now: the version
staged in a COPR, which says what is being pushed regardless of what has
been built. An effort with no COPR — packages built straight into a side
tag — has every branch asked about on every run.

A side tag's contents also apply to packages already being tracked, not
only to ones being discovered for the first time. The tag is the
authority on what exists in it, and using it costs nothing, because that
query already happened once for the whole tag. An older build in a side
tag does not overwrite a newer record.

Asking that much needed a cheaper question. Each branch used to be one
`koji list-tagged` per package per tag, so fifteen packages meant thirty
subprocesses per branch. A branch's tags can also be listed whole, once,
and read for every package at once. Measured against the Fedora hub, a
release's candidate tag answers in about 8 seconds with some 24,000
builds, while the same query narrowed to one package takes about 1.25
seconds — so listing the whole tag pays from roughly seven packages
upward. Both forms are kept and the package count decides. New in
`sandogasa-koji`: `latest_tagged_all`.

Two states also described the situation wrongly, and now share one
phrasing:

- a build in a side tag is in no release tag at all, so no compose will
  ever pick it up. This read `built for rawhide, not yet in the repos`,
  as though waiting were enough; it now reads `newer build in a side tag,
  not landed in rawhide`
- a version staged in a COPR with no Koji build of it read `in dist-git,
  no rawhide build found`, which was odd when a build *was* found, just
  of an older version. It now reads `newer build in a COPR, not landed in
  rawhide`

`built for rawhide, not yet in the repos` survives for a build in the
release's own tags, where a compose really is the only wait. The branch
line names the side tag when no update carries the build yet —
`rawhide: 0.19.1-1.fc45, built 0.19.3-1.fc46 in f46-build-side-146944` —
since that tag is what `bodhi updates new --from-tag` needs. A release's
own tags stay unnamed, because the state already says what is awaited.

The README's example output was two releases stale, showing a heading the
tool no longer produces.

### Development: dev builds stop writing tens of gigabytes of debuginfo

`target/` had reached 80GB in `debug` alone and run the machine out of
disk more than once. Two changes, both measured rather than assumed.

`[profile.dev] debug = "line-tables-only"` halves what a dev build
writes: a cold `cargo build --workspace --tests` went from 6.3GB to
3.3GB. Backtraces keep their file and line numbers — verified in the
built binary, whose `.debug_line` still decodes to `wip.rs:1578` — and
what is given up is variable inspection under a debugger, which is not
how anything here gets diagnosed. Release builds, which is what distro
packaging drives, are untouched.

`make sweep` (`scripts/sweep.sh`) deletes the trees the release gates
leave behind: `cargo semver-checks` builds rustdoc for the current *and*
the published version of every library crate, and `cargo cov` builds a
separately instrumented copy of the workspace — 19GB and 6.5GB
respectively at 0.19.3 — plus `target/package` and stray `*.profraw`.
They are caches keyed on versions that just changed, so a release is
exactly when they are provably disposable, and the release checklist now
ends there. `target/debug` is deliberately left alone: it is what makes
day-to-day builds fast, and `cargo clean` already says what it removed.

## v0.19.3

### sandogasa-koji: a hub that is not answering no longer hangs the caller

Found during F45's mass branching, with the hub down: `ebranch
check-wip` printed one line and then blocked indefinitely — the koji CLI
waits forever, and nothing here bounded it. Ten minutes in there was
still no output and no way to tell what it was waiting on.

Every koji call is now bounded, 30 seconds by default, overridable with
`SANDOGASA_KOJI_TIMEOUT` (`0` waits forever, the old behaviour). The
child is killed rather than abandoned, and both its streams are drained
on their own threads: a pipe holds 64 KiB and `list-tagged` on a release
tag runs to megabytes, so reading after the wait would make a child
blocked on a full pipe indistinguishable from a hung hub.

One timeout is enough for a run. A hub that did not answer will not
answer the next query either, and these callers ask once per tag per
package — so the first timeout latches per profile, later calls fail
immediately, and `hub_unresponsive` lets a caller with queries left stop
and report from what it already has. check-wip now finishes in under a
minute against a down hub, warning once and serving the report from the
ledger, instead of hanging.

New public surface: `hub_unresponsive`, `TIMEOUT_ENV`.

### ebranch: --forget, the counterpart to --add

Packages could enter a ledger without anyone typing their name — from a
COPR, and now from a side tag — but nothing took one out. `--prune
packages` will not, correctly: it acts only on packages a COPR once
staged, so a discovered one is beyond its remit, leaving hand-editing
the TOML as the only way.

`--forget NAME,...` drops them, and records the refusal, because
deletion alone would be futile: the side tag or COPR that produced the
package is still registered, so the next run would take it straight back
up. The ledger keeps an `ignored` list that both discovery paths honour,
which makes forgetting a standing decision rather than a one-time
delete. `--add` is its inverse and clears the refusal, so a ledger never
both tracks and ignores the same package. Forgetting something already
forgotten is a no-op; forgetting a name that was never tracked is an
error, so a typo does not read as success.

### ebranch: check-wip reports the age of what it says

A branch line dated itself by the oldest fact *known* about the branch,
including facts it was not showing. A build matching what is shipped is
deliberately not printed, and once a branch is current nothing re-asks
Koji about it, so that hidden record keeps an older date — and dragged
the line back to it. sandogasa's Rawhide line read "as of 2026-08-11"
with every fact on it seen that morning. The date now comes from the
parts actually rendered, which is what the line always claimed to mean.

A target a side tag implies is taken up after Bodhi has said which
branches are distinct releases, rather than before. Rawhide's side tags
are named for whatever version Rawhide currently is, so registering
`f46-build-side-146944` added `f46` as a target and dropped it again as
a duplicate of Rawhide — on every run, announcing both halves each time.

### ebranch: a side tag seeds the ledger, and prune stops deleting what it never tracked

A registered side tag is a statement about what an effort covers, the
same as a COPR, and its contents were already being fetched — one query
per tag, answering for every package in it. Only the packages the ledger
already knew were read out of that answer; the rest was discarded. Now a
side tag's builds seed the ledger, which makes the no-COPR route (build
a stack into a side tag) as self-populating as the COPR route instead of
needing `--add` per package. Verified against a live tag: from an empty
ledger, `--side-tag f46-build-side-146944` found `rust-emojis` and
reported `rawhide: 0.8.2-2.fc45, built 0.9.0-1.fc46` — dist-git and repo
state filled in during the same run, since a package discovered
mid-run is caught up rather than left as a lone build line.

The version lands where it belongs: a COPR stages a build outside the
distro, while a side tag holds one Koji has already accepted, so this is
recorded as built for the branch rather than staged. Additions are
reported per tag, because a side tag shared with a wider effort can hold
far more than the ledger is about and that should be visible.

This required fixing `--prune packages`, which deleted packages it had
no evidence about. It dropped anything with no staged version, which
conflates "a COPR had this and no longer does" with "no COPR ever had
it" — so a package added by `--add` was silently deleted by the next
prune, and a side-tag-discovered one would have been too. A package is
now pruned only if a COPR the ledger follows has actually staged it at
some point. Existing ledgers err toward keeping: an entry that predates
the flag is not pruned until it appears in a COPR again.

### ebranch: --prune takes what to forget, and never acts on silence

A ledger holds two lists on different timescales — the packages an
effort tracks outlast many rollouts, while a side tag dies with the one
it carried, deleted once its update goes stable — and `--prune` was a
boolean that only ever meant packages. It now names the list:
`--prune packages`, `--prune side-tags`, or both. A bare `--prune` still
means packages, so nothing that worked stops working.

Neither list is pruned on a question that went unanswered. A package is
forgotten only when every COPR the ledger follows answered and the
package was in none of them; a side tag only when Koji says the tag does
not exist. A timeout, an unreachable hub, a failed COPR or an `--offline`
run leaves both lists alone and says which check did not establish
anything. Today's Koji outage is the case that matters: pruning on
"could not ask" would have erased every side tag in the ledger, the same
shape of bug as reporting a build as absent because the query failed.

`sandogasa-koji` gained `tag_missing`, which separates the hub saying a
tag does not exist from every other failure. The knowledge that this is
a message match lives there rather than at each call site, because the
koji CLI exits non-zero for every kind of failure alike.

### ebranch: a ledger follows the distro through mass branching

Targets implied by side tags are recomputed every run instead of being
recorded once. A Rawhide side tag is named for whatever version Rawhide
currently is, so `f45-build-side-*` implied a target that was then
dropped as a duplicate of `rawhide` — correct until mass branching made
F45 a release of its own, at which point the target had to come back or
the branch's facts would be forgotten precisely when they started to
matter. Watched happen live: `f45` returned as a target by itself and
took up its own line, `0.19.1-1.fc45`, next to Rawhide's.

Also observed mid-branching, and reported correctly rather than
confidently: for a while no Bodhi release has branch `rawhide` at all —
F45's has already become `f45` and F46 does not exist yet — which reads
as "no Bodhi release matches rawhide by branch or name" rather than a
claim that Rawhide is gone.

## v0.19.2

### Dependencies

`emojis` 0.8 to 0.9. Fedora carries one dependent — this workspace — so
`rust-emojis` needs bumping to 0.9 alongside this release for the
package build to resolve.

### hs-relmon: wprof is packaged now

Its manifest entry tracked upstream releases because nothing packaged it.
It is in Rawhide, so it tracks `fedora-rawhide` across `fedora,hs9`
like every other packaged entry.

### ebranch: check-wip — a branch, a target and a release are three different names

Targeting anything but Rawhide meant naming branches, and Fedora's
names for a release do not agree with each other. Bodhi's F45 has
branch `rawhide`; EPEL-10.3 has branch `epel10`, while its archived
minors have `epel10.0` and `epel10.2`; dist-git has an `epel10` branch
but never an `epel10.3`; and fedrq answers for `epel10.3`. Matching one
of those against another produced four wrong answers, all of them
confident.

Releases are now matched by branch **or** by name, so a target named
after the release (`f45`, `epel10.3`) finds it. Previously the lookup
warned "no Bodhi release for f45" and skipped every Koji query for that
branch — the release exists, and saying otherwise sent the reader
looking for the wrong problem. The warning now says what was searched:
no release matches *by branch or name*.

Two targets that resolve to one Bodhi release are one release under two
names, and every fact about it was reported twice. Fedora names
Rawhide's side tags after its version, so `--side-tag f45-build-side-146825`
recorded `f45` as a target alongside `rawhide`. The duplicate is
dropped, Rawhide winning because it is always examined, and kept as an
alias so the side tag's builds are attributed to the branch that *is*
examined rather than going unnoticed. The alias is derived from the side
tags themselves, not from whichever run happened to record the target.

Whether a target already carries the version is now checked before
whether dist-git has a branch of that name. EPEL 10's minor releases all
ship from the `epel10` branch, so a package shipped for `epel10.3` was
told it needed branching — false, and the one state where the answer was
already known. Without a repository fact there is nothing to conclude
from, so the branch check still has the last word there; `needs
branching` reads `needs a branch` now that it is about a name.

The dist-git line shows every branch. It listed only Rawhide and the
targets while reading as the repository's branch list, which invites
exactly the conclusion it caused: sandogasa showed `epel9, f43, f44,
rawhide` and looked unbranched for EPEL 10, which it is not.

Branches are listed newest release first, and per package the releases
already carrying the new version come first. Alphabetically `epel10.3`
sorts before `epel9` and both before every Fedora branch, which is
neither oldest- nor newest-first — just the order the strings fall in.
Now the primary key is what each release ships, newest version first, so
two rollouts at once separate into the releases that are done and the
ones still to do, and the secondary key is release recency: Rawhide,
then Fedora by version, then EPEL. A bare `epel10` branch builds for
whichever minor is current, so it leads `epel10.2`, and Rawhide leads
its `main` alias.

A side-tag name that can never be queried is refused rather than stored.
`--side-tag` gained CSV in this release, and a value written by a run
before that was kept verbatim, so
`f44-build-side-1,f43-build-side-2` parsed as branch `f44` and warned
about "No such tag" on every run afterwards. The tag id must now be
digits — it is a Koji task number — which makes the joined form
unparseable; anything unusable a previous run stored is dropped on
sight. Facts recorded for a branch no longer examined are forgotten for
the same reason: nothing refreshes them, and in the report they are
indistinguishable from current ones.

### ebranch: check-wip headings follow the target, and routes can be set

Two gaps that would have shown up the first time an effort targeted
more than Rawhide.

The state heading was computed against Rawhide alone, so per-target
facts were gathered and shown in the detail lines but never grouped on.
Rawhide is still the spine — everything lands there first and is
branched from it, so while Rawhide is behind that is what the heading
reports — but once it is current the heading becomes the least advanced
target's state, and names every target in it: `needs a branch for
epel9`, `needs an update for f44`, `update in testing for f44, f43,
epel9`. Naming one of several would read as though the others were
further along — sandogasa's own 0.19.1, sitting in testing for three
branches, was reported against only one of them. With every target done
it says so without naming any, since choosing between equals would be
arbitrary. Each branch now gets one line rather than three. Shipped, built and
in-an-update each named a version, which mostly repeated itself — what
Koji has and what the repositories have are usually identical — while
the update line gave an alias without saying which version it carried.
Now a version appears once, a build only when it is ahead of the
repositories, and the update qualifies the version it carries:
`f44: 0.12.0-6.fc44 in FEDORA-2026-62c8a3ebba stable`. The date is the
oldest of the facts shown, so the line never claims more freshness than
its stalest part.

An update is also only recorded when it actually carries the build in
question. Bodhi's newest update for a package is often an old one — the
uutils stack has a 120-package update from four months ago, two versions
behind. That did not affect the heading, which checks for a current
build before consulting any update, but it is consulted once the build
*is* current, where a stale record would claim an update was carrying
work that nothing carries.

The Koji lookup asked the wrong tag. `stable_tag` sees only stable
builds, so anything in the candidate or testing window read as unbuilt.
Both the candidate and testing tags are now queried — neither sees the
other, though both inherit the stable tag — and the newest answer wins.
New `--side-tag` (repeated or CSV, like the other multi-value flags)
records a side tag on the ledger, takes its branch as a target — using
one says that branch is in scope — and scans it too,
since a side-tag build is tagged only there until an update carries it,
which is most of the window in which the next move is to submit one. One
query per side tag covers every package in it.

A package with no COPR behind it is now queried at all. The build
lookup only asked about packages ahead of the repositories, which needs
a version of interest, which a hand-added package has neither staged nor
built — so nothing populated `built` and nothing ever would. Verified by
tracking sandogasa's own update with `--add` and no COPR, which finds
0.19.1-1.fc43 in testing against 0.18.1 shipped.

Where no build is found the report now says exactly that rather than
"needs building": a build may exist in a side tag the ledger was never
told about, so the absence is ours, not the world's.

Several Bodhi releases share a branch — F43, F43C and F43F all report
`f43` — so the release is now picked by its id prefix rather than by
whichever Bodhi happened to list first, which could have pulled the
container or flatpak tags.

Rawhide is not exempt from the update states. A Rawhide build can be
carried by an update too — automatically, or submitted from a side tag —
and Bodhi files those under the release name for whatever Rawhide
currently is, F45 rather than "rawhide". Querying by branch name found
nothing and made it look as though Rawhide had no updates at all.

Retirement is also now checked before branching. A retired package with
a target recorded would have read `needs branching`, describing a dead
package as work waiting to be done.

`--set pkg=review:BUG`, `pkg=pr:ID`, `pkg=direct` records which route a
package takes, writing the ledger without contacting anything. Until
now the route was documented but only settable by hand-editing TOML, so
the field was inert and the README promised more than the tool did.
Switching route clears the identifier the previous one carried, so a
stale bug number cannot outlive the route it belonged to. Routes stay a
decision: no lookup sets one, however suggestive.


### ebranch: check-wip on retired packages — why, when, and whether anyone is re-reviewing

A retired package kept its dist-git repository, so the report could
only say it was retired, leaving the reader to work out whether it
should come back and whether anyone was already on it.

Three things now answer that. The `dead.package` reason is shown, which
is usually decisive — `rust-uu_touch` reads "rust-coreutils internal
dependency; replaced by uutils-coreutils", meaning it should not come
back at all. It costs nothing extra: `is_retired` was already fetching
that file and discarding the body, so `retired_reason` returns the
content and its absence is the not-retired answer.

The retirement date is shown when it can be established: 2026-06-09 for
that package. Pagure has no commit-log endpoint that could be found,
but a branch's HEAD is reachable — `git/branches?with_commits=1` gives
the hash and `c/<hash>/info` its `commit_time`. For a retired package
HEAD is normally the retirement commit, since nothing follows a
retirement, and the check that it *is* is its subject matching
`dead.package`. Without that match no date is claimed: it would be the
last time anyone touched the repo, later than the retirement, and too
optimistic for judging how long a package has been dead.

Whether a review is open is reported rather than guessed at. The search
prefers the newest open Review Request, so finding only a closed one
means nothing is in progress: `retired, no open review` versus
`retired, review in progress`. Deliberately not phrased as "its
original review" — a package can be retired more than once, so the
newest closed review need not be the first.

New in `sandogasa-distgit`: `retired_reason`, `branch_heads` and
`commit_info`.


### ebranch: check-wip asks Koji and Bodhi, not just the repositories

"Not in the repos" had more than one cause and only one name. A build is
tagged in Koji the moment it succeeds and reaches repodata only at the
next compose — or, on a branched release, only once an update is pushed
— so a package could be reported as needing a build when the build was
already done and waiting.

Koji is now asked for the latest build tagged for each branch, and on a
branched release Bodhi for the update carrying it. The states separate
accordingly: `needs building for rawhide`, `built for rawhide, not yet
in the repos`, and `built, update pending` / `in testing` / `pushed`.

Each branch's Koji tag comes from Bodhi's release list rather than being
constructed, so no release number is hardcoded and Rawhide is followed
as it moves — `f45` today. Only packages whose staged version is ahead
of what the branch ships are queried, since for anything current the
question is settled and Koji costs a subprocess per package. Rawhide
skips the Bodhi step, having no updates.

An absent record never implies a build: `built_at_least` is false when
Koji was not asked, so a package with no Koji record reads as needing a
build rather than as one mysteriously waiting.

A `--koji-profile` flag was added for CentOS SIG work and then removed
before release. The Koji tag comes from Bodhi, which knows only Fedora
and EPEL, so the flag had no tag to act on for a CBS branch — a flag
that looks like it enables something it cannot is worse than its
absence. What CentOS support actually needs is recorded in `TODO.md`.


### Config defaults cannot authorize a write, and booleans can be overridden

Two gaps in the `[defaults]` mechanism, found by asking how to override
a single pinned boolean.

`--yes`, `--claim`, `--apply`, `--prune`, `--submit` and `--give-karma`
are now refused as config defaults, with a hard error naming the flag
and pointing at the command line. Each exists to skip a confirmation,
and a config file is exactly where such a setting gets forgotten —
after which every run writes to Bugzilla, Bodhi or dist-git without
asking. A `--no-yes` escape would not have helped, since it only serves
someone who remembers the setting exists.

For booleans that are reasonable to pin, the way to override one for a
single run is a `--no-<flag>` partner, and it must use
`conflicts_with`, not `overrides_with`. The mechanism already skips a
default that conflicts with an explicitly-given flag, which is exactly
the hook needed. `overrides_with` looks like the natural choice and
quietly fails: it is mutual and order-sensitive, and injected defaults
are appended *after* the command line, so the injected flag arrives
last, wins the override, and unsets the negative — leaving no trace
that it was passed.

`ebranch check-wip` gains `--no-offline` on that pattern, so a pinned
`offline = true` can be overridden without `--no-defaults` dropping
every other default too. Both rules are in `DEVELOPMENT.md`.


### ebranch: check-wip notices retirement, and stops re-asking settled questions

A retired package keeps its dist-git repository, so "the repo exists
but nothing is built for Rawhide" described a dead package as work
waiting to be done. Retirement is now read from `dead.package` and
reported ahead of anything about builds, since unretiring is what
blocks everything downstream. On the uutils effort that immediately
reclassified `rust-uu_touch`, whose repository exists only because
retirement leaves it behind.

Whether coming back needs a fresh review depends on how long the
package has been retired. That is Fedora policy, with tooling that has
changed recently, so it is deliberately not encoded: the tool reports
the retirement and leaves the judgement to the reader.

Observations that cannot change are no longer re-fetched. A closed
review of an imported package is settled, so it is skipped — a saving
of one Bugzilla query per package per run on a long-lived effort.
Retirement deliberately breaks that condition, because a returning
package may need a new review.

New `--rescan-reviews` searches for a review request again even where
one is recorded. Searching rather than fetching the known ID is the
point: after a retirement the recorded bug is the old review, and what
matters is whether a new one has been filed.

`--package NAME,...` narrows the per-package lookups and the report,
which is what makes a rescan cheap — you know which package was
retired, and the alternative is a Bugzilla query for every other one.
The COPR reconcile deliberately stays whole: it is a single query for
the effort, and narrowing it would leave a package that has left the
COPR still recorded as staged. A name the ledger does not track warns
rather than silently matching nothing.


### sandogasa-bugzilla: ask for the fields we deserialize

Bugzilla's default field set leaves out `flags`, and `Bug` declares
that field `#[serde(default)]`, so a read that did not request flags
deserialized to an empty list — indistinguishable from a bug that
genuinely has none. Every approved package review therefore looked
unapproved, since approval *is* the `fedora-review+` flag. Bug 2498026
carries it and `ebranch check-wip` still reported the review as
awaiting approval.

Reads now send `include_fields=_default,flags`. `_default` keeps
everything the default set has, so adding a field to that list cannot
take one away, and a wiremock test asserts the parameter is on the
wire so a later refactor cannot quietly drop it.

Anything else reading a bug's flags gains the fix: `sandogasa-report`
and `fedora-cve-triage` both deserialize `Bug`.

`DEVELOPMENT.md` now carries the rule this belongs to — "not there"
and "did not ask" are different answers — alongside the two other
instances of it found the same day.


### ebranch: track packages on their way into the distro

`check-wip <ledger>` reports where each package of a coordinated
packaging effort stands. A stack of new crates, or a version bump that
drags its dependencies along, moves every package through the same
sequence — staged somewhere, reviewed or submitted as a pull request,
built for Rawhide, branched, built again, shipped in an update — and
working out which package is where decides what to do next.

The effort lives in a ledger, the TOML file named on the command line,
rather than being read fresh from the COPR each run. A COPR shrinks as
packages graduate out of it, so a report rebuilt from one would lose
exactly the work that is finished, and the ledger also holds what no
service can report: which route a package takes, and which review bug
or pull request is landing it. Each run reconciles — new packages are
added, existing ones refreshed, and one that has left the COPR keeps
its entry and stops counting as staged, since it has finished rather
than vanished. `--prune` forgets those.

Every observation is dated, so `--offline` can serve the report from
the ledger without contacting anything and still say how old the
reading is. Refreshing writes the ledger back despite the command
reading like a query: observations are facts, they were expensive to
gather, and dropping them would only make the next run pay again.
Decisions are never inferred — a package that cannot be placed is
reported as unplaced rather than guessed at.

Beyond the COPR it asks dist-git whether each package has a repository
and which branches it has, and asks each targeted branch — Rawhide
always included — what version it already ships. That is what lets a
heading say `in dist-git, newer build staged` rather than merely that
the package exists: staged 0.13.0 against Rawhide's 0.12.0 is work
that has not landed, and both versions are printed so the comparison
can be checked. A package with no dist-git repository has not been
imported yet, which for a new package means its review is unfinished.

Absence and ignorance are kept apart throughout. `exists: false` from
dist-git is a real answer and is stored; a failed lookup stores
nothing and the report says "not checked" rather than implying the
package is missing.

A package dist-git has no repository for has its review request looked
up, since the review is what stands in the way. Only those are
searched — one already in dist-git is past that stage, and searching
every package would cost a Bugzilla query each to learn nothing.
Approval is the `fedora-review+` flag, not the bug being closed: a
review closed without it was abandoned rather than accepted, and the
report distinguishes the two.

The search itself moves out of `check-pkg-reviews` to be shared, and
now confirms a candidate with `review_request_package` rather than a
prefix comparison — exact where a substring is not, and no longer
silently missing a review whose summary uses something other than
" - " after the package name.

Still to come: Koji builds for a branch, Bodhi updates, and the
`--update` mode for the parts that need a person.


### ebranch: propose an update's bug list

Working out which bugs belong on a big update was one of the more
tedious parts of submitting one, and it is mechanical work. `--submit`
now proposes them, from two places.

Bugs still open against a package the update builds. The search is
deliberately not scoped to the update's release: the-new-hotness files
update requests against Rawhide and package reviews live under Fedora,
so an EPEL update's bugs are mostly not EPEL bugs.

And `rhbz#` references in the changelog entries the update introduces.
This is the half that earns its keep: a bug fixed in Rawhide is closed
when that build lands, so it is no longer open and the first source
cannot see it — while a branch update carrying the same fix still
closes it for that branch. Only the *new* entries are read, those
newer than the version-release the target already has, since scanning
the whole changelog would attach bugs fixed in releases the target has
had for years. Bug references are taken from `rhbz#123`, `bz#123`, a
`#123` following a Resolves/Fixes/Closes keyword, and Bugzilla URLs; a
bare `#123` is ignored, being as likely a GitHub issue, and this list
decides what gets closed.

A candidate is proposed only if the vote logic would score it +1, so
what gets attached and what gets voted on cannot disagree. Each is
shown with how it was found. `--yes` skips proposing altogether:
attaching a bug closes it when the update goes stable, which is not
something to do unasked.


### ebranch: judge FTBFS and FailsToInstall bugs

`check-update --give-karma` had nothing to say about the two kinds of
bug an update most often exists to fix.

An FTBFS bug claims a package does not build on a release. A build of
that package in the update is the artifact the bug says cannot exist,
so it now scores +1 — proof rather than inference, and the strongest
verdict here.

A FailsToInstall bug is answered from the check's own installability
analysis, which resolves exactly what the bug complains about: +1 when
the package's requirements all resolve, -1 naming the requirement that
does not. Weaker than the FTBFS case, since resolving against an
assembled repo set predicts what dnf would do rather than observing it,
so the reason quotes the requirement for the reader to weigh. A clean
result is not reported at all when the analysis could not run fully —
"nothing found" is not "nothing wrong".

Both kinds are recognized from two signals, and never from a loose
reading of the summary. The tracker a bug blocks is the general one: it
works whatever the wording, which matters because human-filed FTBFS
bugs follow none — "python-pyemd fails to build with setuptools 74+",
or an `[abrt]` crash report that never mentions building — and name no
release at all.

Fedora's bots do use fixed wording that names the release, so those
summaries are read as a second signal: `F44FailsToInstall: <component>`
and `<component>: FTBFS in Fedora rawhide/f44`. That covers what the
trackers cannot, since EPEL has no trackers and a lookup can fail. The
FTBFS form is matched on its numbered token and never on the literal
`rawhide`: the mass rebuild stamps whichever version was Rawhide when
the bug was filed, so a bug reading `rawhide/f44` is about f44 forever
while Rawhide has moved on, and matching the word would resurrect every
past cycle's bugs into today's.

Either way the release must match the update's, so a bug about another
release is left alone.

`karma::run` now takes the check report rather than a karma and reason
its caller derived from that report, which is also two fewer arguments.


### poi-tracker: retired-package triage found no bugs on numbered branches

`triage-retired` searched Bugzilla with `version=<branch>`, so a
retirement on `f43` queried `version=f43`. Fedora files its bugs
against a bare version number — `41` through `45`, plus `rawhide` — so
that query matched nothing, and no results is indistinguishable from a
retired package having no open bugs left to close. Only `rawhide` and
the `epel*` branches, where the branch name happens to be the Bugzilla
spelling, ever worked; those were also the only ones with tests.

Both paths were affected: the per-package search and `bugs_for_branch`,
which filters batch results by the same product and version pair.

The mapping now lives in
`sandogasa_bugclass::bugzilla::product_version_for_branch` and returns
`Option`: `f43` → `43`, `epel10.3` → `epel10` (minor versions share a
product version), and `None` for a branch Bugzilla has no product for,
such as `c10s` or `eln`. poi-tracker warns and skips those instead of
running a query that cannot match. Returning an `Option` is the point —
the old signature had to invent an answer for every input, which is how
the wrong one went unnoticed.


### ebranch: say when an update cannot be fixing a bug

An update that builds nothing for a bug's package cannot be fixing
that bug. `check-update` used to treat such a bug like any other it
had no opinion about, and just prompted with a bare default of 0. It
now suggests -1 and prints the reason ("update builds no rust-dtor").
`--yes` takes that suggestion, rather than posting a 0 there is reason
to think is wrong.

Only bugs that name their package outright are judged this way: an
update request names it through its Bugzilla component, a review
request through the package in its title. A CVE or FTBFS bug is not
classified here, and its component can be stale after a package
rename, so a missing match means nothing. Those keep the plain prompt.

`--submit` now screens the bug list before showing the submission
plan. A bug listed on an update that ships nothing for it will be
closed when that update goes stable, against a package it never
touched. Catching that before submission costs nothing, while a -1
afterwards is both noise and beside the point. Misfiled bugs are
printed with the package each one names, and you are offered the
chance to leave them off. `--yes` warns and keeps them instead:
silently discarding a bug passed to `--bug` is not something to do
unasked. A Bugzilla fetch failure likewise leaves the list untouched.


### sandogasa-bugclass: a package name is not a version prefix

`extract_new_version` stripped the component name off a release
monitoring summary and took whatever remained as the version, so a
component that is the *prefix* of a longer package name matched a bug
that was not about it. `rust-ctor` against "rust-ctor-proc-macro-0.0.13
is available" yielded `proc-macro-0.0.13`, and comparing that to a real
version left the verdict to whatever rpmvercmp made of it — it ranks a
leading digit above a leading letter, so `1.0.8` came out "newer" and
the bug looked fixed.

In `ebranch check-update --give-karma` that meant a confident, wrong
`+1` posted to Bodhi: an update shipping rust-ctor claimed to fix the
rust-ctor-proc-macro update request, with `update delivers
rust-ctor-1.0.8 >= proc-macro-0.0.13` as its stated reason. Bugs whose
package is genuinely absent from an update now fall through to the
interactive prompt, which is what should have happened.

The match is now anchored: after the component name the summary must
continue with a `-` and then a digit, so a remainder that begins
another name segment is no longer accepted as a version.

Anchoring on a name the caller supplies is the only reliable way to
read these summaries, because they are ambiguous on their own.
`rust-md-5-0.10.6` splits as `rust-md-5` + `0.10.6` or as `rust-md` +
`5-0.10.6` with equal justification, and both are real Fedora package
names (as are `rust-sha-1`, `rust-utf-8`, `rust-loopdev-3` and
`rust-z-base-32`). Splitting from the right the way
`sandogasa_koji::parse_nvr` does is no help either: a summary carries
no release field, and a version may itself contain a `-`
(`python-peak-rules-0.5a1.dev-r2707`).

poi-tracker's `semver-audit` and `triage-updates` and
sandogasa-pkg-health's pending-update check pass a component that came
from Bugzilla, so anchoring simply always succeeds for them; they
shared the flaw through `extract_new_version` and are fixed with it.

`ebranch check-update` was the one caller with no component to anchor
on — Bodhi's bug records carry only an ID and a title — which is why it
was reduced to trying each of the update's builds as a prefix. It now
reads each bug's component from Bugzilla, in the batched fetch it
already made to backfill titles Bodhi had not cached yet, and matches
that against the update's builds by exact name. Since the component
names the package outright, nothing has to infer where the name ends.
When the Bugzilla fetch fails the component is unknown and the bug goes
to the interactive prompt rather than being guessed at.

`classify` labelled a bug `BugKind::Update` on a bare
`summary.starts_with(component)`, so it had the same blind spot — a
`rust-ctor-proc-macro-0.0.13` summary read as an update request for
`rust-ctor`. It now anchors through `extract_new_version` like
everything else. This also makes the summary have to *end* with "is
available" rather than merely contain it, which is how
the-new-hotness files them.

This predates the review-request support added in the same release;
review requests match a build name exactly and were never affected.

### Release tooling: verify a publish landed

`make check-published` (`scripts/check-published.sh`) checks every
publishable workspace crate against crates.io at the workspace version,
listing anything missing and printing the re-run command. `cargo ws
publish` can stop partway — a 429, an interrupted upload, a crate whose
packaging fails — and a tag pointing at a half-published workspace is
worse than a tag that arrives late, so this now runs between publishing
and tagging. It takes an optional version argument to check a past
release.

The check exists as a script because the query has a trap: crates.io
requires a User-Agent under its API data access policy and answers
`200` with an error body when one is absent, so an ad-hoc `curl` check
without a UA reports *every* crate as missing — indistinguishable from
a failed publish.

The release notes in `AGENTS.md` also now record that `--publish-interval`
has to be computed rather than reused: every crate shares the workspace
version, so the number bumped per release is the full member count,
which is well past crates.io's burst of 30. There is a floor of roughly
`N - 30` minutes on the run whatever interval is passed. And publishing
rewrites `Cargo.lock` without the workspace's dev-dependencies, which
should be discarded rather than committed.

## v0.19.1

### Man pages for every tool

Each tool now ships a man page at `tools/<tool>/man/<tool>.1`,
generated from its clap definition by the new `sandogasa_cli::man`
module. One page per tool documents the whole CLI: the top-level
options, then every subcommand as a subsection under COMMANDS, so
`man ebranch` covers `check-update` and the rest rather than sending
the reader to nine separate pages.

Generating from clap is what keeps the pages honest — the man page and
`--help` are rendered from one definition, so they cannot disagree
about what a tool accepts. A `man_page_matches_cli` test in each
tool's `main.rs` fails when a committed page stops documenting a
visible flag or subcommand, which makes drift a test failure rather
than something a reader discovers. `scripts/gen-man.sh` regenerates
every page.

The pages are committed and included in the published crates, so a
packager can install them with `install -m644` without building or
running the binaries — a cross-build for another architecture cannot
execute the tool to scrape its `--help`.

Man page rendering is behind a new `man` feature on `sandogasa-cli`,
off by default and enabled only as a dev-dependency, so shipped
binaries do not carry the roff renderer. New workspace dependency:
`clap_mangen`.

### Man page footers name the version

The pages carried no version, and their `.TH` fields were shifted:
clap_mangen writes an unset date as bare whitespace, which groff reads
as an *absent* field, so every later field moved down one — the source
was parsed as the date and the manual as the source. The footer read
`Sandogasa Manual | sandogasa | koji-lag(1)` and the header fell back
to groff's generic "General Commands Manual".

Setting the date fixes the shift, and the footer now reads the way
`man bash` does — `sandogasa 0.19.0` at the left, the page's date in
the centre, `koji-lag(1)` at the right — with "Sandogasa Manual" in the
header.

Because the version is in the page, `man_page_matches_cli` also checks
it, so a page that falls behind the workspace version is a test
failure. Regeneration therefore has to follow the version bump at
release time, and forgetting it can no longer ship a stale footer.

Regenerating preserves the committed date of any page whose content
has not otherwise changed, so a one-tool CLI change does not restamp
the other fifteen.

### A Makefile for discoverability

The workspace's checks were spread across `scripts/` and a `cov` alias
in `.cargo/config.toml`, which meant knowing they existed before you
could run them. A root `Makefile` collects them: `make help` (the
default target) lists everything, `make check` runs what a pull request
should pass, and `make release-checks` adds the audit, semver and
coverage gates. `make man` regenerates the man pages.

It is a task runner, not a build system — cargo still builds and
installs, and distro packaging drives cargo directly without this file.

### ebranch: karma voting understands review requests

`check-update --give-karma` decided per-bug feedback automatically only
for release-monitoring bugs, whose summary carries a version to compare
against the update's builds. A new package's Review Request bug has no
version, so every one of them fell through to the interactive prompt —
exactly the case a `--type newpackage` update is made of, where each
bug names one of the packages being shipped.

A review request is now auto-voted `+1` when the update builds the
package under review, with `update ships <nvr>, the package under
review` as the recorded reason. A review of a package the update does
not build gets no automatic verdict: it may belong to a different
update, so it is still put to the user rather than voted down.

New in `sandogasa-bugclass`: `bugzilla::review_request_package`, which
extracts the package name from a `Review Request: <package> -
<description>` summary. The prefix match is case-insensitive and the
name is the first word after it, since package names contain dashes of
their own.

### Docs: CONTRIBUTING.md, and AGENTS.md for any agent

A new `CONTRIBUTING.md` states what a contribution needs: one
identified issue to fix or implement, tests that pass offline, `cargo
fmt` and no new clippy warnings, docs put where they belong, and a
signed-off commit.

Its AI section follows the [Fedora AI-Assisted Contributions
Policy](https://docs.fedoraproject.org/en-US/council/policy/ai-assisted-contributions/)
and the kernel's [AI Coding
Assistants](https://docs.kernel.org/process/coding-assistants.html)
guidance: the human is the author and accountable, assistance is
disclosed with an `Assisted-by: <agent>:<model-id>` trailer (never
`Co-Authored-By`), only a human may add `Signed-off-by` because the DCO
is a certification only a person can make, and a model must not be the
sole or final arbiter of whether a contribution is acceptable.

`AGENTS.md` at the top level symlinks to `.claude/CLAUDE.md`, so agents
other than Claude Code find the project conventions at the name they
look for. The trailer rule there is now agent-neutral, and records that
the ask-before-committing rule exists because the sign-off certifies
the DCO in a human's name.

The README also describes the project's actual scope — cross-
distribution packaging focused on Fedora, CentOS and Debian, with the
activity-tracking crates useful to anyone tracking their own
contributions.

### Docs: READMEs describe rather than justify

The tool READMEs had accumulated rationale for recent changes —
comparisons to how things used to work, arguments for why a default is
what it is. That belongs in this file and in the development notes, so
the READMEs now state what the tools do.

fedora-cve-triage's needed correcting as well: it still documented the
per-check subcommands retired in 0.19.0, and still described the
false-positive review as skipped when piped with every detected bug
closed unreviewed, which stopped being true when the review became
uniform. Its opening now describes the range of misfiled CVEs a
maintainer's queue collects, rather than leading with the bundled-
JavaScript case alone.

## v0.19.0

### sandogasa-distgit: send a User-Agent

The last reqwest client without one, and it talks to dist-git —
Fedora's infrastructure tarpits UA-less requests. It now builds through
`sandogasa_cli::http`, so it picks up the shared timeout and crypto
provider along with the UA.

### fedora-cve-triage: only `run` triages now (breaking CLI)

The six per-check subcommands — `js-fps`, `cross-ecosystem`,
`interpreter-fps`, `unshipped-tools`, `fix-version`, `bodhi-check` —
are removed. `run --check <name>` does what each did, and since
`[check."<name>"]` can scope a check to its own components, assignees
and skipped components, nothing was left that only a standalone
invocation could express. They had already been hidden from `--help`,
and keeping two entry points in step was the reason several of this
release's bugs existed at all.

Migration — one run config replaces the per-check ones, with the query
supplied by flags:

| Was | Now |
|---|---|
| `js-fps -f js-fps-folly-stack.toml` | `run --check js-fps` |
| `bodhi-check -f bodhi-check-salimma.toml` | `run --assignee michel@michel-slm.name --check bodhi-check` |
| `bodhi-check -f bodhi-check-freerdp.toml` | `run -c freerdp --check bodhi-check` |
| `fix-version --apply` | `run --check fix-version --apply` |
| `--close-bugs` | `--apply` |

`--status` is new, overriding the config's `statuses` the way
`--component` and `--assignee` already override theirs. The old
`fix-version` and `interpreter-fps` configs swept
`POST`/`MODIFIED` where the others did not; that divergence looks
accidental, so the shipped config uses `NEW`/`ASSIGNED` and `--status`
covers the wider sweep. The eight per-check configs under
`configs/fedora-cve-triage/` are deleted, leaving the maintainer-
agnostic `run.toml`; their component lists were the targets of past
runs rather than configuration, and are now flags.

### fedora-cve-triage: uniform per-bug review, plus -y (breaking CLI)

`--apply`/`--close-bugs` now reviews every bug through the same
keep/explain/remove flow, whichever check proposed it: keep performs
the planned action, explain performs it and records the note on the
bug, remove skips that bug. Previously only the false-positive checks
reviewed; `fix-version` and `bodhi-check` were single all-or-nothing
confirmations, so one wrong reassignment among seventeen could only be
avoided by narrowing the whole run.

New `-y`/`--yes` accepts everything without reviewing or confirming,
and `--claim` reassigns without asking. Claiming now goes through
`sandogasa_bugzilla::claim::resolve_claim`, so the project matrix
holds: `--claim` claims, `-y` alone declines rather than reassigning
bugs nobody asked it to, and an unset email skips silently.

**Breaking:** without a terminal and without `-y`, nothing is written.
The false-positive checks previously closed every detected bug
unreviewed when piped, while the other two stages refused — the same
invocation was maximally trusting on one path and maximally cautious
on the other. Automation that relied on the bulk close must now pass
`-y`, and `--claim` for the reassignment it previously lost.

### fedora-cve-triage: run's config is layered and shippable

`run --config` is now optional. Without it the profile is read from
`/etc/fedora-cve-triage/run.toml` merged beneath
`$XDG_CONFIG_HOME/fedora-cve-triage/run.toml`, user winning per key —
the same layering as the tool's own `config.toml`. A package can
therefore ship a complete profile (every check's tracker, reason and
scoping) while a user writes only `assignees = [...]`. An explicit
`-f` still names one file, unlayered, and with neither present the run
is refused naming both paths.

The profile could not simply be dropped at
`/etc/fedora-cve-triage/config.toml`: that path is parsed as the tool's
own `AppConfig` and would fail on the missing `[bugzilla]` section.
Supplying the path via `[defaults.run]` doesn't work either, since
`parse_with_defaults` injects defaults only after a first successful
parse and a missing required argument fails it.

### sandogasa-config: layered lookup for a differently-named file

`ConfigFile::try_for_tool_file(tool, file_name)` gives the
`/etc/<tool>/<file>` and `$XDG_CONFIG_HOME/<tool>/<file>` pair for a
file other than `config.toml`, so a tool can layer a profile without
hand-rolling XDG resolution. Permissions are enforced only for
`config.toml`, since such a profile is not where credentials belong.

### fedora-cve-triage: JS checks route by what the maintainer knows

Three changes to how `js-fps` and `cross-ecosystem` decide, after a run
closed three cachelib bugs with the wrong justification.

`sandogasa-nvd`'s `targets_js` no longer accepts a bare "javascript" in
a description. `javascript:` URIs are the standard XSS payload, so
advisories in every ecosystem mention them — Mistune, a Python
library, describes not blocking "percent-encoded javascript URIs" — and
that alone made `js-fps` propose closing a live Python CVE. The list
now needs a phrase naming the ecosystem (`javascript library`,
`node.js`, `npm package`, …); it was earning nothing anyway, matching
neither DOMPurify nor axios.

Both checks now share one JS determination, including the
GitHub-repository-language fallback that only `cross-ecosystem` had. A
CVE NVD hasn't analyzed carries no CPE data, so DOMPurify's and axios'
CVEs were invisible to `js-fps` and fell through to `cross-ecosystem`,
whose "different package in a different ecosystem sharing a name" is
untrue of a package that vendors those files as website assets.

Because nothing in a CVE says *why* a package contains JavaScript, the
distinction between the two checks is maintainer knowledge. `[check."<name>"]`
now accepts `components`, `assignees` and `skip_components`, filtered
over the run's single query, so `js-fps` can be scoped the way its
standalone config always was — without which it claims every JS CVE
and `cross-ecosystem` never fires.

### fedora-cve-triage: unshipped-tools no longer guesses at absence

The check closes a bug when the package doesn't ship the affected
tool, but it treated "cannot tell" as "not shipped" — so
`uutils-coreutils`, whose spec ships `%{_bindir}/uu_*`, looked like it
shipped no `mv` and CVE-2026-35354 was flagged as a false positive.
Globs cannot be reported by name, so their absence from the binary
list meant nothing, and the same held for a spec that could not be
fetched at all (a retired branch, a renamed component).

Both now bow out with a note naming the bug, leaving the call to a
human. Matching is also looser in the safe direction: one
`<word><-|_>` prefix is stripped, so `uu_mv` counts as shipping `mv`
where the binaries *are* named. `sandogasa-distgit` gains
`spec::binaries_are_globbed` for the first part.

### fedora-cve-triage: list the changes next to the confirmation

Every resolution stage now prints the bugs it is about to act on
immediately before asking to proceed. `fix-version` printed its plans
as it classified them, which under `run` happens up front for all six
checks — so "Apply 17 update(s)?" arrived with the list long scrolled
away behind other checks' output. `bodhi-check` printed its categorized
lists further up, with the reassign prompt and several other sections
in between, and its late-filed set was only ever counted, never
listed. The false-positive stage now names the bugs its plan covers
rather than just counting them.

A new `tools/fedora-cve-triage/DEVELOPMENT.md` records the design a
later check has to fit: the classifier contract, the shared `Triage`
context and why its accessors return owned data, why the check order
is what it is, NVD's authority over the fallbacks, narrowing versus
subtracting, and the rule that a plan belongs next to its prompt.

### fedora-cve-triage: --skip-component excludes packages from a run

`--component` is an allowlist, so "everything assigned to me except
this one package" meant enumerating every other component — which
defeats the point of an assignee sweep. `--skip-component NAME,...`
(and `skip_components` in the config) subtracts instead, on `run` and
every standalone check.

The exclusion is applied after the Bugzilla search, since its
component filter is a set to match rather than one to subtract from,
and matching is case-insensitive. A run that drops bugs says so and
names the components, because a quietly shorter run looks like a clean
one. Being a subtraction, a skip list does not satisfy the requirement
that `components` or `assignees` narrow the query.

### fedora-cve-triage: --help leads with `run`

The nine subcommands were listed as one alphabetical block, which gave
`run` no more prominence than the six checks it supersedes. Those
checks are now grouped below the options instead, in the order `run`
applies them, leaving `config`, `run` and `search` in Commands. clap 4
has no help heading for subcommands, so this is `hide` plus an
`after_help` section; the checks themselves are unchanged and
`help <check>` still works.

### fedora-cve-triage: --assignee and --component on every check

Apart from whose queue to sweep, a triage config is generic, so
`--assignee` (like the existing `--component`) now overrides it on
`run` and on all six standalone checks. Each flag replaces the
corresponding config list, since a flag asks a different question than
the file. `configs/fedora-cve-triage/run.toml` ships as a
maintainer-agnostic config for exactly this.

`js-fps`, `cross-ecosystem` and `unshipped-tools` previously required
`components` in their config; it is now optional, since `assignees`
can narrow the query instead. A config (or invocation) that narrows by
neither is refused rather than sweeping every CVE bug in the product,
which also replaces `fix-version`'s narrower "config must list
'components'" error.

### fedora-cve-triage: one `run` for every classifier

The six classifiers had to be invoked one at a time, in an order the
operator had to remember, and each did its own Bugzilla search and kept
its own NVD cache — so the same CVE was fetched once per check against
a service rate limited to one request every six seconds.

`run` applies them to one bug population in the configured order, the
first check that claims a bug keeping it. That ordering is the point: a
bug `js-fps` recognizes as a false positive is never judged against
Bodhi, and one filed against a branch the package never shipped on is
moved before its fix is looked for. The default order is `js-fps`,
`cross-ecosystem`, `interpreter-fps`, `unshipped-tools`,
`fix-version`, `bodhi-check`; `checks` in the config or `--check` on
the command line changes it or runs a subset.

Its config states the query once and then each check's settings under
`[check."<name>"]`. One search and one NVD lookup per CVE now serve
every check, as do the dist-git spec, branch, Bodhi and
GitHub-language caches.

Each classifier is now written once and shared: the standalone
subcommands keep their flags, configs and output, and call the same
functions `run` does. Every check's own review flow is preserved, so
`--apply` is as careful as the individual subcommands.

### sandogasa-bodhi: Clone on the response models

`Update`, `Build`, `BodhiRelease` and the types they contain derive
`Clone`, so a caller can hold an update while consulting another
cache.

### fedora-cve-triage: close bugs whose fix version NVD doesn't have

`bodhi-check` could only work from NVD's CPE data, so a CVE still
`Awaiting Analysis` was skipped as "No fixed version in NVD" even when
the fix had shipped weeks earlier — CVE-2026-14266 against 7zip being
the case in hand, where 7zip-26.02 went stable on 2026-07-01 and the
CVE was not published until 2026-07-29.

Two ways out, in preference order. A `[fixed_versions]` table in the
bodhi-check config supplies a version per CVE, consulted only when NVD
yields nothing, so it never overrides NVD and a stale entry is
harmless. Failing that, the advisories NVD links to are read for a
`fixed in <version>` line — ZDI ends every advisory that way, and the
oss-security posts quoting them inherit it. A scraped version is a
suggestion, not a decision: it is confirmed at a prompt (default no)
and merely reported in non-interactive runs, because closing a live
security bug on parsed prose should not happen unasked. Advisories are
read once per CVE, however many bugs track it.

The "No fixed version in NVD" list now names the CVE, NVD's analysis
state and the reference URLs, so it distinguishes "NVD hasn't looked
yet" from "NVD looked and nothing fixes it", and says where to check.

### sandogasa-nvd: expose analysis state and reference URLs

`CveResponse` gains `vuln_status()` and `reference_urls()`, and
`CveItem` a `vuln_status` field, so callers can tell an unanalyzed CVE
from one with no fix and follow its references. `FixedVersion` now
derives `Clone`, `CveItem` derives `Default`.

### sandogasa-config: the not-found error names both layers

`load`'s "file not found" error named only the user path, which
misdirects a deployment configured entirely from `/etc` — that file
was never meant to exist there. It now names whichever paths were
configured, joined by "or", since the error only fires when none of
them is present.

### Docs: License sections on three READMEs

`fedora-review-digest`, `sandogasa-review` and `sandogasa-sourcehut`
were missing the License section the other 38 crates carry; their
`LICENSE-APACHE`/`LICENSE-MIT` symlinks were already in place.

### Docs: system-wide configuration

Every tool reads `/etc/<tool>/config.toml` beneath the per-user
file, but that was only written down in `DEVELOPMENT.md` and the
sandogasa-config crate docs — no tool's README mentioned it. Each of
the 16 now carries a "System-wide configuration" section naming both
paths, distinguishing the nine that load their own settings from the
seven that read the file only for its `[defaults]` table.

`DEVELOPMENT.md` gains the details that packaging needs: no tool
ever writes under `/etc`, so a system file is always admin-authored
(an RPM wants to own the directory and `%ghost %config(noreplace)`
the file); a system file alone is sufficient, since `load` succeeds
with no user file; and the 700/600 enforcement applies to the user
file only, so a packaged `root:root 0644` is what to ship.

That last point is deliberate rather than a caveat: the system layer
is for shared, non-secret settings, and credentials stay in the
per-user file or an environment variable. Restricting a shared token
by group barely works on Fedora, where each user has their own
group; setgid would be both discouraged and a poor fit for tools
that exec `koji`, `fedrq` and `git`; and a shared token collapses
the audit trail at the far end. A machine that needs its own
credential should get its own user, or take the token from the
environment.

## v0.18.2

### fesco-chair: Council happenings in the agenda and the script

The wiki gained a standing Council-representative slot, so `script`
emits `!topic Council happenings` between the next-week's-chair action
and Open Floor, and the agenda announcement carries a
`= Council happenings =` section in the matching position, after New
business. The section is always emitted and left empty for the chair
to fill, like Followups and New business.

### fesco-chair: wrap both emails to 71 columns

The agenda announcement now wraps like the summary, sharing one
`sources::WRAP` so the width can be changed in one place. Two lines
of its static template were over the width and had been getting
rewrapped by the sending client, orphaning `#meeting:fedoraproject.org`
and `it`; they are re-flowed, and a test now asserts no rendered line
exceeds the width unless it carries an unbreakable URL — which guards
the template against the same thing creeping back.

Ticket entries in both emails are rendered by one `sources::entry`,
so a long title wraps the same way wherever it appears.

### fesco-chair: wrap the summary email to 71 columns

meetbot emits its minutes one line per event, with `AGREED`/`ACTION`
lines routinely past 200 columns, so something wraps them either way:
if `summary` doesn't, the sending mail client does — at its own width
and with continuations flush left, which loses the bullet structure
the minutes rely on. `summary` now wraps the body itself, keeping
each line's leading indent and `*` bullet and aligning continuations
under the text. Ticket titles wrap the same way.

The width matters more than it looks. It must not exceed the
client's, or every line is broken a second time and the result is
littered with one- and two-word orphans; the 2026-07-28 summary went
out wrapped at 80 and its archived copy shows exactly that. 71 rather
than the conventional 72 because that archived copy contains
surviving 71-column lines — so 71 is known to pass through untouched,
while nothing in it reaches 72 despite lines clustering at 66-71.
Both are within the convention; this is the one with evidence.

Lines whose URL alone exceeds the width still overflow — splitting a
URL stops it being a link — and such a URL stays on the line it
started on rather than orphaning the label introducing it. That's the
four artefact links and the occasional `LINK:` line.

`wrap_prefixed` moved from ebranch to `sandogasa_cli`, which is where
the second caller found it.

### fesco-chair: summary announces late in-ticket votes

Tickets sometimes get tagged `pending announcement` after the
schedule announcement has gone out, which left them with no
announcement at all — the agenda had already been sent and the
summary only carried the minutes. `summary` now lists every open
`pending announcement` ticket in a "Discussed and Voted in the
Ticket" section between the artefact links and the minutes, with the
tally parsed from its comments, and reminds the chair to comment,
untag and close them. Since the label is dropped once an
announcement goes out, anything still carrying it is by definition
unannounced.

This adds a tracker lookup to a subcommand that previously only
needed meetbot, so it is best-effort: with no token configured or an
unreachable tracker it warns and prints the minutes as before. The
`--json` output gains a `voted` array.

### sandogasa-cli: shared confirm prompt and HTTP plumbing

Two additions that existed in copy-pasted form across the workspace:

- `confirm(question, default_yes)` prompts on stderr as
  `question [Y/n]: ` / `question [y/N]: ` and reads one line from
  stdin. It replaces ~15 hand-rolled variants whose semantics had
  drifted apart.
- A new `http` feature (off by default, so network-free tools don't
  pull in reqwest) provides `TIMEOUT`, `builder`/`blocking_builder`
  (crypto provider installed, user agent and timeout set) and
  `ok`/`json_ok`/`blocking_ok`/`blocking_json_ok`, which turn a
  non-success response into `"{what}: HTTP {status}: {body}"`.

### Workspace: deduplication sweep

Roughly 1,800 lines removed with no intended change in behavior,
mostly by adopting the two helpers above and collapsing repeated
local patterns. Highlights: sandogasa-gitlab lost 20 copies of the
HTTP status check and four pagination loops; sandogasa-forgejo's
three pagination loops became one; sandogasa-fedrq's twelve query
methods share one command builder and one column parser;
poi-tracker's `cmd_*` functions return `Result` instead of
hand-rolling the error-to-exit-code dance 39 times;
cpu-sig-tracker's `retire`/`untag` duplication moved into shared
modules and its four blocking JIRA wrappers became one (now sharing
a single tokio runtime); hs-relmon and cpu-sig-tracker dropped their
pure-delegation GitLab newtype wrappers in favor of re-exporting
`sandogasa_gitlab` directly; hs-intake's three `compare-*` modules
share one implementation; sandogasa-report gained a `forge` module
holding the per-forge token lookup and date-range helpers that four
modules had each copied.

Interactive prompts across every tool now come from the shared
helper, which unifies three small details that previously varied per
call site: prompts go to stderr with a trailing colon, `yes`/`no`
are accepted as full words alongside `y`/`n`, and an unrecognized
answer takes that site's default instead of being read as "no".
HTTP error strings from the affected client crates now use the
shared `"{what}: HTTP {status}: {body}"` wording. The
`fedora-cve-triage` missing-`bodhi` message comes from
`require_tools` and reads `missing required tool(s): bodhi
(install: sudo dnf install bodhi-client)`.

Two fixes fell out of the sweep: `sandogasa-gitlab` and
`sandogasa-repology` were sending a hardcoded `0.6.2` user agent
(now derived from `CARGO_PKG_VERSION`), and `sandogasa-jira`,
`-mailman`, `-nvd`, `-bugzilla` and `-discourse` were sending none
at all.

### fesco-chair: followup inference reads number-last topics

`extract_ticket_numbers` only recognized `* TOPIC: #NNNN Title`
minutes lines, but chairs also write the ticket number last
(`* TOPIC: Title #NNNN`, as at the 2026-07-21 meeting) — so none of
that meeting's tickets counted as previously discussed and #3636
landed under New business instead of Followups. Every `#NNNN` on a
TOPIC line now counts.

### fesco-chair: parse lowercase decision tallies

The in-ticket decision parser only matched uppercase
APPROVED/REJECTED, so a concluding comment like "… my proposal is
approved (+6, 0, 0)" (ticket #3634) left the DECISION placeholder in
the agenda. The verdict is now matched case-insensitively and
normalized to uppercase in the announcement.

### fedora-review-digest: run fedora-review, with staged-dependency repos

A new `run` subcommand drives `fedora-review` itself and flows the
finished result straight into the digest, instead of only consuming
a directory produced by hand. Its reason to exist: staged group
updates whose dependencies live in a COPR before anything lands in
Rawhide (e.g. rhbz#2497354, rust-gufo-svg needing gufo-common ≥
2.0.0~alpha while Rawhide had 1.1) — `--copr OWNER/PROJECT`
(repeatable/CSV) enables the staging COPR's repo for the mock
build, `--repo URL` passes an arbitrary repo baseurl, and
`-m`/`--mock-config` picks the mock config (which doubles as the
COPR chroot name). Extra repos reach mock via `--addrepo` through
`fedora-review -o`, restating fedora-review's default mock options
(which `-o` would otherwise replace). `--dry-run` prints the
assembled command; `--no-digest` stops after the build. The
`fedora-review` binary is only required by `run` and is probed
before starting.

`run` also takes `--uniqueext SUFFIX` (mock `--uniqueext`), giving
the review its own buildroot so it can proceed alongside a mock
build already running in the same chroot (mock doesn't abort
cleanly when the chroot is in use), and a general
`--mock-option OPT` (repeatable) that appends any other
single-token mock option to the `-o` string.

### koji-lag: windows are half-open

Completion windows are now half-open `[start, next-midnight)`
instead of closed at both ends: a build completing at exactly
00:00:00.000000 UTC belongs to the day that starts then, never to
both adjacent days. With microsecond timestamps this boundary was
a measure-zero edge (and dataset merges already deduped it by task
id), but single-day *reports* could in principle have counted such
a build twice — now they can't. `--since`/`--until` still name
whole inclusive days on the CLI; the dataset schema's
`FetchWindow.to` is documented as the exclusive bound.

## v0.18.1

### New koji-lag tool

Quantify Koji build queue lag and per-arch build-time drag — the
"one slow architecture delays every build" problem (s390x
foremost), made worse for scratch builds that gate dist-git PR CI
at lower priority. `fetch` collects a completion-time window of
build/buildArch task metadata from a Koji hub (anonymous XML-RPC;
no credentials or koji CLI needed; paced and resumable); `merge`
pools independently collected datasets (records dedupe per
instance + task id, coverage windows coalesce, gaps are reported);
`report` gives per-arch queue-wait and build-time distributions
(median/p90/max), critical-path attribution (which arch finished
last per build and how far behind the runner-up — the marginal
delay it cost), and a scratch-vs-official split, with `--json` for
machine consumption. Human tables are padded Markdown pipe tables
(terminal-readable, pasteable into tickets) with a built-in column
legend. Datasets are plain JSON with a checked-in schema.

Windows cover whole UTC days and select builds by completion
time: `--days N` means the last N *complete* days (ending at
today's 00:00 UTC regardless of local time), a dateless end never
includes the partial running day, and a still-running build is
picked up by whichever later fetch covers the day it finishes —
so periodic few-days-at-a-time collection counts every build
exactly once, with no partial-day seams.

The fetch strategy was shaped by live measurement on a hub under
mass-rebuild load, where even five-minute completion-window
filters timed out: no server-side completion filters at all.
Parent build tasks are found by walking `listTasks` pages
newest-first by task id (an index walk, ~1.3s/500 rows under that
same load) and windowed client-side; per-arch children come from
parent-batched queries against koji's task(parent) index (~0.5s).
Validated end to end against Fedora's hub — one day of
mass-rebuild volume (5.5k builds / 15k tasks) fetches in minutes
and immediately quantified the problem: s390x median queue wait
2.6h (build time only 3.7m — starvation, not speed), bottlenecking
1120 of 3502 builds for 2337 machine-hours in a single day, worse for
scratch. stream/cbs instances are registered but untested, and
DB-dump ingestion is a recorded TODO.

### sandogasa-kojihub: shared Koji hub XML-RPC client

koji-diff's hand-rolled XML-RPC layer moved to a new
sandogasa-kojihub crate (koji-diff re-exports it, so
`koji_diff::xmlrpc` paths keep compiling — non-breaking; note
that cargo-semver-checks flags the cross-crate re-exports as
removed items, a known false positive for source compatibility
since it neither inlines foreign items nor accepts a kind change,
verified by compiling the old usage patterns), joined
by a typed `hub` layer: `HubClient` with `listTasks` (single
pages, a descending-id walk, and a completion-window bisection
sweep), `getTaskInfo`, `listHosts`, `listChannels`, and an
Option-tolerant `HubTask` record carrying the UTC unix timestamp
fields.

The wire client gained hardening that live testing against Fedora
infrastructure proved necessary (all three failure modes are
documented in DEVELOPMENT.md): a User-Agent (UA-less heavy
requests are tarpitted), no connection pooling (heavy queries on
reused keep-alive connections stall), HTTP/1.1 only, HTTP-level
failures surfaced as retriable errors, and error messages that
include the reqwest source chain instead of the opaque "error
sending request". hs-relmon's separate CBS XML-RPC client is a
recorded migration TODO.

### hs-relmon: unresolvable --track references are loud, not silent

`check-latest --file-issue` silently skipped filing/updating the
issue when the `--track` reference version couldn't be resolved —
with no reference nothing counts as outdated, so the run printed a
correct-looking table and did nothing (observed with `perf`, whose
repology project is empty; the kernel's perf lives under repology
project `linux`, and the `--repology-name` override wasn't passed).
Resolution failure now prints a warning naming the repology project
and the remedy, and `--file-issue` with an unresolved reference is
a hard error in single-package mode (counted, non-fatal, in batch
runs).

### poi-tracker: adopt orphaned packages

New `adopt` subcommand — the action counterpart to
sandogasa-pkg-health's orphaned flag: walk the inventory, find
packages whose dist-git owner is the `orphan` sentinel user, show
each one's orphaning reason, and take ownership via the dist-git
plugin's take-orphan endpoint (the API behind the web UI's "Take"
button). Interactive runs confirm each package individually
(adoption is a commitment — no batch yes/no); `-y` adopts every
match, `--dry-run` lists without needing credentials, and
inventory-marked retired/unshipped packages are skipped (dist-git
refuses retired packages; those need a releng ticket).

Requires a dist-git API token with the "Modify an existing
project" ACL: `poi-tracker config` now prompts for it (stored
under `[dist-git] api_token`, matching sandogasa-pkg-acl), with
`--api-token` / `PAGURE_API_TOKEN` overrides. sandogasa-distgit
gains `orphan_info` (orphan state + reason) and `take_orphan`
(surfacing the server's actionable error detail, e.g. "You must
be a packager to adopt a package.").

### All tools: system-wide config layer at /etc/<tool>/config.toml

Config reads now layer: an optional `/etc/<tool>/config.toml` is
read first and overridden per key (recursively for tables) by the
user's `~/.config/<tool>/config.toml`, with command-line flags
overriding both. This applies to every tool automatically —
`ConfigFile::load` does the merging — and to the `[defaults]`
flag-defaults table below. `save` (interactive `config` flows)
only ever writes the user file. Previously a missing user file was
an error even if an admin had provided settings; now the system
layer alone suffices. sandogasa-config additions:
`system_config_path`, `merge_tables`, `ConfigFile::read_merged`,
`with_system_path`, `describe_sources`.

### All tools: pin flag defaults in the config file

Every tool now honors a `[defaults]` table in its
`~/.config/<tool>/config.toml`, so flags you always pass can
become defaults. A top-level key applies tool-wide — to global
flags and to any invoked subcommand carrying that flag — so
`[defaults] explain = true` makes every dbranch subcommand with
`--explain` narrate; `[defaults.<subcommand>]` sub-tables scope a
default to one subcommand. Keys are the flag's long name;
booleans turn flags on, strings/numbers become values, arrays
repeat repeatable flags. Precedence: the command line (and a
flag's env var) always wins; a default that conflicts with an
explicitly-given flag is skipped rather than erroring; the new
`--no-defaults` flag (added to every tool) ignores the table for
one run; unknown keys are hard errors. The mechanics live in the
new `sandogasa_cli::defaults` module (`parse_with_defaults`) and
the pattern is documented in DEVELOPMENT.md. sandogasa-config
gains `ConfigFile::try_for_tool` (non-panicking variant of
`for_tool`).

## v0.18.0

### API model structs are now `#[non_exhaustive]` (breaking)

Response-model structs in the client crates are now
`#[non_exhaustive]`, so future field additions (which happened
twice this cycle: `BodhiRelease.stable_tag`, pkg-health
`Context.koji`) stop forcing breaking releases. **What breaks:**
literal construction (`Struct { .. }`) and exhaustive
destructuring of these types outside their defining crate no
longer compile. Affected: sandogasa-bodhi (all response models:
`Update`, `Build`, `BodhiBug`, `BodhiRelease`, `Comment`,
`Caveat`, the `*Response` wrappers, …), sandogasa-bugzilla
(`Bug`, `Flag`, `Comment`, `CommentBucket`, the `*Response`
wrappers), sandogasa-distgit (`ProjectAcls`, `AccessUsers`,
`AccessGroups`, `Contributors`, `ContributorLevels`,
`ProjectInfo`, `PullRequest`, `PullRequestProject`,
`PullRequestsResponse`), and sandogasa-pkg-health's `Context`.
Request structs callers must build (`NewUpdateFromTag`,
`BugFeedbackItem`) and sandogasa-koji's parser products
(`TaggedBuild`, `TagAddEvent`) stay constructible. **Migration:**
construct test fixtures via `serde_json::from_value` (the
pattern the workspace's own tests use), and `Context` via
`Context::new`.

### anstream 0.6 → 1.0

Major dependency bump, bundled with this breaking release per the
dependency policy. Only dbranch's terminal UI uses it and its
macro API is unchanged. rust-anstream 1.0.0 is already in every
Fedora branch (rawhide through epel9), so Fedora packaging is not
blocked. clap still pulls in anstream 0.6 transitively; the two
coexist until clap catches up.

### fesco-chair: meeting script uses the `!fesco` alias again

The `!fesco NNNN` maubot alias is fixed and deployed
(fedora-infra/maubot-fedora#154), so `script` emits `!fesco NNNN`
for plain fesco/tickets issues instead of the
`!forge issue fesco tickets NNNN` workaround. Items on other
repos (e.g. fesco/docs) and pull requests keep the explicit
`!forge` lookup — the alias doesn't cover them.

### sandogasa-pkg-health: new pending_update check; shared semver classifier (breaking)

pkg-health gains a `pending_update` check (Medium tier): the
persisted, aged counterpart of poi-tracker's `semver-audit`. It
finds the package's open release-monitoring bug, compares the
advertised version against the rawhide spec, and classifies the
bump by semver impact — including the Koji-verified stale/pending
distinction (a version built only into a side tag reports as
"committed, awaiting release", never as a stale bug). Uses the
`koji` CLI when available (anonymous queries); degrades to the
spec-only verdict with a startup warning otherwise.

To keep the two tools classifying identically, the version
classifier moved from poi-tracker's `semver_audit` module to
`sandogasa_bugclass::semver` (`Bump`, `classify`,
`classify_with_status`, `version_at_least`, `numeric_components`),
and the spec preamble parsers moved to `sandogasa_distgit::spec`
(`parse_field`, `parse_version`). poi-tracker's `semver-audit`
behavior and JSON output are unchanged; it stays as the
interactive one-shot view per the observe/act seam recorded in
TODO.md.

**What breaks:** pkg-health's library `Context` gained the pub
`koji` field (and is now `#[non_exhaustive]`, see above) — code
constructing `Context` with a struct literal must switch to
`Context::new`. poi-tracker's `semver_audit` module no longer
exports `Bump`/`classify`/`version_at_least`/`parse_spec_*`
(binary-internal, but noted for completeness) — they live in
`sandogasa_bugclass::semver` and `sandogasa_distgit::spec` now.

### sandogasa-pkg-health: flag orphaned packages

`maintainer_count` treated the dist-git `orphan` sentinel user as a
regular maintainer, so an orphaned package (observed with `ccze`,
whose owner releng moved to `orphan`) inflated its effective count
by one and gave no signal that it needs adoption. The check now
reports an `orphaned` boolean, excludes `orphan` from the direct
and effective counts, and leads the summary line with
`ORPHANED (adopt or lose it to retirement)`. Reports written by
older versions render unchanged (the missing field just omits the
marker).

### poi-tracker: don't call a bug stale when the build is only in a side tag (breaking)

`semver-audit` classified a bug as "up to date (stale bug)" as soon
as the rawhide spec's `Version:` matched the bug's advertised
version, and `triage-updates`' stale check treated the same spec
match as "shipped" when Bodhi had no update — so a version
committed to dist-git and built only into a side tag (observed
with rust-tree-sitter-elm 5.9.4, tagged only into
`f45-build-side-144259`) read as stale, and triage-updates would
offer to close its bug as ERRATA prematurely.

Both now verify against Koji instead of trusting the spec: a
version only counts as shipped when a build carrying it is tagged
into the release's stable tag chain (`koji list-tagged --latest
--inherit` on the Bodhi-provided `stable_tag`, so `f43-updates`
also covers `f43`, and the EPEL equivalents — while side tags and
`-candidate`/`-testing` tags never match). Bodhi remains the
primary source (it has the update alias/status); Koji covers what
Bodhi legitimately can't vouch for — content inherited from an
older release or answers lost to an outage. `triage-updates`' bug
comment for such closes now cites the tagged build instead of a
spec-reconstructed NVR.

- `semver-audit` gains a **"Committed, awaiting release"** category
  (JSON `bump` value `pending-release`, additive) for
  spec-matches-but-not-tagged packages; "up to date (stale bug)" is
  now Koji-verified.
- The `koji` CLI is now used by `semver-audit` and `triage-updates`
  (`sudo dnf install koji`); both degrade with a warning when it's
  missing — unverifiable bugs are left open / reported as up to
  date, never closed on spec evidence alone.
- sandogasa-koji: new `latest_tagged(tag, package, profile)`
  (package-scoped, inheritance-following) and `is_available()`.
- sandogasa-bodhi: `BodhiRelease` gains `stable_tag` (defaulted, so
  existing fixtures keep deserializing). **What breaks:** the new
  pub field breaks struct-literal construction of `BodhiRelease`
  outside the crate (the struct is now `#[non_exhaustive]`, see
  above); construct fixtures via serde instead.

### Bug-closing tools uniformly offer to claim the closed bugs

New project rule: any tool that closes bugs also offers to reassign
them (`assigned_to`) to the person running the command — triaging is
a benefit in itself, and the person cleaning up stale bugs may want
the credit. The mechanics now live in one place, a new
`sandogasa_bugzilla::claim` module: `resolve_claim` encodes the
decision matrix (`--claim` claims without prompting, `-y` without
the flag declines, no configured email skips silently, otherwise
prompt) and `apply_claim` adds `assigned_to` to an update body.

- **poi-tracker `triage-updates`** gains `--claim` (the previously
  missing surface): bugs closed as ERRATA by the stale-bug check can
  now be claimed, with the same flag/prompt semantics as
  `triage-retired`. Bugs moved to `MODIFIED` keep their assignee —
  they stay open and belong to whoever owns the in-flight update.
- **poi-tracker `triage-retired`** ported to the shared module; the
  prompt now includes the closure count ("Also claim ownership of
  the N bug(s) being closed …").
- **fedora-cve-triage**'s reassign prompts route through the shared
  matrix (behavior unchanged: it prompts whenever an email is
  configured).
- **fedora-review-digest** uses `apply_claim` for its review-claim
  body (behavior unchanged).

### sandogasa-pkg-acl: don't misreport flaky infra as a missing user

`user_exists`/`group_exists` in sandogasa-distgit treated any non-2xx
response as "does not exist", so a transient 502/503 from
src.fedoraproject.org made `pkg-acl set` (and `give`) claim the user
or group doesn't exist — observed live with `set --user mikelo2`
failing and the identical rerun succeeding seconds later. Existence
checks now retry transient server and transport errors with backoff
(like the other GET paths already did), treat only a 404 as "does not
exist", and surface anything else as an explicit failure ("could not
verify user '...' exists on dist-git: ..."). A new root
`DEVELOPMENT.md` records the general gotcha: Fedora infrastructure is
flaky; only a 404 means "not found".

### dbranch: `--debusine-project` for shared multi-package workspaces

The Debusine upload path composed the workspace and workflow names
from the source package (`r-<owner>-<srcpkg>`,
`publish-to-<suite>-<srcpkg>`), which assumed one workspace per
package. The new `--debusine-project <project>` flag on `rebuild` and
`update` (requires `--debusine`) overrides the project part in both
names, for a shared workspace hosting several packages — Debusine's
`r-YOURNAME-PROJECTNAME` naming doesn't require the project to be a
package name. Defaults to the source package as before.

### sandogasa-report: document the config-before-report setup path

External feedback (from pairing sandogasa-report with hansei): it
wasn't clear how to make sure all credentials get asked for. The
README now spells it out — `sandogasa-report config -c config.toml`
walks every forge instance used by the config's domains and prompts
for the full credential set (FAS username, Bugzilla email, Sourcehut
git emails, per-instance usernames and API tokens for
GitLab/GitHub/Forgejo/Sourcehut), so a single run covers any
`report -d` combination from that config. Documentation only; no
behavior change.

## v0.17.0

### sandogasa-report: fix dropped salsa tags and gitlab.com release noise (breaking sandogasa-gitlab API)

What broke: sandogasa-gitlab's `Tag.created_at` changed type from
`String` to `Option<String>` — consumers reading it as a `String`
must handle the `None` case (undated old tags). `project_releases`'s
403 handling is behavioral only. (cargo-semver-checks has no lint for
field type changes, so it reported no bump required; this is a real
compile break for library consumers.)

Two log findings from the H1 report run:

- Old tags on salsa come back with `created_at: null` (2021-era tags
  predating GitLab's tag-timestamp recording); the non-optional field
  in sandogasa-gitlab's `Tag` made the whole page fail to decode,
  silently dropping the *entire project's* tags from the report
  (michel/distrobox has 86 tags, 5 of them undated). `created_at` is
  now optional, and the report treats an undatable tag as
  out-of-window.
- Projects with the releases feature disabled (gitlab.com dist-git
  mirrors) answer the releases endpoint with 403 Forbidden;
  `project_releases` now treats that like 404 — "no releases", not a
  logged error.

(The third logged warning — sourcehut "reference not found" for an
empty repo with no default branch — was already handled by design:
the repo is skipped and the section continues.)

### ebranch: check-update separates FTI from FTBFS — and catches both

The reverse-dependency check had two defects that compounded into
false "0 would break" verdicts:

- **Exact string matching (FTI bug):** a reverse dep was flagged only
  when a binary Requires was *byte-identical* to an old Provide. A
  Provide reads `crate(sha3/default) = 0.10.8` while a Require reads
  `crate(sha3/default) >= 0.10.0` (or its rich form), so versioned
  deps could never match — the old check was effectively a
  soname-removal detector (unversioned Require vs removed unversioned
  Provide), blind to every versioned pin.
- **SRPMs never inspected (FTBFS gap):** only `subpkgs_requires`
  (binary subpackages) was queried, so BuildRequires like bear's
  `(crate(ctor/default) >= 0.6.0 with crate(ctor/default) < 0.7.0~)`
  were structurally invisible across a ctor 0.6→1.0 bump.

The *discovery* step was always right — `fedrq whatrequires` resolves
versioned and rich reverse deps — which is why affected packages were
listed as "checked" and then every one silently dropped by the
confirmation step. The practical consequence: `--give-karma` derived
**+1** ("no issues found") for updates that break reverse deps.

Reverse deps are now checked on two axes, each evaluated with full
rich-dep semantics and RPM version comparison against the update's
*new* provides:

- **FTI** (fails to install) — a binary subpackage's Requires stops
  resolving once the update ships
- **FTBFS** (fails to build from source) — the source package's
  BuildRequires stops resolving for its next rebuild

The report summary shows the split (`11 would break (4 FTI, 10
FTBFS)`) and each broken requirement is labeled; in `--json`,
`BrokenRequires` gains a `kind` field (`fti` / `ftbfs`). When a
touched capability is also provided by an unrelated package the check
can over-report — the safe direction. Verified against the
uutils-and-nushell staging COPR, where the old check reported nothing
and the new one flags all 11 reverse deps, including the not yet
rebuilt nushell and uutils-coreutils themselves.

sandogasa-fedrq gains `src_requires` (a source package's
BuildRequires via `fedrq pkgs --src -F requires`, additive).

### ebranch: check-update accepts COPR projects

Big coordinated updates often stage in a COPR before any side tag or
Bodhi update exists; `check-update` now takes a COPR as its input —
an `owner/project` spec (`@rust/uutils-and-nushell`) or the project
URL — alongside side tags and Bodhi aliases. The update contents come
from COPR's public monitor API (latest succeeded build per package in
the chroot matching the branch, x86_64 preferred), and the provides
comparison runs through fedrq's `@copr:` repo class like the
`@testing` path (COPR repos index source RPMs and maintain their own
repodata — no koji involvement, and koji is no longer required for
COPR input). COPR input requires `-b`; `--testing-branch` picks the
chroot when `-b` is a base branch (`-b al9 -r @epel --testing-branch
epel9`). `--give-karma`/`--submit` are rejected for COPRs, which
publish through their own repos.

New sandogasa-copr library crate: the monitor client plus the pure
branch→chroot and NVR-extraction helpers.

### quick-xml requirement tightened back to "0.41"

rust-quick-xml 0.41 has reached Fedora and EPEL, so the temporary
`">=0.40, <0.42"` range (which let Fedora builds resolve against its
packaged 0.40.1 while everyone else got the RUSTSEC-2026-0194/-0195
fix) is no longer needed. Resolution is unchanged for `Cargo.lock` and
`cargo install` users — both were already on 0.41.0.

## v0.16.0

### ebranch: check-crate includes unmet-version deps by default (breaking CLI)

`check-crate --transitive` now expands unmet-version dependencies
(packaged but too old for the requirement) by default — omitting them
silently under-reported what needs rebuilding. What broke: the
`--include-unmet` flag is **removed**, replaced by `--exclude-unmet`
to restore the old skip-them behavior. (`--include-optional` is
unchanged: on-by-default would be noisy until feature-aware resolution
lands; see TODO.)

### ebranch: no implicit EPELPackagersSIG tracker (breaking CLI)

The defunct EPEL Packagers SIG's tracker bug is no longer blocked
automatically — a behavior carried over from the Python ebranch. What
broke:

- `file-request` without `--blocked` used to substitute the
  `EPELPackagersSIG` alias; it now blocks nothing. Pass
  `--blocked <bug-or-alias,...>` explicitly to block trackers.
- `file-requests` (the batch path) always blocked `EPELPackagersSIG`
  with no way to override; it now blocks nothing by default and gains
  the same `--blocked` flag, applied to every request it files.
- The `branch_request::EPEL_SIG_TRACKER` constant is gone, and
  `run_file_requests`/`file_batch` take a `blocked` parameter.

Migration: add `--blocked <your tracker>` to filing invocations that
relied on the implicit tracker (`--blocked EPELPackagersSIG` restores
the literal old behavior, but that tracker is unmaintained). `--sig`
is unchanged and was always explicit.

### XDG base-directory compliance audit

All of our own per-user storage now resolves through the `dirs` crate,
which fully implements the XDG Base Directory spec — including the rule
that a *relative* `$XDG_*` value is invalid and must be ignored:

- sandogasa-fedrq's cache clearing (`--refresh`) previously accepted a
  relative `$XDG_CACHE_HOME` verbatim; it now falls back correctly, and
  the no-home panic message names both `XDG_CACHE_HOME` and `HOME`
- fesco-chair's agenda state likewise accepted a relative
  `$XDG_STATE_HOME`; it now uses `dirs::state_dir()`
- sandogasa-config's `ConfigFile::for_tool` and sandogasa-bodhi's
  `cli_cache_path` used a literal `~/.config` fallback when no home
  directory could be determined — which would silently resolve to a
  `./~/.config` path in the current directory; they now fail loudly
  with a message naming `XDG_CONFIG_HOME`/`HOME`

Everything else was already compliant: tool configs go through
`sandogasa_config::ConfigFile::for_tool` (`$XDG_CONFIG_HOME`, default
`~/.config`) and sandogasa-hattrack's holiday cache uses
`dirs::cache_dir()`. Paths owned by *external* tools (`~/.gbp.conf`,
`~/pbuilder`, `~/.fedora.upn`, bodhi-client's and debusine-client's
config files) intentionally follow those tools' own conventions.

### New fesco-chair tool

A helper for [FESCo meeting chair
duties](https://fedoraproject.org/wiki/FESCo_meeting_process) — it
prepares text to paste, it never sends or posts:

- `agenda` — the announcement email for devel@, built from the FESCo
  Forgejo tracker: open `pending announcement` tickets under "Discussed
  and Voted in the Ticket" with their decision parsed from the
  concluding vote comment (`APPROVED/REJECTED (+X, Y, Z)`), `meeting`
  tickets split into Followups vs New business by scanning recent
  meetbot minutes for each ticket's `TOPIC: #NNNN` line, overridable
  per ticket with `--voted`/`--followup`/`--new`. Open fesco/docs
  issues and PRs are offered onto the agenda (prompted per item on a
  terminal, `--docs <N,...>` non-interactively; unselected items show
  up in `--json` as `docs_open`)
- `script` — the day-of checklist (reminder command, quorum, 15-minute
  topic rule) plus the meetbot command script
  (`!startmeeting`/`!topic`/`!forge issue`/`!forge pr`/`!agreed`/…; the
  broken `!fesco` alias is avoided until maubot-fedora#154 deploys),
  pipeable to a file. `agenda` saves its assembled result to
  `$XDG_STATE_HOME/fesco-chair/agenda.json` and `script` replays it for
  the same meeting date (no refetching or re-prompting; any override
  flag reassembles instead), and `summary` clears it after the meeting
- `summary` — the post-meeting "Summary/Minutes" reply email: artefact
  links plus the full plain-text minutes, discovered on meetbot by date

Requires a Forgejo API token — store it with `fesco-chair config`
(validated against the instance, saved with restricted permissions),
or set `FORGEJO_TOKEN_FORGE_FEDORAPROJECT_ORG` / `FORGEJO_TOKEN` (the
sandogasa-report convention, which overrides the stored token). The
chair workflow will grow ticket updates later. sandogasa-forgejo gains
`Client::repo_issues` (list issues by state + label names, paginated)
and `Client::issue` (fetch one issue) to support it (additive).

### ebranch: check-update `--submit` — pre-flighted Bodhi submission

`check-update <side-tag> --submit` runs the reverse-dependency check
first and, only when it passes, creates the Bodhi update from the side
tag (the API behind `bodhi updates new --from-tag`) — so a subpackage
update that is accidentally missing a package is caught before anything
is published. Update notes come from `--notes <text>` inline or
`--notes-file <path>` for longer descriptions; `--type`, `--severity`,
`--bug <ID,...>`, and `--stable-karma`/`--unstable-karma`/
`--disable-autokarma` mirror the bodhi CLI. A non-passing check goes
through the same keep/explain/remove curation as `--give-karma` and
then asks whether to submit anyway (default no); non-interactive runs
and `--yes` never submit a failing update. The submission plan
(packages, type, bugs, thresholds, notes preview) is confirmed before
posting, and notes/session/flag validation happens before the analysis
so mistakes fail in seconds. After submitting, the check report is
posted on the new update as a review comment via the `--give-karma`
flow — per-bug feedback records whether each listed bug is addressed
by the delivered versions (Bodhi zeroes the submitter's overall karma;
per-bug feedback still counts), and `--comment` adds reviewer notes.
Bug titles Bodhi hasn't cached yet are backfilled straight from
Bugzilla — Bodhi syncs titles asynchronously, so a just-created
update reports `title: null`, which used to blind the per-bug
auto-vote and show `<no title>` prompts (this also benefits
`--give-karma` on very fresh updates). Authentication reuses the
bodhi CLI's OIDC session, like `--give-karma`.

sandogasa-bodhi gains the API this rides on: `NewUpdateFromTag` /
`NewUpdateResponse` models and `BodhiClient::new_update_from_tag`
(additive).

### dbranch: Debusine uploads (`--debusine`)

The `upload` stage of `rebuild` and `update` can now publish to a
[Debusine personal repository](https://wiki.debian.org/DebusineDebianNet#Repositories)
instead of a dput archive: `--debusine <name>` (the `r-<name>-*`
workspace owner on debusine.debian.net) uploads with
`dput -O debusine_workspace=r-<name>-<srcpkg>
-O debusine_workflow=publish-to-<suite>-<srcpkg> debusine.debian.net`,
where the suite is the target's base release — a trixie backport
publishes to `trixie` (the official pattern), `update` to `sid`.
Mutually exclusive with `--ppa`/`--upload-target`; rejected for Ubuntu
PPA targets and bulk runs (Debusine hosts Debian suites only). The
debusine-client dput profile and the `debusine setup` token are
pre-flighted before any expensive work.

### dbranch: Debian backports targets

`rebuild` now recognizes `debian/<codename>-backports` branches (e.g.
`debian/trixie-backports`) as a third target type alongside Ubuntu PPAs
and Debian proposed-updates: version `<debver>~bpo<N>+<M>` (the official
backports scheme), changelog distribution `<codename>-backports`, entry
generated with `gbp dch --bpo` and normalized as usual (which also drops
gbp's trailing period on the `Rebuild for …` line). The branch's
`gbp.conf` gets only `debian-branch` — gbp's default `debian/%(version)s`
tag is already right in the `debian/` namespace — preserving any existing
settings, and `salsa-ci.yml` gets `RELEASE: "<codename>-backports"` (an
officially supported salsa-ci release whose image enables the backports
apt repo) with no relaxations — without the pin salsa-ci builds against
sid. The local build stage scratch-builds in the base release's chroot
(`pbuilder-dist trixie`, not `trixie-backports`). Backports require a
Debian host (like proposed-updates) and upload to dput's default target.

Also fixed for all target types: when dbranch *creates* a packaging
file from scratch (e.g. `gbp.conf` on a branch whose Debian branch has
none), the changelog entry and per-file commit now say
`* Create <file> for <target>` instead of `* Adjust …`; when one file is
created and another edited in the same run, they are listed on separate
`Create`/`Adjust` lines.

### quick-xml requirement relaxed to a range

v0.15.3 required quick-xml `"0.41"` strictly (the RUSTSEC-2026-0194 /
-0195 fix), which Fedora can't satisfy yet (it ships 0.40.1). Since our
exposure is low — Koji XML-RPC from trusted TLS endpoints, parsed with
plain `Reader` (the worse advisory is `NsReader`-only) — the requirement
is now `">=0.40, <0.42"`: our `Cargo.lock` and `cargo install` users
still get 0.41.0 (the resolver picks the range maximum, and `cargo
audit` stays clean), while a Fedora build can resolve against its
packaged 0.40.1 until rust-quick-xml is updated there. The 0.40.1 floor
is verified (both consumers build and pass tests against it).

## v0.15.3

### Security: quick-xml 0.40 → 0.41

Addresses RUSTSEC-2026-0194 and RUSTSEC-2026-0195 (two high-severity
denial-of-service issues in quick-xml's XML parsing, fixed in 0.41.0).
Affects koji-diff and hs-relmon, which parse Koji XML-RPC responses. No
API changes were needed. Note: rust-quick-xml 0.41 is not packaged in
Fedora yet (tracked in TODO.md).

### HTTP request timeouts everywhere

Every HTTP client in the workspace now sets a 120-second request
timeout, so a hung connection fails loudly instead of blocking a run
forever (reqwest's default client has no timeout at all). Covers the
library crates (bugzilla, discourse, distgit, gitlab, jira, mailman,
meetbot, nvd, repology — bodhi, github, forgejo, and sourcehut already
had one) and the tools' own clients (ebranch's crates.io and Bodhi
session calls, fedora-cve-triage's GitHub lookups, hs-relmon's CBS
client, koji-diff's XML-RPC client). fasjson's Kerberos `curl`
shell-out gets `--max-time 120` for parity.

### --version works on every tool

`hs-relmon` and `hs-intake` were missing the standard clap header; all
14 tools now support `--version` and show the standard
name/version/description `--help` header.

### ebranch: base-distro guard for resolve / file-requests

EPEL packages must not replace base-distro (RHEL / CentOS Stream)
packages — but `resolve` treated "present in the base at a too-old
version" identically to "absent entirely", and `file-requests` happily
filed a branch request for python-setuptools on epel10 (rhbz#2482250,
closed CANTFIX: it's in RHEL 10). Now, for EPEL targets, `resolve`
probes the base distro behind the target (epel10 → c10s; epel9 → al9,
since fedrq's c9s layers epel9 + epel9-next and UBI is incomplete) and
such deps are **blocked**: the closure is pruned there and a "Blocked by
base distro" section presents the two real options — introduce an
alternate, non-conflicting package (opt in per package with
`--override PKG,...` or the interactive prompt; alternates need a new
package review, not a branch request), or lower the depending package's
requirement to the base version. A dep the base actually satisfies is
treated as satisfied (matters for `@epel`-only target repos).
`--base-branch` overrides the mapping on both `resolve` and the
branch-request subcommands. `file-request`/`file-requests` additionally
run a base-distro pre-flight of their own, skipping/refusing packages
present in the base even when a stale or pre-guard report still lists
them, and skipping report packages marked as overrides. Resolve reports
gain `blocked_by_base` and `overrides` fields (older reports still
load). New `sandogasa_fedrq::resolve_source_vr()`.

### ebranch: check-crate machine output coexists with the human report

`check-crate`'s machine modes (`--koji`, `--copr`, `--dot`) now print the
human-readable report — what needs building, and at which versions — to
**stderr**, leaving **stdout** to the machine output alone. So
`check-crate … --koji > build.sh` shows the report in your terminal while
`build.sh` stays clean and pipeable, and `… --koji | sh` still works.
`--json` is unchanged (it already carries the full report); `--toml`
writes its file and, on its own, still prints the human report to stdout.

### New crate: sandogasa-sourcehut (sr.ht GraphQL client)

`sandogasa-sourcehut` is a client for the Sourcehut (sr.ht) GraphQL API,
covering the activity `sandogasa-report` summarizes: patchsets submitted
(lists.sr.ht), ticket activity (todo.sr.ht), and commits authored
(git.sr.ht). sr.ht has no unified PR model — each service is a separate
GraphQL endpoint (`https://<service>.<host>/query`, Bearer PAT, cursor
pagination). The service schemas are vendored under `schema/` for
reference (refresh with `scripts/update-srht-schemas.sh`).

### sandogasa-report: Sourcehut support

`sandogasa-report` can now include Sourcehut activity for a domain
(`[domains.<d>.sourcehut] instance = "sr.ht"`, per-user login under
`[users.<key>.sourcehut]`, token via `sandogasa-report config` /
`SOURCEHUT_TOKEN[_<HOST>]`). The section reports patches sent/applied,
tickets opened/closed, and commits in your own repos (split into yours
vs third-party), with `--no-sourcehut` to skip it. Commit depth follows a
consistent policy across forges: a total + repo count in the summary,
per-repo counts at `--detailed`, and individual commits with subject at
`--detailed --detailed` (the latter where per-commit data exists —
Sourcehut; github/gitlab stay at per-repo counts). Because sr.ht exposes
only the account's *primary* email, commit ownership is matched against
that plus a per-profile `git_emails` list (or `["*"]` for all), prompted
for by `sandogasa-report config`. Ticket metrics come from the
authenticated user's event feed, so they populate only when reporting on
the token owner.

### dbranch: rebuild handles maintainer-clean packaging

Two fixes for rebuilding a package you don't maintain, where the Debian
branch is kept minimal:

- **Create `debian/gbp.conf` when the source branch has none.** A clean
  Debian branch often ships no `gbp.conf`, so `gbp dch` defaulted
  `debian-branch` to the Debian branch and refused to run on the rebuild
  branch ("not on branch …"). `rebuild` now creates a minimal `gbp.conf`
  (`debian-branch` = the rebuild branch, `debian-tag` = the branch's
  namespace format) on the rebuild branch — the Debian branch stays clean
  for upstreaming. Previously it only *adjusted* an existing one.
- **Handle the modern single-line `salsa-ci.yml`.** The current upstream
  template is just an `include:` of `recipes/debian.yml` with no
  `variables:` block; `rebuild` used to warn "unexpected format; left
  unchanged" and skip it. It now appends a fresh `variables:` block
  (`RELEASE` + backports relaxations) when none exists.

## v0.15.2

### fedora-cve-triage: curate false positives before closing bugs

The false-positive detectors (`interpreter-fps`, `js-fps`,
`cross-ecosystem`, `unshipped-tools`) no longer close every detected bug
behind a single bulk `[y/N]`. With `--close-bugs` on a TTY, each detected
false positive is now reviewed individually via the shared
`sandogasa-review` keep/explain/remove flow: **keep** closes it as
NOTABUG with the configured reason, **explain** closes it with the reason
plus a written justification recorded on the bug, and **remove** leaves
it open (the detector was wrong — possibly a real CVE). The reassign and
final go/no-go prompts come after the per-bug review.
Piped/non-interactively the behavior is unchanged (every detected FP is
closed with the reason).

### New crate: sandogasa-review (shared keep/explain/remove)

`sandogasa-review` is a small library with the interactive
keep/explain/remove resolution mechanism (`Resolution` +
`resolve_interactive`) for reviewer-curated findings. It was extracted
from `fedora-review-digest` so the same flow can be shared across tools.
`fedora-review-digest` now uses it (behavior unchanged).

### ebranch: check-update lets the reviewer curate findings before karma

When casting karma interactively (`check-update --give-karma`, on a TTY,
without `-y`), check-update now walks the blocking findings — installability
issues and reverse-dependency breakage (grouped by the changed Provide that
causes it) — and lets the reviewer **(k)eep** each (real, still counts),
**(e)xplain** it (real but acceptable, with a written justification), or
**(r)emove** it (a false positive). The decisions feed *both* the posted
comment (an "Issues addressed by the reviewer" section records the
explanations; removed findings are dropped) and the derived karma: removing
or explaining the only blocking finding lets a −1 rise to 0/+1, instead of
forcing a manual karma override with no rationale on record. Under `-y` or
non-interactively, every finding is kept (unchanged behavior).

### ebranch: fix check-update side-tag staleness false positives

The side-tag staleness check was guessing a build's binary names from
its *source* name with `bin.starts_with(source)`, which both missed
real binaries on a rename (`python-jiter` ships `python3-jiter`) and
matched `-debugsource`/`-debuginfo` packages — which live in a separate
debug repo, not the side tag's main repodata, so the lookup came back
empty and reported a fresh build as "stale". It now runs a single
batched `fedrq -F line:source,version,release` query, groups the
results by the authoritative `source` field, and judges a build fresh
if any of its binaries resolves to the expected version-release
(release included, so a `0.15.0-1` → `0.15.0-3` bump is detected).
Debug subpackages are excluded. New
`sandogasa_fedrq::pkgs_source_vr()`.

The interactive `koji regen-repo` offer also now drops *both* caches
(fedrq smartcache + libdnf5) before re-checking — previously it cleared
only the smartcache, so libdnf5 kept serving the pre-regen metadata and
the re-check re-warned despite a successful regen. New
`sandogasa_fedrq::clear_all_caches()`; `--refresh` uses it too.

### ebranch: check-update infers the branch from Fedora side tags

`check-update` now infers the branch for a Fedora side tag from its name
(the prefix before `-build-side-`: `f43-build-side-*` → `f43`), matching
the existing Bodhi-alias inference, so a bare Fedora side tag no longer
requires `-b`. EPEL is *not* auto-resolved: the `epelN` branch alone
can't resolve base-OS dependencies, so an EPEL input without `--branch`
— whether an `epel*-build-side-*` side tag **or a Bodhi EPEL update**
(whose release derives to `epelN`) — now fails with an actionable hint to
pass a RHEL-compatible base branch plus the EPEL repo (e.g. `-b al9 -r
@epel` for epel9, `-b c10s -r @epel` for epel10) rather than silently
producing misleading installability results. `--branch`/`--repo` remain
override-only; `--repo` defaults to the branch's stable base repos (the
correct comparison baseline).

### ebranch: check-update summarizes large updates

`check-update` now leads with counts instead of dumping every list, so a
330-package update stays readable. The package summary groups updated
packages by their `old → new` version transition (biggest groups first,
e.g. "75 packages: 6.7.0-1.fc43 → 6.7.1-1.fc43") and lists newly
introduced packages separately; an Analysis block gives Changed-Provides
/ installability / reverse-dep counts. The actionable findings (removed
Provides, installability issues, reverse deps that would break) are still
shown in full; the bulky non-actionable lists (every updated Provide, the
full package list, OK reverse deps) collapse to counts. `--detailed`
restores the complete lists, and long lists otherwise cap at 15 with a
"… and N more" footer. (`--json` is unchanged and carries the new
per-package `changes`.)

### ebranch: check-update fixes for large side-tag updates

Three fixes found checking a 330-build KDE megaupdate
(FEDORA-2026-2b36efabf2):

- **Stale side-tag false positives.** Side-tag NVRs now come from
  `koji list-tagged --latest`, so a side tag that accumulated superseded
  builds (e.g. 6.7.0 then 6.7.1 of a package) no longer leaks the old
  NVR. The staleness check also only flags repodata that's *older* than
  expected (via rpmvercmp) — a newer build than expected isn't stale.
- **Bogus installability issues from boolean deps.** Two fixes:
  capability extraction left the close-paren of an inner rich-dep group
  stuck to the name (`sound-theme-freedesktop)`), which never resolved;
  and the check now *evaluates* boolean/rich deps with their real
  semantics instead of requiring every referenced capability — `A if B`
  needs A only when B resolves, `A unless B` only when it doesn't, `or`
  needs any, `and`/`with` need all, `without` ignores the excluded term.
  So `((pulseaudio-module-gsettings and sound-theme-freedesktop) if
  pulseaudio)` is correctly satisfied. A flagged boolean dep also now
  reports *which* capabilities failed.
- **Performance.** Stable-repo capability resolution
  (`provides_of_provider`) is memoized per capability, so a shared dep
  like `libstdc++.so.6` resolves once per run instead of once per
  requiring package.

### ebranch: accurate check-update note when Provides can't be compared

`check-update` printed "no side tag available; cannot compare Provides"
whenever it fell back to reverse-deps-only — misleading for a Fedora /
EPEL 9 update, where the real reason is usually that the `@testing`
metadata fedrq reads doesn't show the update's NVR yet (transient mirror
propagation after a push, or a stale local cache). The note now names
the actual reason via a `skip_reason`: `@testing` not showing the NVR
(with the expected NVRs and a retry / `--refresh` hint), the EPEL 10
`@testing` limitation (suggesting a side tag), or a genuine no-source
case (with the Bodhi status). The reason is also exposed as
`skip_reason` in `--json`, and notes are word-wrapped to 76 columns.

`--refresh` now also clears the libdnf5 metadata cache
(`~/.cache/libdnf5`), not just fedrq's smartcache (`~/.cache/fedrq`).
The libdnf5 cache is what queries for the host's *own* Fedora release
reuse, so it could serve stale metadata for the native branch even
after a smartcache clear. (`sandogasa-fedrq` gains `libdnf5_cache_dir`
and `clear_libdnf5_cache`.)

### sandogasa-forgejo: new library crate

A Forgejo / Gitea REST API (`/api/v1`) client, scoped to what the
sandogasa tools need: the token owner's pull requests across every repo
they contribute to (`my_pull_requests`, via the global issue/pull search
with `created=true`), issue create/search (`create_issue` /
`search_issues`, for filing release-engineering tickets), and
`validate_token`. Works against any instance — codeberg.org, a Fedora
Forgejo, a self-hosted Gitea — by taking the instance root URL in full.
Mirrors `sandogasa-github` / `sandogasa-gitlab` in shape.

### sandogasa-report: Forgejo PR-merge accounting

Domains can now include a `[domains.<name>.forgejo]` block (`instance`,
optional `owner` filter) to report pull requests opened and merged — and
issues opened and closed — on a Forgejo / Gitea instance — Codeberg, a
Fedora Forgejo, etc. The query is token-scoped (`created=true`), so it
captures contributions to *any* repo, not just the user's own namespace. The opened list annotates each
PR's fate — `(merged)`, `(closed)`, or `(applied)` for a closed-unmerged
PR whose commit nonetheless landed on the target branch (a maintainer
cherry-picked/fast-forwarded it rather than clicking merge; detected by
checking whether the PR's head commit is contained in its base branch).
Per-instance usernames live
under `[users.<key>.forgejo]` and tokens under `[forgejo_tokens]`
(env overrides `FORGEJO_TOKEN_<HOST>` / `FORGEJO_TOKEN`); the `config`
subcommand prompts for them, and `--no-forgejo` skips the source.

Fixed: the `config` subcommand failed with a spurious "TOML parse error
… unexpected content" on any existing overlay. It parsed the file with
`str::parse::<toml::Value>()`, which in the toml 1.x crate parses a value
*expression* and misreads a leading `[table]` header as an array. It now
uses `toml::from_str` (document parsing), matching the report loader.

## v0.15.1

### fedora-review-digest: new tool

Condense a `fedora-review` run of an auto-generated spec into a short
rust-sig-style Bugzilla review comment, dropping the template noise that
isn't decision-relevant for a generated package. Reads a finished
`fedora-review -b` result directory (or a bug id resolved to `<id>-*`)
and emits the three `===`-separated blocks: an optional reviewer note, a
checklist with a per-item ✅/🫤/❌ verdict and the MUST issues that need
attention, and the post-import rust-sig task boilerplate.

Marks are inferred and then confirmed interactively (`+1/0/-1` per item,
evidence shown inline, `-y` to accept). It computes what `fedora-review`
doesn't decide for a generated spec — crates.io latest version, the
spec↔`Cargo.toml` license cross-check — and applies rust2rpm-aware
handling: suppress the benign "File listed twice" for crate-instdir
files that are also `%doc`/`%license`, note a
manually-added license (`included manually[, fix submitted to
upstream]`), distinguish skipped vs disabled tests, and fail the
builds-and-installs item when `fedora-review`'s install check did. For a
crate that ships a binary it verifies the statically-linked dependency
licenses: it reads the `LICENSE SUMMARY` `rust2rpm` writes to the build
log, checks each is folded into the binary subpackage's `License:`
(naming any that are missing), and prints the full breakdown for the
reviewer to inspect. Remaining `fedora-review` MUST issues are resolved
interactively — keep (blocks), explain (accepted with a justification,
kept on record), or remove (false positive) — and the verdict flips to
APPROVED once all are addressed with no `-1`. rust2rpm only for now;
pyp2spec and running fedora-review itself are planned. Needs
`fedora-review` (to produce the dir) and `curl` (crates.io check;
`--no-net` to skip).

`--post` writes the result back to the review bug (after confirmation):
on approval, the digest comment + `fedora-review+` + status POST; when
not approved, the digest comment + `fedora-review?` (unless already set).
In both cases it claims the bug, assigning it to the reviewer unless
it's already theirs. Uses a Bugzilla API key from `$BUGZILLA_API_KEY` and
email from `$BUGZILLA_EMAIL`, each falling back to
`~/.config/fedora-review-digest/config.toml` (a `config` subcommand sets
them up and verifies them).

### dbranch: pre-flight PPA uploads against Launchpad

Before a PPA upload (`--ppa`/`ppa:` target), dbranch now checks via the
Launchpad API (`curl … getPublishedSources`) whether the package is
already published in that PPA. If it isn't — or the PPA can't be
verified (a typo'd name 404s) — it asks to confirm before uploading
(default **no**), catching an accidental wrong-PPA upload. A genuine
first upload hits this once and is confirmed, like trusting a new SSH
host. The prompt fires only on an interactive run; `--yes` or a non-tty
warns and proceeds, and `--dry-run`/`--explain` just narrate the `curl`.
A missing `curl` skips the check rather than blocking the upload. Only
PPA targets are checked — the Debian-archive default and explicit
`--upload-target` hosts have no equivalent pre-check.

### Workspace: update URL
The repo was renamed from `fedora-cve-triage` to `sandogasa`, with
`fedora-cve-triage` now only one of the many tools and library crates.

But the URL in the root `Cargo.toml` was never updated, so while users
clicking through from `crates.io` will get redirected to the correct
repo, this is both slightly confusing and inefficient.

## v0.15.0

### dbranch: bulk no longer hides the checked-out PPA branch

A bulk `rebuild` excluded the merge source from the target set. Since
the Debian branch isn't a Ubuntu codename it was already excluded by the
codename filter, so that exclusion only ever bit when you were checked
out **on a PPA branch** — silently dropping it from the rebuild set (and
treating it as the merge source). Bulk now selects every live Ubuntu PPA
branch regardless of what's checked out. A bulk **merge** still needs the
Debian branch as its source, so it now refuses early with a remedy if
the source is a PPA branch (check out the Debian branch or pass
`--source`); non-merge bulk stages (e.g. `--stage upload`) run from any
branch.

### dbranch: `update` upload to the archive requires a Debian host

`dbranch update`'s upload stage `dput`s to the Debian archive
(unstable), which only works on a Debian host — Ubuntu's `dput` doesn't
understand that target. It now hard-fails early on a non-Debian host
when uploading to the **default** target. `import`/`build`/`lint` are
unaffected (they run fine on Ubuntu), an explicit `--upload-target`
(e.g. `mentors`) is exempt, and a `--dry-run` is exempt.

### dbranch: fix a missing blank line in changelog conflict resolution

When resolving a `debian/changelog` merge conflict, the incoming Debian
stanza and the local rebuild stanza could run together (footer line
immediately followed by the next header) when git drew the hunk
boundary without a trailing blank on the incoming side. The resolver
now normalizes the junction to exactly one blank line. Observed on a
proposed-update merge; the same shared code path can in principle hit
it on a PPA rebuild merge, though that hasn't been seen in practice.

### dbranch: rebuild auto-detects Debian proposed-updates

`dbranch rebuild debian/<codename>` now recognises a Debian stable
branch (codename via `debian-distro-info`, e.g. `trixie` → 13) and
switches from the Ubuntu PPA scheme to a proposed-update:

- **version** `<base>~deb<N>u<M>` (tilde, so it sorts *older* than the
  plain build and never shadows testing/unstable on upgrade), `M`
  incrementing from the changelog like the PPA `+N` counter
- **changelog distribution** the codename (`trixie`)
- the changelog command shown/run is `gbp dch --stable` (not `--bpo`);
  the entry is still normalized to the `~` form + `* Rebuild for
  <codename>`
- **salsa-ci.yml** gets `RELEASE: "<codename>"` and **none** of the
  backports relaxations (it's a real stable build)
- **upload** goes to `dput`'s default target (the Debian archive) — no
  `--ppa`/`--upload-target` needed (an explicit `--upload-target` is
  still honored); the PPA "upload needs a target" rule applies only to
  PPA branches

Needs `debian-distro-info` (from `distro-info`) — consulted only for
`debian/`-namespaced branches, so plain PPA rebuilds are unaffected.

A proposed-update run **requires a Debian host** and hard-fails early
otherwise (`gbp dch --stable` needs a newer gbp, and the stable build
chroot and `dput`-to-stable are Debian-only). A `--dry-run` is exempt
(it executes nothing).

### dbranch: `--urgency` to override the changelog urgency

`dbranch rebuild` and `dbranch update` now take `--urgency <level>`
(default `medium`), passed to `gbp dch` as `-U`. Use `--urgency high`
(or `critical`) for a security upload. The value is passed through to
`dch`, which validates it.

### dbranch: `update` subcommand — new-upstream update of the Debian branch

`dbranch update [<branch>]` updates the Debian branch
(`master`/`main`/`debian/unstable`, default the current branch) to a
new upstream: `gbp import-orig --uscan --pristine-tar --no-interactive`
then `gbp dch -c -R -D unstable`, then the shared
`build → lint → push → upload → tag` tail. Differences from `rebuild`:

- The changelog is **not** normalized — `gbp dch`'s entry stands (a
  genuine new-upstream version, no `~codename+N` suffix), so commits
  since the last release show up as bullets. The distribution is pinned
  to `unstable` (`-D`) so dch's release heuristic can't substitute the
  host's own (e.g. an Ubuntu devel codename), which would fail Debian CI.
- The build suite is decoupled from the changelog distribution: builds
  against **testing** by default, `--build-suite unstable` to switch.
- Upload defaults to dput's own target (the Debian archive) with no
  flag; `--upload-target mentors` for a vetted upload (no `--ppa`).
- Self-heals a partial run: if a previous `update` imported the upstream
  but failed before writing the changelog, `gbp import-orig` refuses to
  re-import — the import stage now treats that one refusal as success and
  continues to `gbp dch`, so a plain re-run recovers. Other failures
  still propagate.

Stages: `import` (head) + `build`/`lint`/`push`/`upload`/`tag`; default
`import`, `all` = `import,build,lint,push`. Needs `devscripts` (uscan)
and `pristine-tar` for the import. Internally the
`build → … → tag` pipeline is now shared between `rebuild` and
`update`.

### dbranch: synthesize the rebuild changelog body (list adjusted files)

The rebuild changelog entry no longer flattens to just `* Rebuild for
<codename>` *or* dumps `gbp dch`'s body (which, after merging the
Debian branch, lists the entire merged Debian delta). `normalize_top_
stanza` now discards gbp's body and synthesizes a clean one: `* Rebuild
for <codename>` plus, when dbranch adjusted packaging files this run, a
single `* Adjust <files> for <codename>` line (e.g. `* Adjust gbp.conf
and salsa-ci.yml for questing`). So the first rebuild of a branch
records the gbp.conf/salsa-ci.yml setup; later rebuilds (nothing to
adjust) just say `* Rebuild for <codename>`. Discarding gbp's body also
drops any stray `UNRELEASED`.

### dbranch: rebuild self-heals an unadjusted existing PPA branch

Rebuilding an existing PPA branch whose `debian/gbp.conf` still pointed
`debian-branch` at the Debian branch — e.g. one branched by hand from
`main`/`debian/unstable` without dbranch — failed: `gbp dch` refused
("you are not on branch '<x>'") and the codename was wrongly taken from
gbp.conf (`-D main`). Now the codename is derived from the branch name,
and the merge stage applies the gbp.conf (`debian-branch` /
`debian-tag`) and salsa-ci.yml adjustments before `gbp dch` for
**existing** branches too (idempotent — a no-op on already-adjusted
branches), so such a branch is fixed up automatically on first rebuild.

### dbranch: safer bulk run — codename selection, EOL check, confirmation

A no-argument `dbranch rebuild` (bulk mode) is now both safer and more
deliberate:

- **Selection by Ubuntu codename.** It picks only branches whose
  codename is a real Ubuntu release (via `ubuntu-distro-info --all`) —
  `noble`, `ubuntu/questing`, etc. — so it no longer sweeps up the
  Debian branch, `master`/`main`, Debian suites (`debian/trixie`,
  `bookworm-backports`), or gbp plumbing. (Replaces the old
  "every local branch except a few" heuristic.) Bulk mode now requires
  `ubuntu-distro-info` (from the `distro-info` package).
- **EOL releases skipped by default.** A codename no longer in
  `--supported` is end-of-life; those are skipped (with a note).
  `--include-eol` rebuilds them too — but only locally: it can't be
  combined with the `upload` stage, since Launchpad rejects uploads to
  an EOL series' PPA.
- **Newest release first.** Bulk branches are processed in release
  order, newest first (oldest last), using `ubuntu-distro-info`'s
  ordering. A failed stage still aborts the whole run (unchanged), so
  the newest releases are attempted first.
- **Confirmation before work.** The resolved branch set is printed and
  confirmed (`[Y/n]`, default yes) before anything runs. `--yes`/`-y`
  skips the prompt for scripted runs; `--dry-run` just prints the set;
  a non-terminal stdin without `--yes` is refused with a remedy rather
  than run unconfirmed.

`rebuild --help` gains a **Bulk** section for `--yes`/`--include-eol`.

### dbranch: `--explain` shows a diff of each hand-edit

Under `--explain`, after dbranch edits a file itself — resolving the
`debian/changelog` conflict, normalizing the rebuild entry, or the
gbp.conf / salsa-ci.yml tweaks — it now runs `git diff` on that file
and pauses, so you see exactly what changed before it's committed
(`git diff` being a real command you could run yourself). No effect
outside `--explain` or under `--dry-run` (nothing is edited there).

### dbranch: refresh the pbuilder chroot before building; group `--help`

The build stage now refreshes the codename's pbuilder base chroot
(`pbuilder-dist <codename> update`) before building when it exists but
is older than a day, so builds aren't against stale packages. Control
it with the mutually-exclusive `--refresh-chroot` (force, regardless of
age) and `--no-refresh-chroot` (skip; build against the chroot as-is);
the default auto-refreshes only when stale. A brand-new chroot is still
created (`… create`) as before.

`dbranch rebuild --help` now groups flags into **Stages**, **Upload**,
and **Output** sections for readability.

### dbranch: `fixup` subcommand for existing branches

`dbranch fixup [<branch>...]` applies the PPA-branch packaging
adjustments — gbp.conf's `debian-branch` / `debian-tag` and the
salsa-ci.yml preset, the same ones the `merge` stage makes when
creating a branch — to **existing** branches, to repair ones set up
before (or outside) dbranch (e.g. branches missing `debian-tag`, which
made `gbp tag` use the wrong namespace). It checks each branch out,
adjusts, and commits what changed; idempotent, and defaults to the
current branch. Errors up front if there's no `debian/` directory (run
from the wrong repo).

### dbranch: `--source` to override the merge source branch

`dbranch rebuild --source <branch>` sets the branch merged into each
target, instead of always using the checked-out branch — so dbranch
can run without first checking out the Debian branch (e.g. from another
branch or a detached HEAD). The source is validated up front (clear
error if the ref doesn't exist) and feeds the `target == source` guard
and bulk exclusions.

### dbranch: `tag` stage (gbp tag)

A new `tag` stage tags the release: it first runs `dh clean` (`gbp tag`
refuses a dirty tree, and `debuild -S` leaves a generated
`debian/files`), then `gbp tag` — which derives the version from
`debian/changelog` and gbp's `debian-tag` format. Runs after `upload`
in the pipeline and is **opt-in** (not part of `all`). Adds `dh`
(debhelper) to the per-stage tool check, and requires `gbp` whenever
`tag` is selected.

### dbranch: `upload` stage (dput)

A new `upload` stage `dput`s the built source `.changes` (from
`debuild -S`) to its archive. The target is given by `--ppa
<user/name>` (sugar that becomes a `ppa:<user/name>` dput target; a
leading `ppa:` is tolerated) or `--upload-target <host>` for any dput
host (e.g. `mentors`, `ftp-master`); the two are mutually exclusive,
and the stage errors up front if neither is given. It runs after
`push` in the pipeline (so CI can pass before publishing) and is
**opt-in** — `--stage all` stays `merge + build + lint + push` and does
not upload. Adds `dput` to the per-stage tool check.

### dbranch: per-job CI watch progress

While watching a pipeline (push stage / `watch-ci`), dbranch now also
polls the pipeline's jobs (`glab api projects/:id/pipelines/<id>/jobs`)
and prints each job as it finishes — `✓ <name> (<stage>)` on success,
`✗ … — <status>` on failure — instead of only the pipeline-level
state. Best-effort: a failed jobs query is ignored (the pipeline poll
still drives pass/fail).

### dbranch: `--quiet` mode

`dbranch rebuild --quiet` (`-q`) suppresses the shelled-out tools'
output (git, gbp, debuild, pbuilder-dist, lintian), leaving just
dbranch's own step narration. Each command's output is captured and
replayed only if it fails, so problems stay diagnosable. Mutually
exclusive with `--explain` (the opposite, step-through verbose mode).

### dbranch: adjust gbp.conf and salsa-ci.yml when creating a new PPA branch

Creating a brand-new PPA branch now performs the two one-time
packaging tweaks the workflow needs, each as its own signed commit:

- `debian/gbp.conf`: point `debian-branch` at the new branch itself
  (so its codename resolves correctly and gbp treats it as its own
  Debian branch), and set `debian-tag` to the `ubuntu/%(version)s`
  format so `gbp tag` tags under `ubuntu/` (matching the branch's
  namespace) rather than gbp's default `debian/`.
- `debian/salsa-ci.yml`: inject the PPA-rebuild `variables` preset —
  `RELEASE: "unstable"` (salsa-ci builds against Debian unstable) plus
  the backports-style relaxations `SALSA_CI_LINTIAN_SUPPRESS_TAGS`,
  `SALSA_CI_DISABLE_VERSION_BUMP`, `SALSA_CI_DISABLE_PIUPARTS` —
  preserving the file's existing entries and comments.

Both edits are idempotent and skipped when the file is absent or
already adjusted; they only run on the new-branch (create) path, not
when merging into an existing branch.

### dbranch: track remote-only target branches instead of recreating them

A target branch that exists on `origin` but was never checked out
locally was misclassified as new and recreated from the Debian branch
(`git checkout -b <branch> <debian-branch>`), discarding the real PPA
branch's history. dbranch now classifies a target as local,
remote-only, or new: a remote-only branch is checked out as a tracking
branch from `origin/<branch>` (`git checkout -b <branch>
origin/<branch>`) and then merged/built as usual; its codename is read
from `origin/<branch>`'s `debian/gbp.conf`. Only a branch that exists
nowhere is created from the Debian branch. (Bulk, no-argument runs
still only consider local branches.)

dbranch also skips a redundant `git checkout <branch>` when already on
the target branch (the build/lint/push path) — it would otherwise just
print `Already on '<branch>'` and add a pointless `--explain` pause on
a no-op.

### sandogasa-cli: unified tool-availability check (breaking)

One batch function now covers both existence and probe checks:
`require_tools(&[(exe, install_hint, Option<probe>)])`. Each tuple's
optional probe means *run* `<exe> <arg>` and require a zero exit
(e.g. `Some("--version")`, `Some("version")` for koji, `Some("--help")`
for pbuilder-dist); `None` checks only `$PATH` existence. It checks
every entry and returns one error listing all missing tools with
their install hints. `tool_exists(name)` remains as the bare PATH
check.

Removed `require_tool` and `require_tool_with_arg`. Migrate:
`require_tool(n, h)` → `require_tools(&[(n, h, Some("--version"))])`;
`require_tool_with_arg(n, arg, h)` → `require_tools(&[(n, h, Some(arg))])`.
Callers updated: ebranch (fedrq/koji/bodhi), poi-tracker (koji),
koji-diff (koji), and dbranch (git/gbp/debuild/lintian probe
`--version`, pbuilder-dist probes `--help`).

### dbranch: new tool

A helper for propagating a Debian package across its Ubuntu/PPA
branches. Run from the Debian branch (whatever is checked out —
`master`, `debian/unstable`, …), `dbranch rebuild <branch>...` brings
that branch into each named PPA branch. A target that doesn't exist
yet is created from the Debian branch; with no targets it does all
existing PPA branches (every local branch except the current one and
gbp's `upstream` / pristine-tar). The codename comes from an existing
branch's `debian/gbp.conf` (`debian-branch` basename) or the branch
name's basename.

Work runs in `rpmbuild`-style stages via `--stage` (default `merge`):

- `merge` — switch to / create the target, merge the Debian branch,
  resolve the `debian/changelog` conflict deterministically (incoming
  Debian entry above the existing rebuild entry — the
  `dpkg-mergechangelogs` result), then `gbp dch --bpo -R -D <codename>`
  and normalize the stanza to `<debver>~<codename>+<N>` /
  `* Rebuild for <codename>`. The Debian base version is detected even
  from a PPA branch (a `~<codename>+<N>` suffix is stripped).
- `build` — `debuild -S -sa -d` + `pbuilder-dist` (opt-in for now),
  creating the codename's pbuilder chroot first
  (`pbuilder-dist <codename> create`) when
  `~/pbuilder/<codename>-base.tgz` is absent.
- `lint` — `lintian -I` on the built `.deb`s in
  `~/pbuilder/<codename>_result/` (`-I` includes info-level tags;
  linting binaries directly avoids re-unpacking the source, which
  `debuild -S` already lints). lintian is quiet when clean, so its
  output is echoed and a tag-count summary printed.
- `push` — push the branch (`git push -u origin <branch>` the first
  time, to set the upstream the remote ref didn't have yet; a plain
  `git push` once it tracks `origin/<branch>`), then (unless
  `--nowait`) watch
  that commit's GitLab CI pipeline to completion via the `glab` CLI,
  which auto-detects the salsa host / project from the git remote.
  dbranch polls `glab ci list --sha <commit> -F json` — targeting the
  exact pushed commit, not the branch, so it never latches onto the
  *previous* commit's pipeline during the post-push window before the
  new one is created. (It deliberately avoids `glab ci status`, whose
  `--live` needs a TTY to wait and whose action menu otherwise blocks;
  glab is still run with stdin on `/dev/null` as a backstop.) It waits
  until the pipeline reaches a terminal state: `failed`/`canceled`
  propagates a non-zero exit; `success`/`skipped`/`manual` pass; if no
  pipeline appears within ~3 min it's treated as benign (nothing to
  watch). The instance's glab auth is verified first
  (`glab auth status --hostname <host>`, host derived from the
  `origin` remote) — glab keeps a token per host, so this fails early
  with the `glab auth login --hostname <host>` command rather than a
  downstream API error (glab's own output is captured and only shown
  on failure, since older glab misreports a working token as invalid).
  `--nowait` pushes without waiting; attach later with
  `dbranch watch-ci [<branch>]` (defaults to the current branch, and
  likewise watches the branch-tip commit's pipeline) — e.g. after a
  `--nowait` push or a dropped connection. Adds a `serde_json`
  dependency (to parse glab's pipeline JSON).
- `all` — all of the above.

A failing stage command propagates its **real exit code** (lintian
uses its default — non-zero on error-level tags; a failed push or CI
pipeline propagates `git`'s / `glab`'s code), so `dbranch` exits with
the same status rather than a generic `1`.

dbranch is also a learning tool. `--dry-run` prints every command
without running anything; `--explain` runs the workflow but narrates
each command and pauses for Enter before running it (a step-through,
Ctrl-C aborts) for following along or sanity-checking; the two
compose. Narration is color-coded via `anstream`/`anstyle`,
auto-disabled when piped or under `NO_COLOR`. Also adds `anstream`
and `anstyle` to the workspace.

### hs-relmon: skip archived / issues-disabled GitLab projects when filing

`check-manifest` and `check-latest --file-issue` now check a
project's status before filing and skip — with a one-line note rather
than a counted error — when the GitLab project is archived
(read-only) or has the Issues feature disabled. Both states return
`403 Forbidden` on issue creation; previously each such package was
reported as a failure (and `check-manifest` exited non-zero). Seen on
e.g. `socat` (archived) and `mesa` / `centos-release-hyperscale`
(issues disabled). On a status-lookup failure the tool assumes filing
is allowed so a transient error never silently suppresses an issue.
New `sandogasa_gitlab::ProjectStatus` and
`sandogasa_gitlab::Client::project_status`.

### hs-relmon: new `file-conflicts` subcommand

Finds files shipped by more than one source package across the
repositories enabled together on a Hyperscale host. Where
`dupe-subpkgs` catches two sources shipping the same binary RPM
*name* in one tag, this catches the sharper case: a **file** conflict
between differently-named RPMs in *different* repos — e.g. the
`kernel` source ships `/usr/bin/ynl` and the `pyynl` tree inside
`python3-kernel-tools` (kernel repo) while a standalone `python3-ynl`
(main repo) ships the same paths, so dnf hits a conflict that name-
and-tag matching never sees.

Scans per EL version over the enabled repo set (default `main` +
`kernel` on EL10/10s; `main` only on EL9/9s, which has no kernel
repo; override with `--repositories`), pulling each binary RPM's file
list from Koji via `listRPMFiles` batched through `system.multicall`
(a whole tag is a handful of HTTP requests, not one per RPM), then
flags any path owned by two or more distinct sources. Directories,
`%ghost` entries, and debug payloads under `/usr/lib/debug` /
`/usr/src/debug` are excluded. `--release` limits which Hyperscale
releases are scanned (CSV of `9`, `9s`, `10`, `10s`) and `--package`
reports only conflicts involving named source packages. Read-only,
`--json` output, exits non-zero on any conflict. New
`cbs::Client::list_rpm_files_multi`, `cbs::RpmFile`,
`cbs::RpmFileList`, and `cbs::TaggedBinary.rpm_id`.

### hs-relmon: new `dupe-subpkgs` subcommand

Finds binary RPMs shipped by more than one source package within a
single Hyperscale tag. Hyperscale overrides stock CentOS packages
and occasionally moves where a binary RPM is built from (e.g.
splitting `perf` out of `kernel-tools`); mid-move, two source
packages can ship the same binary in the same tag, leaving the
depsolver to pick one. The redundant source should be retired.

Scans each repository's `-release`/`-testing` tags (EL9/EL10 and the
Stream variants) via Koji `listTaggedRPMS` (latest build per source,
`inherit=false` so only Hyperscale-tagged content counts), maps
binary-RPM name → distinct sources, and flags any with two or more.
Detection is per-tag (a collision only matters when both providers
land in the same enabled repository); `-debuginfo`/`-debugsource`
RPMs are excluded. Read-only by default (no Koji auth), `--json`
output, `--repositories` to scan beyond `main`, `--release` to limit
to specific Hyperscale releases (CSV of `9`, `9s`, `10`, `10s`), and
`--package` to report only collisions involving named sources;
exits non-zero when any collision is found.

`--fix` adds an interactive resolution pass: for each cluster of
sources sharing a binary it recommends untagging the oldest build
(the likely stale leftover) and — crucially — lists the binaries
that only each candidate provides, so you can see what would vanish
from the tag. Untagging `kernel-tools` to resolve a `perf` collision
would also drop `cpupower`, `rtla`, `rv`, … so the choice stays with
a human (one prompt per cluster, default skip; requires CBS auth). In
`--json` mode or without a terminal the plan is printed and nothing
is untagged. New `cbs::Client::list_tagged_binaries`,
`cbs::TaggedBinary`.

### fedora-cve-triage: new `interpreter-fps` subcommand

Detects CVEs that live in a language interpreter/runtime but were
filed against an application merely written in that language — e.g.
CVE-2025-13836 (a DoS in CPython's `http.client`, NVD product
`python:python`, fixed in cpython) misfiled against `asahi-installer`,
a Python app that ships no interpreter. The fix arrives via the
`python3.x` update, so the application's bug is a false positive.

A bug is flagged only when every product the CVE marks affected is
the interpreter itself (so a CVE that also names a real product is
never swept up) and the component is not an interpreter package
(`python3`, `python3.NN`, `pypy`, …). Scans by `components` or by `assignees`
(sweep a maintainer's CVE bugs); closes detected FPs as NOTABUG +
tracker block with `--close-bugs`, mirroring `js-fps`. Python today;
the interpreter table extends to other runtimes. New
`sandogasa_nvd::CveResponse::affected_products`.

## v0.14.0

### poi-tracker: export drops unshipped packages from the hs-relmon manifest

`export` (hs-relmon manifest) now excludes packages marked
`unshipped` in the inventory — they're gone (no CBS builds), so
hs-relmon has nothing to track or prune for them. Existing
manifest entries for unshipped packages are removed
unconditionally (not gated by `--prune`, since `unshipped` is an
explicit marker), and new ones are never added; the count is
reported. The inventory keeps the tombstone, so a revived package
returns to the manifest on the next export. Fixes unshipped
packages (e.g. one whose builds `prune-archived` cleaned up)
lingering in the manifest as normal tracked entries. New
`MergeResult.unshipped_removed`.

### hs-relmon: check CBS auth before pruning

`prune-tags`, `prune-manifest`, and `prune-archived` now verify an
authenticated CBS koji session up front (via `koji moshimoshi`)
before any read-side planning, failing fast with an actionable
hint (`run centos-cert`) instead of erroring at the first
untag after a long scan. Dry runs skip the check (read-only). New
`sandogasa_koji::check_auth`.

### hs-relmon: new `prune-archived` subcommand

Cleans up CBS builds for packages whose upstream repo is archived
(manifest `archived = true`, from
`poi-tracker sync-gitlab --mark-unshipped`). For each archived
package it compares every build in its `-release`/`-testing` tags
against the stock distro version for that tag's channel — CentOS
Stream N for Stream tags (`hyperscaleNs`), AlmaLinux N for RHEL
tags (`hyperscaleN`) — and untags builds at or behind stock
(redundant now). Builds newer than stock, or with no stock entry,
are never untagged automatically (the archived repo may be their
only source): they are prompted per build, and `--yes` warns and
skips them. New `repology::{centos_stream_release, almalinux_release}`
and an `archived` field on hs-relmon's manifest `PackageEntry`.

### poi-tracker: export carries the archived-builds marker to hs-relmon

`poi-tracker export` (hs-relmon manifest) now writes `archived =
true` on packages whose inventory `archived_builds` marker is set,
so hs-relmon knows which archived-upstream packages have CBS
builds to prune. Reconciled bidirectionally on every export — a
reactivated package loses the flag. hs-relmon's manifest gains a
matching `archived` field on `PackageEntry`/`ResolvedPackage`
(its prune logic is still the pending follow-up).

### poi-tracker: `sync-gitlab --mark-unshipped` (CBS release check)

Cross-checks each GitLab-synced project against CBS (CentOS koji)
and records archival state. An archived GitLab repo with no
released CBS build is marked `unshipped` (a tombstone, skipped
like a retired package); an archived repo that *still* has
release builds is marked `archived_builds` — it still ships, so
it is not skipped, but its lingering builds are a cleanup
candidate and the command suggests running hs-relmon to prune
them. "Released" respects each SIG's lifecycle: Hyperscale ships
for both RHEL `N` and CentOS Stream `Ns` (`hyperscaleN-*-release`
or `hyperscaleNs-*-release`), Proposed Updates is Stream-only;
`--centos-release` sets the valid majors (default `9,10`).
Requires `koji` with the `cbs` profile.

New surface: `Package.archived_builds` + `has_archived_builds()`
in sandogasa-inventory (schema regenerated; merged field-aware),
`sandogasa_koji::{list_tags, list_tagged_package_names}`, and
`sandogasa_gitlab::list_archived_project_names`. The marker
apply logic is now field-generic (`prune_retired::apply_marker`),
shared by `unshipped` and `archived_builds`.

### Dependencies: reqwest 0.13, toml 1, toml_edit 0.25, quick-xml 0.40 (breaking)

Bumped the four deferred major dependency upgrades, all now in
Fedora/EPEL (reqwest 0.13.3, toml 1.1, toml_edit 0.25.12,
quick-xml 0.40.1; lockfile pinned to the Fedora-shipped point
releases).

Breaking (API): `reqwest::Error` appears in some public
signatures (e.g. `sandogasa-bodhi`'s query methods), so the
reqwest major bump changes that type's identity for library
consumers. No sandogasa API was intentionally changed.

TLS posture change: reqwest 0.13's default `rustls` feature pulls
in `aws-lc-rs`, which is not packaged in Fedora, so we build with
`rustls-no-provider` and keep the ring crypto provider (as
before, statically linked, build-dep only — no runtime RPM). The
provider is no longer compiled in as a default, so it is
registered at startup via the new `sandogasa_cli::init()` (called
from every tool's `main`) and defensively in the library client
builders. Trust roots now come from the system store
(`rustls-platform-verifier` → Fedora's `ca-certificates`) instead
of a copy of Mozilla's CA list baked into the binary, so CA
updates flow through `dnf` rather than a rebuild.

New `sandogasa-cli` surface: `init()` (standard per-`main`
startup hook — extend it for future cross-cutting setup) and
`install_crypto_provider()`.

quick-xml 0.40 migration: `BytesText::unescape` is gone and
`read_text` now yields a raw `BytesText`; the koji-diff and
hs-relmon XML-RPC parsers decode + unescape explicitly via a
local helper.

### koji-diff: fix the koji availability check

`koji-diff` checked for koji with `koji --version`, which exits 2
(koji uses the `version` subcommand), so it always aborted with
"is it installed correctly?" even on a working koji. Switched to
`require_tool_with_arg("koji", "version", ...)`, matching ebranch.

### sandogasa-distgit: group syncs no longer import non-rpms projects (breaking)

A Pagure group's project listing includes everything the group
can access — `container/`, `tests/`, and `modules/` projects and
forks, all reported under their bare names — and the group
endpoint honors neither `namespace=` nor `fork=false` (found
live: `container/python-classroom` imported into a
python-packagers-sig inventory as package "python-classroom",
`modules/askalono-cli` into a rust-sig inventory as
"askalono-cli", plus 117 forks). `group_projects` now records each project's
`fullname` and keeps only the `rpms/` namespace; skipped
projects are counted in the sync output. A re-sync with
`--prune` clears previously imported strays.

Breaking (API): `ProjectInfo` gained a public `fullname` field —
code constructing it with a struct literal must add it.

### poi-tracker: `prune-retired` flags nonexistent projects as invalid entries

A dist-git 404 means the inventory entry itself is wrong — a
non-RPM repo (module, container image, tests) imported under its
bare name by an older group sync, or a binary subpackage name
recorded instead of the source package. The first full scan
marked such an entry `unshipped`, which was misleading. 404s are
now reported as a separate "invalid entries — fix or remove"
list, never marked; a stale marker on such an entry is cleared
by the next run.

### poi-tracker: `sync-distgit --mark-unshipped`

Run the prune-retired check on the packages a sync adds (bounded
by `-j`/`--jobs`, default 8), so a fresh inventory starts with
its `unshipped` markers in place instead of needing a follow-up
`prune-retired` run. Best-effort: a failing check warns and the
sync still saves. New library surface:
`prune_retired::scan_packages` and `active_branches_from_bodhi`
extracted from the prune-retired flow.

### sandogasa-inventory: field-aware multi-inventory merge (breaking)

Merging inventories (poi-tracker's `-i`/`-I` with multiple files)
previously replaced a colliding package entry wholesale with the
later file's version, silently dropping fields the later file
didn't set — including `priority`, `retired_on`, and `unshipped`
markers. Merges are now field-aware: the later file's set fields
win, its unset fields keep the earlier values, `retired_on` is
unioned, and `unshipped` survives a bare later entry. Genuine
conflicts (both files set different values) are reported on
stderr, later file winning.

Breaking (API): `Inventory::merge` now returns `Vec<String>`
(conflict notes) instead of `()`; new `Package::merge_from`.
Callers that ignored the old unit return just discard the new
return value. `Package` also gained public fields this release
(`unshipped`, `archived_builds`) — code constructing one with a
struct literal must add them.

### poi-tracker: parallel `prune-retired` scan

The scan now checks packages concurrently, bounded by `-j`/`--jobs`
in-flight dist-git requests (default 8) — roughly an 8x speedup,
turning a 4500-package inventory from an hour into minutes. The
report order and the abort-on-persistent-failure behavior are
unchanged.

### sandogasa-bodhi: retry transient failures on the auth path

Token refresh, OIDC metadata/userinfo fetches, and Bodhi's
login/csrf requests now retry transport errors and 5xx responses
with backoff (new `auth::send_with_retry`). These requests run
right when `--give-karma` is about to post — after minutes of
analysis — so a single connection blip previously wasted the
whole run. The comment POST itself is deliberately not
auto-retried, since repeating it after an ambiguous failure could
double-post.

### ebranch: reviewer notes and provenance in posted reports

The comment `--give-karma` posts is always the full Markdown
check report, now with a provenance footer recording the ebranch
version and the command invocation that produced the analysis.
`--comment <TEXT>` adds reviewer notes as a section near the top
of the report (it no longer replaces the report); when the flag
is omitted you are prompted for notes interactively, and `--yes`
skips the prompt.

### ebranch: own-update detection no longer aborts the vote

A transient network failure looking up the session username (for
own-update karma skipping) aborted `--give-karma` after the whole
analysis had already run. The lookup now retries with backoff and,
if it still fails, warns and proceeds assuming a foreign update —
Bodhi enforces the own-update karma rule server-side regardless.

### ebranch: fix false "removed Provides" for compat packages (@testing path)

`check-update` compared provides per source package when querying
via `@testing`, so an update that bumps a crate and adds a compat
package shipping the old version (e.g. rust-const-oid 0.10 +
rust-const-oid0.9) falsely reported the old provides as removed —
and flagged reverse deps as broken. Provides are now unioned
across all packages in the update on both sides before comparing,
matching what the side-tag (koji) path already did.

### poi-tracker: new `prune-retired` subcommand

Finds inventory packages no longer carried on any active branch:
the dist-git project is gone (404), it has no branch on an active
release, or it is retired (`dead.package`) on every active branch
it has. The active branch set is queried from Bodhi's active
releases (plus rawhide), or set explicitly with `--branch`.

By default matches are marked with an `unshipped` reason in the
inventory rather than deleted — retired packages keep their ACLs,
so deleted entries would come straight back on the next
`sync-distgit`. The marker drives the rest of the tooling:
`triage-updates` and `semver-audit` skip unshipped packages,
`triage-retired` still processes them so remaining bugs get
closed, and `sync-distgit`/`sync-gitlab` `--prune` preserve them.
Markers are refreshed in both directions (a revived package is
unmarked). `--remove` deletes entries outright; `--dry-run`
previews; the usual `--pattern`/`--start-from`/`--end-with`
filters apply.

New library surface: `Package.unshipped` + `is_unshipped()` in
sandogasa-inventory (and the JSON schema), and
`DistGitClient::project_branches` in sandogasa-distgit
(`list_branches` that reports a missing project as `Ok(None)`
instead of an error).

### ebranch: `check-update --give-karma` casts karma with per-bug feedback

`check-update` can now vote on the Bodhi update it just checked:
`--give-karma` posts a comment with overall karma plus per-bug
feedback, like the web UI. The check result suggests the overall
karma — `+1` when no issues are found, `-1` when reverse deps
break or the updated packages have unsatisfied deps, `0` when
the analysis was incomplete — and the user is prompted with that
suggestion as the default. Update-request bugs
(`<pkg>-<version> is available`) are auto-voted `+1` when the
update delivers at least the requested version (by rpm version
comparison) and `-1` otherwise; other bugs are put to the user.
The full plan is shown for confirmation before posting; `--yes`
skips prompts (non-update bugs get `0`) and `--comment <TEXT>`
overrides the comment text, which defaults to the full Markdown
check report. On the user's own updates the overall karma is
skipped (Bodhi ignores submitter karma; the plan says so) while
per-bug feedback is still posted. Before any manual bug prompt
the update's notes are printed for context, and server-side
caveats from Bodhi are echoed after posting. Authentication reuses the bodhi CLI's cached OIDC
session (`~/.config/bodhi/client.json`), refreshing expired
tokens against the ID provider and writing them back. The
session is validated before the analysis runs; with no session,
an interactive bodhi CLI login is started up front.

New library surface: `sandogasa_bodhi::auth` (bodhi CLI session
reuse: `cli_session_token`, `load_tokens`/`save_tokens`,
`refresh_tokens`), `BodhiClient::with_token` (guarded by
`ensure_secure_url`), `BodhiClient::comment` with `bug_feedback`,
a `title` field on `BodhiBug`, and
`sandogasa_bugclass::bugzilla::extract_new_version` (moved from
poi-tracker's `semver_audit`, which now re-uses it).

### ebranch: `check-update` offers to regenerate stale side-tag repos

When the side-tag repodata lags koji (the V-R cross-check fails),
`check-update` no longer just warns: it now offers to run
`koji regen-repo --wait <side-tag>` on the user's behalf (default
yes), clears the fedrq metadata cache, and re-checks freshness
before running the provides comparison — so the analysis uses the
regenerated data instead of silently dropping reverse deps. If the
regen is declined, a second prompt asks whether to continue with
stale data (default no — the check aborts). Prompts only appear in
interactive runs; `--json` mode and non-terminal stdin keep the old
warn-and-continue behavior. New `sandogasa_koji::regen_repo()`
backs this.

## v0.13.0

### poi-tracker: `sync-distgit --fast` via the owner-alias dump

User syncs can now skip the prefix scan entirely: `--fast` fetches
Pagure's `extras/pagure_owner_alias.json` (one ~3 MB request) and
takes the user's directly-maintained packages from it — seconds
instead of minutes, with none of the group-derived download the
scan can't avoid. Trade-off (now also documented in
`crates/sandogasa-distgit/DEVELOPMENT.md` alongside the other
per-user query semantics): the dump records only direct
owner/admin/commit maintainers, so collaborator- and ticket-level
grants are missed — and `--prune --fast` would remove them.
`--fast` implies `--no-groups`; `--pattern`/`--exclude` apply
client-side. New `DistGitClient::user_packages_fast` backs it.

### poi-tracker: `sync-distgit` retries transport errors and resumes from partials

`DistGitClient`'s project queries now retry transient transport
failures (connection reset, timeout) with the same backoff already
used for 5xx responses, so a network blip no longer aborts a long
prefix scan. When a fetch still fails, `sync-distgit` saves the
failed pattern to `<output>.partial.state` next to the existing
`<output>.partial`; re-running the same command resumes from that
pattern (loading the partial as the base inventory), and a
completed run replaces `<output>` and removes both files.

### sandogasa-distgit: exclude forks from per-user project queries

`DistGitClient::user_projects` now passes `fork=false`: without
it, Pagure's listing includes the user's forks, and a fork is
reported under its bare package name with the user as `owner` —
indistinguishable from really owning `rpms/<pkg>`. Fork-only
packages therefore leaked into `sync-distgit --user` inventories
as direct/owner entries (even under `--no-groups`). Re-syncing an
affected inventory with `--prune` removes them.

### poi-tracker: remove deprecated `--auto-prefix --pattern` spelling (breaking CLI)

The pre-0.12.1 scan-resume spelling `sync-distgit --auto-prefix
--pattern <start>` — deprecated with a warning since v0.12.1 — is
now rejected: `--pattern` conflicts with `--auto-prefix`,
`--start-pattern`, and `--end-pattern`. Migration: use
`--start-pattern <prefix>` (optionally with `--end-pattern`).
This completes the removal scheduled in DEPRECATIONS.md.

### poi-tracker: consistent filters across walking commands (breaking CLI)

`semver-audit`, `triage-retired`, and `triage-updates` now share
the same package filters — `--pattern <glob>` (a bare name
matches exactly), `--start-from <name>`, and `--end-with <name>`
— and all three compose. `--batch [EMAIL]` is now available on
`triage-retired` too, replacing its per-retired-package-per-branch
Bugzilla searches with one email-scoped query matched locally;
with `--all-reporters` the batch query drops the reporter filter
as well.

Breaking CLI: `triage-retired --package <name>` is removed — use
`--pattern <name>` (an exact match when no glob characters are
used). `--pattern` no longer conflicts with the range flags.

### poi-tracker: record retirement in the inventory (breaking)

`triage-retired --mark` (single `-i` file only) now records its
findings in each package's new `retired_on` field — the list of
dist-git branches carrying a `dead.package` marker. The update is
bidirectional: a branch found live again is removed, so re-running
`triage-retired --mark` keeps the markers fresh. `semver-audit`
and `triage-updates` skip packages marked retired on rawhide
(their checks couldn't succeed anyway), which also saves their
per-package network traffic.

Breaking: `sandogasa_inventory::Package` gained the public field
`retired_on: Option<Vec<String>>` (and an `is_retired_on()`
helper); code constructing `Package` via a struct literal must add
`retired_on: None`. Inventories without the field parse unchanged.

### poi-tracker: `--batch` mode for `semver-audit` and `triage-updates`

Both subcommands previously issued one Bugzilla search per
inventory package, which dominates the runtime on a large
inventory. The new `--batch [EMAIL]` flag replaces them with a
single query for every open release-monitoring bug assigned to or
CC'ing EMAIL (default: the email configured via `poi-tracker
config`), matched against the inventory locally. Caveat: bugs
where that email is neither assignee nor CC'd are not seen, so
batch mode fits inventories of packages you (co-)maintain or
watch.

### Security: `--` separators before external-tool positional arguments

Shell-outs to `fedrq`, `koji`, `bodhi`, and `curl` now pass `--`
before positional arguments (package, tag, NVR, update alias,
URL), so a value beginning with `-` can never be parsed as a
flag by the external tool. Each tool's handling of `--` was
verified against the real CLI before the change; two call sites
in fedora-cve-triage also had their options reordered to come
before the positionals. `kinit` is intentionally unchanged
(verification was inconclusive, and its principal comes from
local user config). Defense-in-depth — Fedora package names
can't start with `-`.

### poi-tracker: `triage-updates` closes already-addressed bugs via Bodhi

`triage-updates` now checks every open release-monitoring bug
against Bodhi (and, for builds that predate the active releases,
against the branch's dist-git spec): when builds with the
advertised version or newer already exist, the latest addressing
build per release is written to the bug's Fixed In Version field,
and the bug is closed as `ERRATA` when stable in every active
release the package has a branch for, moved to `MODIFIED` while
any addressing update is still in testing, or — when only some
releases carry the fix (commonly just rawhide) — offered for
closing interactively. New flags: `--close-stale` closes the
partial cases without asking, `--skip-stale` disables the check
(restoring the previous priority-only behavior and cost), and
`--pattern <glob>` scopes the run to matching packages.
`semver-audit` now points at `triage-updates` from its "up to
date (stale bug)" group, mirroring its `triage-retired` hint.

The check short-circuits on rawhide: Fedora updates land in
rawhide first (a stable release may never carry a newer version
than rawhide), so a bug whose version isn't in rawhide — neither
in Bodhi nor committed to the dist-git spec — skips the
stable-release queries entirely. EPEL branches update
independently of each other, so EPEL bugs are always checked in
full.

### poi-tracker: new `semver-audit` subcommand

`semver-audit` classifies the pending upstream update for each
maintained package by semver impact, so a maintainer can see which
updates are safe to push. For every package (optionally filtered
by `--pattern <glob>`, e.g. `rust-*`) it reads the open
`upstream-release-monitoring@` "X is available" bug for the new
version, compares it against the rawhide dist-git spec's current
version, and reports **non-breaking** / **breaking** / **up to
date (stale bug)** / **retired (update request invalid)** /
**needs review**, grouped (or as `--json`). `--non-breaking` shows
only the safe updates. ("Up to date" means the packaged version
already matches the available version — the bug is stale.)

Classification follows Cargo's compatibility rule: a change at or
before the leftmost non-zero version component is breaking (so
`1.4 → 1.5` is safe but `0.4 → 0.5` is not). Semver build
metadata (a `+suffix`, e.g. `1.7.0+v1.7.0`) is ignored for the
comparison, per the semver spec. Non-numeric versions
(pre-releases, dates, snapshots) are reported as needs-review
rather than guessed. A package retired on rawhide (a `dead.package`
marker, the signal `triage-retired` keys on) is reported as
retired, consistent with that flow.

### sandogasa-distgit: validate identifiers before URL interpolation

`DistGitClient` now rejects package, branch, user, and group
names that aren't bare dist-git tokens (`[A-Za-z0-9._+-]`, and not
`.`/`..`) before building a request URL, returning an error
instead. This stops a value containing a path separator or a
parent-directory segment — which URL normalization could redirect
to a different resource — from reaching the wire. It matters
because some of these names arrive from API responses (e.g. a
Bugzilla component fed to `fix-version`), not just local config.
Valid Fedora names are unaffected; this is defense-in-depth.

### Security: refuse to send credentials over plaintext HTTP (breaking)

API clients now fail closed when a token would be sent to a
plaintext `http://` URL on a non-loopback host, so a misconfigured
base URL can no longer leak a Bugzilla API key or GitLab/GitHub
token in cleartext. Loopback hosts (`localhost`, `127.0.0.0/8`,
`::1`) stay allowed for mock servers and local development, and the
`SANDOGASA_ALLOW_INSECURE_URL=1` environment variable overrides the
guard for testing or a trusted internal proxy (see
`crates/sandogasa-cli/DEVELOPMENT.md`).

The shared check is the new
`sandogasa_cli::ensure_secure_url` (plus the
`sandogasa_cli::ALLOW_INSECURE_URL_ENV` constant), wired into the
Bugzilla, GitLab, and GitHub client constructors.

Breaking: `sandogasa_bugzilla::BzClient::with_api_key` now returns
`Result<Self, Box<dyn std::error::Error>>` instead of `Self` (it
can now reject an insecure URL). Migration: append `?` (or handle
the `Result`) at call sites — e.g.
`BzClient::new(url).with_api_key(key)?`. The GitLab/GitHub
`new()`/`validate_token()` signatures are unchanged (already
`Result`); they just gained the check. Jira and Discourse clients
are not yet guarded.

### Errata: v0.12.1 `sync-distgit --user` rationale

The v0.12.1 note for `poi-tracker sync-distgit` said "there is no
cheaper API that covers all ACL types." That is accurate but
misleads — it reads as "Pagure has no per-user endpoint." It does:
`/api/0/user/<name>` exists, but it returns HTTP 500 for prolific
users (it can't build the full response), and it wouldn't cover
non-owner ACLs (commit/collaborator/ticket) even if it worked. The
real situation is that *every* user-scoped path fails at Fedora
scale — `/api/0/user/<name>` 500s and `/api/0/projects?username=`
504s — which is why prefix-scanning is the default for `--user`.
Group syncs use `/api/0/group/<name>?projects=true`, a bounded,
indexed lookup, so they need no workaround.

### fedora-cve-triage: URL-encode Bugzilla query values

`build_multi_query` now percent-encodes product/component/status/
assignee values (matching `triage-retired`'s query builder),
instead of interpolating them raw. A value containing a space,
`&`, or `=` previously malformed the query string or injected an
extra parameter. These values come from trusted config, so this
is hardening rather than a fix for an exploitable issue.

### fedora-cve-triage: new `fix-version` subcommand

`fix-version` corrects CVE bugs filed against a dist-git branch
the package never shipped on (e.g. an `[epel-all]` bug landing on
`epel10` for a package that only has `epel8`/`epel9`). It
reassigns each such bug to the package's latest still-standing
branch in the same product family and marks it blocking the
configured tracker; if every branch in that family is retired,
the bug is reassigned to the latest one and closed as `CANTFIX`.
Bugs already filed against a real branch are left untouched.
Defaults to a preview; `--apply` writes the changes (with a
confirm prompt, and an offer to reassign the bugs to your
configured Bugzilla email), and `--component` narrows the run.
Branch existence and retirement come from dist-git (Pagure) —
no Koji or git-history lookups.

### sandogasa-distgit: `list_branches`

New `DistGitClient::list_branches` returns a package's dist-git
branch names via the Pagure `git/branches` API.

### poi-tracker: `triage-retired --branch` accepts multiple branches

`--branch` is now repeatable (and comma-separated), so one run
can check retirement across several dist-git branches (e.g.
`--branch epel8,epel9`). Each branch scopes its own Bugzilla
search and closure comment; a package retired on some branches
but live on others only has its bugs closed for the dead
branches. The default is still `rawhide`. Per-bug output and the
final tally now name the branch each closure is for.

### poi-tracker: `triage-retired --all-reporters`

New `--all-reporters` flag drops the release-monitoring reporter
filter so `triage-retired` closes **every** open bug on a retired
branch (CVEs, FTBFS, and other human-filed bugs included), not
just Anitya / the-new-hotness new-version bugs. The default
remains release-monitoring-only, which is safe to run routinely
across a whole inventory.

### sandogasa-report: group reports by domain in `--domain` order (breaking JSON)

A multi-domain report is now organized by domain rather than by
service. Each domain is a top-level `## <domain>` section,
emitted in the order the domains were passed on the command
line, with its Bodhi/Koji/GitLab/GitHub activity nested beneath
it as `###` subsections. Previously the report was grouped by
service (a fixed Bugzilla → Bodhi → Koji → GitLab → GitHub
sequence), so the first `--domain` had no bearing on output
order. Bugzilla remains a single aggregated section, now placed
immediately after the last domain that references it.

Breaking JSON: the top-level `koji`, `gitlab`, and `github`
objects (previously maps keyed by domain name) and the
top-level `bodhi` object are removed. They are replaced by a
`domains` array, each element `{ "name", bodhi?, koji?,
gitlab?, github? }`, in CLI order. `bugzilla` remains a
top-level sibling. Consumers that read e.g. `.koji["hyperscale"]`
must now find the matching entry in `.domains` and read
`.koji`.

Markdown also re-nests headings: services moved from `##` to
`###`, and their internal subsections from `###` to `####`.
`indexmap` is not used; domain order is carried by the
`domains` array/`Vec` directly.

### Deprecation tracking: DEPRECATIONS.md

New root-level `DEPRECATIONS.md` records deprecated
functionality with its deprecation release, planned removal
release, and replacement. The first entry pins the removal of
poi-tracker's deprecated `sync-distgit --auto-prefix --pattern
<start>` spelling to v0.13.0, and its runtime warning now
names that version.

## v0.12.1

### fedora-cve-triage: `--component` filter for `bodhi-check`

`bodhi-check` accepts a new `-c` / `--component` flag (CSV or
repeated) that limits the run to the given components,
overriding the config file's `components` list. This allows
scoping an assignee-based config to specific packages for one
run without editing the config.

### fedora-cve-triage: `bodhi-check` resolves rawhide bugs

Bugs filed against version `rawhide` previously produced
"cannot determine release" warnings and were skipped, because
the release name cannot be derived from the version field
alone. `bodhi-check` now resolves them to whichever release
Bodhi currently calls rawhide (`branch == "rawhide"`, e.g.
`F45`), where rawhide builds receive automatic updates. The
fedrq provides fallback for NVD product matching queries the
`rawhide` branch for that release (its lowercased name has no
repos until Fedora branches). An explicit `[fedora-NN]`
summary tag still takes precedence.

### poi-tracker: prefix mode is the default for `sync-distgit --user`

Pagure's unfiltered per-user projects query scans every
project's ACLs server-side and routinely exceeds the gateway
timeout (HTTP 504) — and there is no cheaper API that covers
all ACL types — so `sync-distgit --user` without `--pattern`
now scans one name prefix at a time (`a*`–`z*`, `0*`–`9*`).
Group syncs still issue a single query by default.

- `--pattern` now always means a single patterned query. The
  new `--start-pattern <prefix>` (with the existing
  `--end-pattern`, which no longer requires `--auto-prefix`)
  bounds the prefix scan and implies prefix mode, also for
  group syncs.
- The old scan-resume spelling `--auto-prefix --pattern
  <start>` is deprecated but still accepted with a warning,
  to be removed in the next breaking release. Use
  `--start-pattern <prefix>` instead.
- `--auto-prefix` remains as the explicit opt-in to a full
  scan (the way a group sync enables prefix mode).
- The new `--no-auto-prefix` flag forces a single unfiltered
  query (the old `--user` default); if both `--auto-prefix`
  and `--no-auto-prefix` are given, the last one wins.
- When an unfiltered query does hit a 504, the error output
  now suggests retrying with `--auto-prefix`.

### sandogasa-distgit: non-blocking retry backoff

`get_with_retry` now uses `tokio::time::sleep` instead of
`std::thread::sleep`, so the retry backoff yields to the tokio
runtime instead of blocking the worker thread. `tokio` is now a
regular dependency of the crate (it was already pulled in
transitively via `reqwest`).

## v0.12.0

### sandogasa-fasjson: `timezone` field on `FasUser` (breaking)

`sandogasa_fasjson::models::FasUser` gained a public field
`timezone: Option<String>` so callers can read the user's FAS
profile timezone (used by sandogasa-hattrack's `last-seen`,
below). Adding a `pub` field to a struct with no
`#[non_exhaustive]` marker is a semver-breaking change for
code that constructs `FasUser` via a struct literal; consumers
that only read fields are unaffected.

Migration: if you construct `FasUser` directly (rare; mostly
seen in tests), add `timezone: None` to the field list.

### sandogasa-hattrack: narrow `last-seen`'s service set

Two new flags on `last-seen` let callers skip the expensive
HyperKitty mailing-list scan (or any other service) when a
target user clearly doesn't use it:

- `--skip <list>` — comma-separated services to skip.
- `--only <list>` — comma-separated services to ONLY query
  (mutually exclusive with `--skip`).

Values: `bodhi`, `bugzilla`, `discourse`, `distgit`,
`mailman`. Skipped services don't appear in the human output
or JSON `services` array at all (rather than appearing with
"no activity").

### sandogasa-hattrack: public-holiday signal in `discourse` and `last-seen`

The `discourse` and `last-seen` subcommands now flag any
nationwide public holiday falling on the user's local date,
rendered as a `Holiday:` line under `Country:` (and as a
`holidays` array on each `LocalTimeEntry` / `LocalTimeReport`
in JSON output). Data comes from the Nager.Date public API
(<https://date.nager.at>) and is cached per country-per-year
at `$XDG_CACHE_HOME/sandogasa-hattrack/holidays/{CC}-{YEAR}.
json` (typically `~/.cache/...`), so repeat lookups never go
to the network. Only nationwide holidays (`global: true`) are
surfaced — we only know the country, not the subdivision.

When FAS and Discourse advertise different timezones, each row
gets its own holiday check, so a holiday in either location
shows up next to that location's `Local time:` line.

New global flags:
- `--no-holidays` skips the lookup entirely (useful offline).
- `--refresh-holidays` force-refetches the year's data even
  when a cached copy is present.
- `--now <YYYY-MM-DD | RFC 3339>` overrides "now" for the
  local-time / holiday computation. Intended for testing and
  demos — relative timestamps on other services are
  unaffected.

### sandogasa-hattrack: surface local time in `last-seen`

`last-seen` now prints the same `Local time:` / `Country:`
block already rendered by `discourse`, with the same colour
treatment (`--color`, `--working-hours`). Both FAS (via
FASJSON's previously-unused `timezone` field) and Discourse
are queried independently; when both advertise a timezone and
they agree, the block is rendered once, and when they
disagree (e.g. a traveller who updated Discourse but not FAS),
both are rendered side-by-side with a `[FAS]` / `[Discourse]`
suffix so the divergence is visible. JSON output gains a
`local_times` array on the top-level summary, one entry per
distinct timezone with its source(s) attached. The FAS-side
timezone read uses the new `FasUser.timezone` field (see the
`sandogasa-fasjson` entry above).

### sandogasa-hattrack: colour the local-time / weekday output

Adds ANSI styling to the `discourse` subcommand's `Local time:`
line: the weekday tag is green for a weekday or yellow for a
weekend, and the timestamp itself is dimmed when the local hour
sits outside working hours. JSON output is unaffected.

New global flags: `--color <auto|always|never>` (default
`auto`, follows the grep/ls convention — TTY + `NO_COLOR`
honoured) and `--working-hours <START-END>` (default `9-18`,
24-hour clock, start inclusive / end exclusive).

### sandogasa-hattrack: local time and weekend signal in `discourse`

The `discourse` subcommand now derives the user's local time
from their Discourse-set IANA timezone, names the country (via
tzdb's `zone1970.tab`), and flags whether it's currently the
weekend there. Weekends default to Sat+Sun with overrides for
the MENA Fri+Sat block, Iran (Fri), Nepal (Sat), and a few
others. JSON output gains a `local_time` object alongside the
existing `timezone`/`location` fields.

A bundled copy of `zone1970.tab` ships with the crate so the
lookup works on systems without tzdata installed. By default
the system file at `/usr/share/zoneinfo/zone1970.tab` wins as
long as it's at least as new as the bundled copy; otherwise the
bundled one is used and a one-line `info:` is logged. The new
global flag `--tz-source <auto|system|bundled>` forces a
choice.

### poi-tracker: `triage-retired` subcommand

Close open release-monitoring bugs for any inventoried package
that's retired on a dist-git branch. For each package the
command checks Pagure for a `dead.package` marker on
`--branch` (default `rawhide`); when present, every open bug
that `triage-updates` would touch is closed as
`CLOSED/CANTFIX` with a short comment naming the package and
branch.

The branch also scopes the Bugzilla search — `--branch
rawhide` closes `Fedora`/rawhide bugs, `--branch epel10`
closes `Fedora EPEL`/epel10 bugs — so EPEL retirements clear
the right tracking bug. `--package <name>` scopes the run to a
single package (handy for testing); `--start-from <name>` and
`--end-with <name>` bound an inclusive sub-range of the
inventory (e.g. `--start-from rust-nu-cli --end-with
rust-nu-utils` to walk every `rust-nu-*` package).
Network reads (dist-git probes, Bugzilla searches) retry up to
3 times with exponential backoff so a transient connection
blip doesn't abort the whole inventory. Findings print
per-package as each retirement is confirmed (rather than
batched at the end), followed by a one-line-per-package tally
listing the `rhbz#<id>`s about to be closed. Interactive runs
offer to claim ownership (set `assigned_to` to the configured
Bugzilla email) before applying — `--claim` skips that prompt
and is also the only way to claim under `-y`. `poi-tracker
config` now prompts for an optional Bugzilla email used for
claiming. `--dry-run` previews, `--yes` skips the confirmation
prompt.

`sandogasa-distgit` gained `DistGitClient::is_retired(package,
branch)`, a presence probe that returns `true` when the
`dead.package` marker exists on that branch and `false` on
404.

## v0.11.4

### ebranch: branch-request filing and escalation

Ports the EPEL branch-request workflow from the old Python
ebranch (issue #9). Three new Bugzilla-backed subcommands:

- `file-request <pkg> <branch>` — file one "Please branch and
  build" bug against `Fedora EPEL`/`<branch>`, falling back to
  `Fedora`/`rawhide` when the component isn't in EPEL. `--fas`
  and `--sig` add a co-maintainer offer; `--blocked`/
  `--dependson` set links (default: block the
  `EPELPackagersSIG` tracker); `--toml` records the bug ID in a
  check-crate report.
- `file-requests <report.toml> <branch>` — file requests for
  every package in a `resolve --report` closure and link them
  along the dependency graph (a package's request `depends_on`
  its dependencies' requests). IDs are written back under
  `[branch_requests]`.
- `escalate <report.toml> <branch>` — add a `needinfo?` ping to
  requests that have been NEW for ≥7 days and not yet pinged,
  marking them so they aren't pinged twice.

`resolve` gained `--report <file>`, which writes the closure
(package list + dependency edges) as a TOML the branch-request
commands consume. API key resolves from `--api-key` →
`BUGZILLA_API_KEY` → `ebranch config`. All three support
`--dry-run`.

`sandogasa-bugzilla` gained `BzClient::create` (POST
`/rest/bug`) returning a `CreateBugResponse` that surfaces
Bugzilla-level rejections (e.g. an invalid component) without
erroring, so callers can fall back to another product.

### hs-relmon: prune-tags untags testing builds not newer than release

`prune-tags` / `prune-manifest` previously untagged a
`-testing` build only when the exact same NVR was present in
the sibling `-release` tag. It now untags any testing build
whose version is *not newer* than the latest release build —
covering older leftovers in testing, not just the promoted
build. Strictly-newer testing builds are still subject only to
the keep-N retention rule.

### hs-relmon: `review` subcommand

Interactively review builds in Hyperscale `-testing` tags,
modeled on `fedora-easy-karma`. For each build it shows the
build metadata and the currently-released NVR for comparison,
then prompts:

- `+1` promotes — tags into the sibling `-release` tag and
  untags from `-testing`.
- `-1` rejects — untags from `-testing`.
- `0` / `s` / Enter skips; `q` / Ctrl-D stops.

Changelog display is scoped to what changed: for a build whose
package is already in release, only the changelog entries newer
than the released build are shown; for a brand-new package
(nothing in release yet) the changelog is capped at
`--changelog-lines` (default 20). If a testing build is not
newer than the released build (same version already released,
or a downgrade), review prints a warning rather than acting —
pruning the stale testing tag is `prune-tags`' job.

`hs-relmon review` with no argument walks every build in
testing; a package name reviews its latest build per testing
tag; an NVR reviews that specific build. `--repositories`
selects which repos to scan (default `main`); `--skip`
(repeatable or CSV) excludes packages with their own release
pipeline (e.g. systemd) and wins over an explicit target;
`--dry-run` lists the builds and exits.

`sandogasa-koji` gained `tag_build` (sibling of `untag_build`)
and `build_info_with_changelog`. `tag_build` passes `--wait`
explicitly — koji defaults to `--nowait` off a TTY, which would
let promote untag a build from testing before the release tag
landed, briefly leaving it in neither tag.

## v0.11.3

### ebranch: check-update no longer trusts a stale @testing snapshot

`check-update` previously fell back to `@testing` for "new"
provides as soon as that repo returned *any* subpackage for the
source — even when the Bodhi update was still `pending` and
`@testing` actually carried the previous V-R. The diff against
stable was then empty, hiding removed-subpackage cases like a
default-feature rename (e.g. `rust-libmimalloc-sys` flipping
its default from v2 to v3, where `+v3-devel` is replaced by
`+v2-devel`).

Two gates now guard the `@testing` path:

- For Bodhi-alias input, the update's status must be
  `testing`. Anything else (typically `pending`) skips
  `@testing` and uses the build side tag instead.
- `@testing` must report at least one subpackage whose
  `(version, release)` matches one of the input NVRs.

When either gate fails, `check-update` falls through to the
side-tag comparison as before, so reports for pending updates
correctly surface removed provides.

`sandogasa-fedrq` gained `Fedrq::subpkgs_nvrs(srpm)` returning
`Vec<(name, version, release)>`, used by the new gate.

### ebranch: check-update flags stale side-tag repodata

When the side-tag comparison path runs, `check-update` now
cross-checks each koji NVR against the V-R that the side tag's
repodata actually serves. A mismatch means
`compute_changed_provides_via_koji` would diff stable's provides
against an *old* V-R inherited from the parent tag, silently
dropping affected reverse deps from the report. Concrete case:
FEDORA-2026-7db4114930 listed `rust-mimalloc-0.1.50-1.fc44`,
but the side-tag repodata still returned `0.1.48-2.fc44`, so
`crate(mimalloc) = 0.1.48` never landed in `changed_provides`
and `rust-nu` (a real reverse dep) was missed.

The previous `check_side_tag_staleness` only verified that
*some* provides existed for the binary RPM names — it didn't
notice when those provides came from the previous V-R.

New report field `stale_side_tag: Vec<StaleSideTag>`
(`{ package, expected_nvr, actual_vr? }`) surfaces each
mismatch in both the JSON and human output. When non-empty the
report prints a prominent banner asking the user to run
`koji regen-repo` on the side tag and rerun with `--refresh`
(the latter clears fedrq's smartcache, which would otherwise
keep serving the old metadata).

`sandogasa-fedrq` gained `Fedrq::pkg_nvrs(name)` returning
`Vec<(name, version, release)>` for the per-binary lookup.

### hs-relmon: prune-tags untags promoted builds from -testing

`prune-tags` (and `prune-manifest`) now queue any build that
appears in *both* a `-testing` tag and the sibling `-release`
tag for untagging from `-testing`, in addition to the existing
keep-N-newest retention rule. Once a build is promoted to
release, leaving its `-testing` copy in place only adds noise
to `list-tagged` output. Sibling matching is on the literal
tag-name prefix, so `main-testing` pairs with `main-release`
and `facebook-testing` pairs with `facebook-release` — there's
no cross-repository attribution.

## v0.11.2

### sandogasa-report: history-based Koji activity reporting

Koji CBS reporting now walks `koji list-history` events
across the reporting window instead of diffing two snapshots
at the window's boundaries. Snapshot-diff missed any package
that was tagged and untagged entirely within the window;
history-walking captures every "tagged into" event so that
activity surfaces even when the net effect is invisible at
the start/end.

`sandogasa-koji` gained `tag_history(tag, profile, after,
before)` returning `Vec<TagAddEvent>` plus a public
`parse_tag_history` helper for the line-by-line parser.

No JSON shape changes; same `KojiReport` / `PackageEntry` /
`ChangeKind` surface.

### sandogasa-inventory: `Priority` enum + per-package and per-workload fields

New `Priority` enum (`unspecified` / `low` / `medium` / `high`
/ `urgent`, ordered so `max(…)` picks the most important). New
optional fields:

- `Package.priority: Option<Priority>` — explicit override.
- `WorkloadMeta.default_priority: Option<Priority>` —
  workload-level default.

New method `Inventory::priority_for(name)` resolves the value
for a package: per-package field wins outright (including
`unspecified` as an explicit opt-out); else the max
`default_priority` across every workload listing the package.
Both fields serialize to lowercase TOML strings.

### poi-tracker: `triage-updates` and `config` subcommands

`triage-updates` raises the Bugzilla priority on
release-monitoring bugs for inventoried packages whose
resolved priority is set. For each such package, queries OPEN
bugs reported by `upstream-release-monitoring@fedoraproject.org`
against `Fedora` and `Fedora EPEL` and updates any whose
current priority is `unspecified` — leaving already-triaged
bugs alone. `--dry-run` previews; otherwise prompts unless
`--yes`.

`config` walks an interactive Bugzilla API-key setup mirroring
`ebranch config`. Storage at `~/.config/poi-tracker/config.toml`
with restricted perms; lookup order at runtime is `--api-key`
→ `BUGZILLA_API_KEY` env → config file.

### hs-relmon: `prune-tags` / `prune-manifest` subcommands

Untag old hyperscale builds, keeping the N newest in each
`-release` / `-testing` tag. Enumerates the candidate managed
tags (cross product of EL version × repository × stage), calls
`listTagged` once per candidate for the package, and emits
`koji untag-build` calls for everything past the retention
threshold. Per-tag progress is printed with `--verbose`.

Defaults: 2 builds kept per `-release` tag, 1 per `-testing`.
`--repositories main` is the default repository filter;
`--repositories main,facebook` opts into additional channels.
Output is a per-tag breakdown listing both the builds that
will stay tagged and the ones to be untagged, so the user can
sanity-check before confirming. `--dry-run` previews without
acting; without it, prompts per package unless `--yes`.
`prune-manifest <path>` walks every package in the manifest
with the same options, and accepts `--skip <list>` to exclude
packages that manage their own tag cleanup (e.g. systemd).

`-candidate` and tags whose repository isn't in
`--repositories` are not touched.

## v0.11.1

### sandogasa-report: tags and releases on both forges

`GithubReport` and `GitlabReport` gained `tags_pushed` and
`releases_published` fields, with matching summary lines and
detailed `### Tags pushed` / `### Releases published`
sections.

GitHub tag detection walks each touched repo's tag refs via
the Git Refs API and resolves annotated tag objects to check
the tagger date and identity. The user-events stream alone
can't carry tag info: `git push --follow-tags` folds the tag
creation into the PushEvent (which only lists the branch
ref), so a release-tag push doesn't surface as `CreateEvent`.
Match heuristic: tagger.date in the window AND tagger.name or
tagger.email matching the user's GitHub profile name/email
(case-insensitive). Lightweight tags are skipped — they carry
no tagger metadata. GitHub Releases stay on events
(`ReleaseEvent` with `action == "published"`).

GitLab tag detection is two-stage. The events stream tells us
which projects had any user tag push, but the events
themselves can omit per-tag names (a `git push --tags` of N
tags fires one event with `ref_count: N` and `ref: null`) and
GitLab's `tag.created_at` follows the tagger date for
annotated tags rather than the push time, so a batch of tags
created locally across several days but pushed at once
doesn't cluster around the event timestamp. So for every
project where the user pushed any tag, we list the project's
tags and include all entries with `created_at` in the window.
GitLab Releases come from a per-project query against
`/projects/:id/releases`, filtered to releases authored by
the user and released inside the window.

`sandogasa-github` gained `GitTagRef`, `GitObject`,
`AnnotatedTag`, `Tagger` types plus `Client::list_tag_refs`
and `Client::get_annotated_tag`. `User` gained `name` and
`email` fields (both optional). `sandogasa-gitlab` gained
`Tag`, `Release`, `ReleaseAuthor`, `ReleaseLinks` types plus
`list_tags` and `project_releases`.

## v0.11.0

### New: sandogasa-github library crate

Minimal blocking GitHub REST client scoped to what sandogasa
tools need for activity reports: token validation, user
identity lookup, paginated user events, the Search Issues API
for pull requests, and per-repo authored-commit counts. Mirrors
`sandogasa-gitlab` in shape so downstream tools can treat the
two forges structurally the same.

Surface:

- `Client::new(base_url, token)` with `Accept:
  application/vnd.github+json`, `X-GitHub-Api-Version:
  2022-11-28`, and a 120s request timeout.
- `validate_token` — three-state return
  (Ok(true)/Ok(false)/Err) distinguishes rejected creds from
  transport errors.
- `user_by_username` — Ok(None) on 404 so callers can recover.
- `search_pull_requests(query)` — paginated over Search Issues
  up to GitHub's 1000-item cap.
- `user_events(username)` — paginated up to GitHub's
  300-event/3-page cap.
- `count_authored_commits` — treats 404/409 as "no commits" so
  an empty/gone repo doesn't abort the run.

DEVELOPMENT.md captures the design choices that aren't obvious
from the code.

### sandogasa-report: GitHub activity reporting

New data source mirroring GitLab. Each domain can declare
`[domains.<name>.github]` with an `instance` URL (defaults to
`https://api.github.com`) and an optional `org` prefix; the
tool queries the user's PRs (opened / merged / reviewed /
commented on) via the Search Issues API, then walks user
events to find touched repos and counts authored commits per
repo. Rendered as `## GitHub (<domain>)` sections alongside
GitLab.

Profile schema gained `[users.<key>.github]` for per-instance
GitHub usernames, and the overlay gained `[github_tokens]` for
persisted PATs. `--no-github` skips the queries.

Authentication: `GITHUB_TOKEN_<HOSTNAME>` env var (e.g.
`GITHUB_TOKEN_API_GITHUB_COM`) → generic `GITHUB_TOKEN` (the
same name the `gh` CLI uses) → overlay `[github_tokens]`.

`sandogasa-report config` now walks GitHub identities and
tokens in addition to GitLab. The token prompt uses the new
`sandogasa-github::validate_token`'s three-state return so a
saved-but-unreachable token isn't mistaken for an invalid one
and re-prompted needlessly.

GitHub ships with authored-commit counting only for v1;
mirror-pusher detection (analogous to GitLab's `commits_pushed`
vs `commits_authored` split) is deferred — see
`tools/sandogasa-report/TODO.md` for the rationale.

### sandogasa-report: authored-commit count alongside pushed (breaking JSON)

GitLab's push events credit every commit in a push to the
pusher, so a single `git push --mirror` of someone else's repo
can wildly inflate the numbers. Sync now cross-checks with
`/projects/:id/repository/commits?author=<user>` and reports
both:

    - **Commits pushed:** 193 across 6 project(s)
    - **Commits authored:** 14

In detailed mode, the per-project breakdown shows both side by
side so a mirror is obvious at a glance:

    - `CentOS/Hyperscale/rpms/kernel`: 0 authored / 187 pushed
    - `CentOS/Hyperscale/rpms/perf`:   12 authored / 14 pushed

Cost: one additional API call per unique project the user
pushed to.

JSON shape change: `GitlabReport.commits_by_project` is renamed
to `commits_pushed`; a sibling `commits_authored` map is added.

`sandogasa-gitlab` gained `count_authored_commits` as a reusable
primitive.

### sandogasa-report: user profiles (breaking)

Replaces the old `[users] <fas> = "<email>"` map and the
`[domains.X.gitlab].user` override with first-class user
profiles. One profile represents a single person and ties
together their per-service identities — FAS login, Bugzilla
email, and GitLab usernames per instance:

```toml
[users.michel]
fas = "salimma"
bugzilla_email = "michel@example.com"

[users.michel.gitlab]
"gitlab.com" = "michel-slm"
"salsa.debian.org" = "michel"
```

`sandogasa-report report --user michel` resolves the profile
once and each backend picks the right username:

- Bugzilla / Bodhi / Koji: `profile.fas` (or the profile key if
  unset)
- GitLab on `<host>`: `profile.gitlab[<host>]` → `profile.fas` →
  raw `--user`

Unknown `--user` values still work — they're treated as a raw
FAS login for back-compat with scripts that don't use profiles.

`sandogasa-report config` now walks through: profile key
(showing existing profiles), FAS username, Bugzilla email,
per-instance GitLab usernames, per-instance tokens. Every value
has a default (the current one) so re-running with Enter
presses keeps everything in place.

Breaking changes:

- `[users] <fas> = "<email>"` → `[users.<profile>]
  bugzilla_email = "<email>"`
- `[domains.X.gitlab].user` is dropped — move to
  `[users.<profile>.gitlab].<host>`

### sandogasa-report: persisted GitLab tokens

`sandogasa-report config` now prompts for a GitLab API token per
unique instance after the username round and saves them to the
overlay under `[gitlab_tokens]` keyed by hostname (e.g.
`"gitlab.com" = "glpat-…"`). Existing tokens are validated on
re-run and kept if still working. The overlay file is written
with 0600 permissions.

Token lookup order: `GITLAB_TOKEN_<HOSTNAME>` env var →
`GITLAB_TOKEN` env var → `gitlab_tokens.<host>` from the
overlay. Env vars win over config so a one-shot shell override
still works with a persisted token.

### sandogasa-report: `report` and `config` subcommands (breaking)

CLI restructured to a subcommand shape, matching ebranch,
cpu-sig-tracker, and other sibling tools. Existing invocations
of the form `sandogasa-report -c … -d …` now need a leading
`report`: `sandogasa-report report -c … -d …`. New subcommand
`sandogasa-report config` walks each GitLab-enabled domain from
the main config and prompts for the per-user username override,
writing the result to the overlay at
`~/.config/sandogasa-report/config.toml` while preserving any
other keys the user added manually.

### sandogasa-report: per-user config overlay

Configuration is now layered. The `-c` main config holds the
shared structure (domains, groups, koji tags, GitLab instance
URLs) and can be checked in; a per-user overlay at
`~/.config/sandogasa-report/config.toml` is auto-loaded when
present and deep-merged on top, so personal settings (GitLab
usernames, Bugzilla emails, any override) stay out of the
sharable file. Tables merge recursively; scalar and array values
are replaced wholesale by the overlay.

### sandogasa-report: GitLab activity reporting

New data source. Each domain can declare
`[domains.<name>.gitlab]` with an `instance` URL and an optional
`group` prefix; the tool fetches the user's activity events on
that instance, filters by group, and renders a `## GitLab
(<domain>)` section (bare `## GitLab` for single-domain runs).

Reported activity:

- MRs opened, merged, approved, commented on (dedup per MR)
- Commits pushed, summed per project

`--no-gitlab` flag to skip. Authentication: instance-specific env
var `GITLAB_TOKEN_<HOSTNAME>` (e.g. `GITLAB_TOKEN_GITLAB_COM`,
`GITLAB_TOKEN_SALSA_DEBIAN_ORG`) with fallback to generic
`GITLAB_TOKEN`. Lets a single run cover multiple GitLab instances
(gitlab.com + salsa.debian.org, etc.).

Each `[domains.<name>.gitlab]` block may set a `user` override
for cases where the GitLab username differs from the CLI/FAS
username (e.g. FAS `salimma` vs gitlab.com `michel-slm` vs salsa
`michel`). If unset, the CLI `--user` value is used.

`sandogasa-gitlab` gained the supporting primitives:
`user_by_username`, `user_events` (paginated), `project_summary`,
plus `User`, `Event`, `EventNote`, `EventPushData`, and
`ProjectSummary` types.

### hs-meetings: year headings at `###` level

The tool-managed meetings list is included underneath the docs'
`## Meeting minutes` parent heading, so year sections now render
as `### YYYY` instead of `## YYYY`. Fixes the sidebar indent in
mkdocs-material, where `## YYYY` sections sat at the same level
as `## Meeting minutes` and visually detached from it.

### sandogasa-bodhi: paginate `updates_for_user`, date filter, timeout (breaking)

`updates_for_user` used to fetch the full result set in a
single `rows_per_page=500` call, which Bodhi routinely needed
45s to serve and would sometimes hang entirely with no
client-side timeout. Reworked:

- Paginate at `rows_per_page=100` and invoke a caller-supplied
  `on_page` closure `(page, total_pages, running_count)` per
  response, so tools can stream progress to the user instead
  of waiting in silence.
- Accept optional `submitted_since` and `submitted_before`
  `NaiveDate` bounds that map to Bodhi's server-side filter.
  Activity reports no longer walk past the window just to
  discard everything client-side.
- `BodhiClient::new()` / `with_base_url` now build the reqwest
  client with a 120s per-request timeout so a truly hung
  connection fails loudly instead of blocking forever.

Also added `display_name` and `notes` to the `Update` model.
`title` on the API is the space-joined NVR list; the
human-readable heading users see in the Bodhi UI comes from
`display_name` (when set) or the first line of `notes`.

Breaking: `updates_for_user` signature gained
`submitted_since`, `submitted_before`, and `on_page` params.

### sandogasa-report: two-level `--detailed` Bodhi, progress, date window

`--detailed` is now a count flag — passing it twice
(`--detailed --detailed`) opts into a second detail level. All
formatters take a `detail: u8`; only Bodhi uses level 2 today,
the rest treat `>=1` uniformly.

Bodhi rendering at level 1:

    - [alias](url) (status, date)
      Latest `selinux` crates (8 builds)

The summary comes from `display_name` when set, else all
bullet-list lines of `notes` (preserving the full CVE list
when present), else the single build NVR when the update only
has one. Bullet-prefix markers (`- `, `* `, `+ `) are stripped
from each line. Level 2 additionally emits every build NVR as
an indented sub-bullet. Single-build updates also get the
sub-bullet at level 1.

Tool-side Bodhi fetch updates:

- Hands `(since - 30 days, until + 1 day)` to
  `updates_for_user` so Bodhi narrows server-side; 30-day
  buffer catches submissions that pushed inside the window.
- Wires the `on_page` callback to eprintln! when `--verbose`,
  so a long fetch streams progress per page.

Also adds DEVELOPMENT.md design notes covering the
commits-pushed/authored reasoning, event-endpoint half-open
date windows, overlay editing strategy, and future-work
section.

### sandogasa-report: trailing blank on Koji non-detailed output

Koji's summary mode (no `--detailed`) only emitted a single
trailing newline, so a following `## GitLab (…)` heading
rendered rammed up against it. Now matches the
detailed/empty paths by ending with `\n\n`.

### sandogasa-report: GitHub reviewed/commented from events, not search

The Search Issues qualifiers `reviewed-by:` and `commenter:`
match any PR the user has ever reviewed or commented on,
filtered by the PR's own timestamps — so a PR last updated by
someone else inside the window would surface even when the
user's only interaction with it was years ago. Switched to
walking the user-events endpoint (PullRequestReviewEvent,
IssueCommentEvent, PullRequestReviewCommentEvent) and
filtering on the event timestamp itself, so each entry is a
review or comment actually authored by the user in the
reporting window. See `tools/sandogasa-report/TODO.md` for the
300-event ceiling this introduces.

### ebranch: fix bogus installability issues for caps with parens

`extract_capability_names` trimmed trailing `)` from every dep,
even when the `)` was part of the capability name (e.g.
`libc.so.6(GLIBC_2.34)(64bit)` → `libc.so.6(GLIBC_2.34)(64bit`,
missing final paren). The corrupted cap then failed fedrq
lookup, surfacing as a "missing" provide for nearly every
system library. Wrapping parens are now stripped only when the
entire dep is itself a rich/boolean expression.

### sandogasa-report: per-domain Koji sections

Multi-domain runs (e.g. `--domain hyperscale --domain proposed_updates`)
now render one `## Koji CBS (<domain>)` section per domain instead of
merging all Koji activity into a single `## Koji CBS` block. Single-
domain runs keep the bare `## Koji CBS` heading. Bodhi and Bugzilla
sections are unchanged — Bugzilla still runs once across the unioned
Fedora versions, and Bodhi still merges since its release keys are
orthogonal across domains.

The JSON shape changes: `report.koji` is now an object keyed by
domain name (`{"hyperscale": {...}, "proposed_updates": {...}}`)
instead of a single `KojiReport`. The key is omitted when no domain
reports Koji activity.

## v0.10.2

### New: hs-meetings tool + sandogasa-meetbot library

CentOS Hyperscale SIG meeting archive helper. `hs-meetings
list` queries meetbot.fedoraproject.org for meetings whose
topic matches `centos-hyperscale-sig` (overridable) and prints
them as a table (date + stacked summary/logs URLs) or `--json`.
Supports calendar filters via `--period 2026Q1` (or `YYYY`,
`YYYYH1`) and explicit `--since` / `--until`.

`hs-meetings sync --file PATH` fetches from meetbot, deduplicates
against entries already in the target file (matching by date),
and inserts missing entries into the correct `## YYYY` section in
reverse-chronological order. New year sections are created
newest-first. Meetings from 2023 and earlier are dropped before
insertion — those predate meetbot and often carry hand-curated
`[agenda](...)` links, so legacy sections stay untouched. New
entries are rendered without an `agenda,` prefix (no SIG meeting
has had an external agenda link since January 2023). `--dry-run`
previews the change without writing. The target file is intended
to be a tool-managed partial pulled into `meetings.md` via
`pymdownx.snippets`.

Meetbot sometimes records multiple `!startmeeting` fragments on
a single day (same channel when the first attempt wasn't closed
cleanly, or across two rooms if the session was moved). sync
collapses all same-day entries by fetching the log HEAD for each
candidate and keeping the longest one, printing a warning with
the kept and dropped URLs. The SIG only ever runs one meeting
per day, so the longest log is taken as canonical.

`sandogasa-meetbot` gained `Meetbot::content_length` (HEAD-based
byte count) and `dedup_by_longest_log` (the grouping utility
used by sync) as reusable primitives.

Backed by a new `sandogasa-meetbot` library crate that wraps
meetbot's `/fragedpt/` search endpoint behind a typed blocking
client.

### sandogasa-cli: shared date-range helpers

`sandogasa-cli::date::{parse_period, resolve_date_range}`
extracted from sandogasa-report so hs-meetings can share the
same `--since/--until/--period` grammar. sandogasa-report
switched to the shared implementation; the grammar is
unchanged (`YYYY`, `YYYYQ1..Q4`, `YYYYH1..H2`).

### New: cpu-sig-tracker tool

Track CentOS Proposed Updates SIG package state across Koji,
GitLab, and JIRA. Manages the full lifecycle of each tracking
issue — filed when an MR against CentOS Stream exists, watched
until JIRA closes or Stream catches up, then retired and
untagged.

Subcommands:

- `config` — interactive GitLab + JIRA token setup
- `dump-inventory` — enumerate `proposed_updates<N>s-packages-main-release`
  contents into a sandogasa-inventory TOML; `--prune` drops
  packages no longer tagged in either `-release` or `-testing`
- `file-issue` — file a standardized tracking issue for an MR;
  auto-extracts package / release / JIRA key from the MR,
  applies labels, transitions work-item status to In progress,
  stamps start_date from Koji build creation time
- `retire` — close a tracking issue after verifying JIRA
  resolved + build untagged; mirrors JIRA resolution to
  GitLab (Done vs Won't do), stamps due_date, leaves an
  audit-trail comment
- `status` — per-package report with JIRA state + Koji/Stream
  NVR compare + suggested action; `--refresh` reconciles body
  format, work-item status, and start/due dates against live
  data; `--include-closed` extends the refresh scan to
  historical issues; `--package` and `--release` narrow the
  scan
- `sync-issues` — gap analysis per (release, package):
  active / proposed / missing classification
- `untag` — remove a proposed_updates build from both
  `-release` and `-testing` after verifying JIRA resolved;
  accepts either a package name or a specific NVR

Issue bodies follow a canonical markdown format so the read
side can parse back what the write side wrote; work-item
status, `start_date`, and `due_date` go via GraphQL since the
REST `PUT /issues` endpoint ignores them for work items.

### New: sandogasa-jira library crate

Minimal Red Hat JIRA REST client — issue lookup with
status / resolution / resolution date. Used by cpu-sig-tracker
to drive the retire and status flows.

### cov

- Raised the workspace line-coverage gate from 75% to 80%.
- Excluded `src/main.rs` files from the measurement — they're
  structurally 0% (the harness doesn't invoke main()) and the
  logic they delegate to is exercised by module tests.

### New: sandogasa-pkg-health tool

Audit package health across a sandogasa inventory via pluggable
checks classified by cost tier (cheap / medium / expensive).
Reports persist to TOML with selective per-(package, check,
variant) update — re-running one check preserves every other
stored entry's timestamp.

- `HealthCheck` trait (id, description, cost_tier, variants, run,
  format_result)
- Cost tiers: Cheap / Medium / Expensive
- Variant-aware checks (e.g. `bug_count:f45` vs `bug_count:epel10`)
  with independent per-variant staleness
- CLI: `run`, `show`, `checks` subcommands
- `--fedora-version` and `--epel-version` (CSV + repeatable, sorted
  and deduped with duplicate warnings)
- `--max-age` for age-based selective re-run
- `--package` and check selection flags for scoped updates
- Per-package parallelism via rayon (~3.4x speedup on 44 packages)
- JSON Schema for the report format (checked in, snapshot-tested)
- MVP checks: `maintainer_count` (Cheap), `bug_count` (Medium)
- `show` subcommand: display an existing report without re-running

### New: sandogasa-bugclass library crate

Bug classifier extracted from `sandogasa-report` into a shared
library so `sandogasa-pkg-health` can reuse it. The `BugKind` enum
is the tracker-agnostic vocabulary (Security, Ftbfs, Fti, Update,
Branch, Review, Other); per-tracker submodules hold the
classification logic. Currently only Bugzilla is supported.

## v0.10.1

### ebranch

- `check-update`: add installability check for updated packages —
  catches missing dependencies (e.g. `comfy-table`) that would make
  subpackages uninstallable
- `check-update`: output Markdown for direct Bodhi copy-paste
- `check-update`: show repo class in report (e.g. "c10s (@epel)")
- `check-update`: fix stale side tag warning false positives
- `resolve`: verify requested packages exist on source before
  resolving (catches `--source-repo rawhide` misuse)
- Fix root README: Haskell → Hyperscale for hs-intake/hs-relmon

## v0.10.0

### ebranch

- `check-crate`: allow `-r` without `-b` for side tag repos
- `check-crate`: include dev deps in build-order edges (fixes
  incorrect phasing for packages with dev-only dependencies like
  arrow-row → arrow-cast)
- `check-crate`: add `--koji` and `--copr` output modes
- `check-crate`: include root crate as the final build phase
- `check-crate`: add `--refresh` flag
- `check-update`: add `--refresh` flag
- `resolve`: remove `--phases` flag (phases are always computed)
- `resolve`: auto-use `@koji-src:` for source RPM queries when
  `--source-repo @koji:<tag>` is given
- `resolve`: validate all configured repos on startup (catches
  nonexistent Koji repos early)
- `resolve`: reject bare `@koji:` repos as source with a clear
  error message

### poi-tracker

- **New: `sync-distgit` subcommand** — create or update an inventory
  from packages a user or group has access to on Fedora dist-git
  (Pagure). Merges new packages without overwriting existing entries.
  `--user` or `--group` mode with group-access filtering via
  `--no-groups`, `--include-group`, and `--exclude-group`
- Rename `domains` to `workloads` (matching content-resolver
  terminology)
- Workload membership is now declared at the workload level
  (`[inventory.workloads.<key>]` with a `packages` list) rather
  than inline on each package
- Per-workload metadata overrides (name, description, maintainer,
  labels) for content-resolver export
- Multi-workload export: omit `--workload` to produce one YAML
  per workload
- Rename `--domain` to `--workload` across all subcommands

### sandogasa-inventory

- Add `WorkloadMeta` struct with per-workload metadata and package
  list
- Replace `domains` with `workloads` (`BTreeMap<String, WorkloadMeta>`)
- Add `workloads_for_package()`, `add_to_workload()`,
  `workload_names()` methods
- Add JSON Schema generation via `schemars` (`json_schema()`)
- Check in schema at `data/inventory.schema.json` with snapshot test

### sandogasa-distgit

- Add `user_projects()` and `group_projects()` for listing RPM packages
  by user or group from the Pagure API
- Add `AccessGroups::contains_group()` helper

### sandogasa-pkg-acl

- Validate user/group existence before setting ACLs, replacing
  a generic 404 error with a clear message

### Workspace

- Relicense from MPL-2.0 to Apache-2.0 OR MIT

## v0.9.1

### New: sandogasa-inventory library crate

- TOML-based package-of-interest inventory data model
- Content-resolver YAML export (feedback-pipeline-workload format)
- hs-relmon manifest TOML export
- Import from legacy poi-tracker JSON format
- Domain-level defaults, private field stripping, multi-inventory merge

### New: poi-tracker tool

- Package-of-interest tracker for Fedora, EPEL, and CentOS SIGs
- Commands: add, remove, show, validate, export, import
- Multi-inventory merge for exports
- Content-resolver export defaults to {name}.yaml filename

## v0.9.0

### New: sandogasa-koji library crate

- Shared Koji CLI wrappers: `list_tagged`, `list_tagged_nvrs`,
  `build_rpms`, `parse_nvr`, `parse_nvr_name`

### New: sandogasa-report tool

- Activity reporting for Fedora, EPEL, and CentOS SIG packaging work
- **Bugzilla**: review requests submitted/completed, reviews done for
  others, CVE/security, update requests, branch requests, FTBFS/FTI
  (classified via tracker bug aliases)
- **Bodhi**: updates submitted, pushed to testing, pushed to stable,
  with per-release breakdown sorted newest first
- **Koji CBS**: new packages and version updates detected by comparing
  tag snapshots at period start/end. Per-distro version merging,
  quarterly report style output
- Multi-domain support (`-d fedora -d hyperscale`)
- `--period` flag for years (2026), halves (2026H1), and quarters
  (2026Q1), plus `--since`/`--until` for arbitrary date ranges
- `--config` for project-level config (domains, groups, users)
- `--no-bugzilla`, `--no-bodhi`, `--no-koji` to skip data sources
- Brace expansion for Koji tag patterns
- Package groups with optional descriptions for categorical reporting
- User email resolution via FASJSON (rhbzemail) or config mapping

### ebranch

- **Breaking**: remove `build-order` subcommand; merged into
  `resolve --phases`
- `--exclude` flag for resolve: treat packages as already available
  on the target
- Rename `--no-auto-exclude` to `--no-auto-exclude-install`
- Fix side tag detection: use Bodhi's `from_tag` field (was
  incorrectly reading non-existent `from_side_tag`)

### sandogasa-bodhi (**breaking**)

- Rename `from_side_tag` to `from_tag` on `Update` struct (matching
  the actual Bodhi API field name)
- Add `date_testing` and `date_stable` fields to `Update`

### sandogasa-config

- Only enforce 600/700 permissions for user config files
  (`for_tool`), not project-level configs (`from_path`)

### sandogasa-cli

- New `require_tool_with_arg` for tools that use subcommands instead
  of `--version` (e.g. `koji version`)

## v0.8.1

### ebranch

- **New: `check-pkg-reviews` subcommand** — find and link Bugzilla
  package review requests based on the dependency graph from
  `check-crate --toml`. Caches bug IDs in the TOML file, batch-fetches
  bugs for speed, and prompts before applying changes
- **New: `config` subcommand** — interactive Bugzilla API key setup,
  stored securely at `~/.config/ebranch/config.toml`
- **New: `--toml` flag for `check-crate`** — save the full analysis
  (dependencies, edges, build phases) to a TOML file for reuse by
  `check-pkg-reviews` and other tools
- **New: `--dot` flag for `check-crate`** — output the dependency graph
  in Graphviz DOT format with version labels and build-phase grouping
- check-crate now resolves default Cargo features to find optional deps
  activated by default (e.g. `lexical-write-integer` via `lexical-core`)
- check-crate dev deps included by default (`--exclude-dev` to skip),
  matching Fedora's `%check`-enabled builds
- check-crate checks all RPM provider versions, finding compat packages
  (e.g. `rust-rand0.9`). Deps satisfied by compat packages are flagged
- check-crate resolves transitive dep versions matching the parent's
  semver requirement instead of always fetching the latest
- Rename `TooOld` to `Unmet` with full available-versions list
- Rename `--include-too-old` to `--include-unmet`
- Transitive deps now carry a `status` field (`missing` vs `unmet`)
  and a `package` field (RPM source package name)

### sandogasa-config

- Config files are now saved with 600 permissions and directories
  with 700, protecting API keys similar to SSH key files
- `load()` automatically fixes permissions on existing config files

### sandogasa-bugzilla

- New `bugs()` method for batch-fetching multiple bugs in one request

### hs-relmon

- Migrate config storage to `sandogasa-config`, gaining automatic
  secure file permissions for the GitLab access token

### Workspace

- Alphabetize subcommand sections in all tool READMEs to match
  `--help` output order

## v0.8.0

### New: sandogasa-cli library crate

- Shared `require_tool()` function for checking external tool
  availability at startup with clear install hints

### ebranch

- **New: `check-crate` command** — analyze a crates.io crate's
  dependencies against a target RPM repo
  - Shows missing, too-old, and satisfied dependencies with semver
    version matching
  - `--transitive` / `-t` expands missing deps recursively with
    phased build order (topological sort)
  - `--include-dev`, `--include-optional`, `--include-too-old` to
    widen transitive expansion
  - `--exclude CRATE,...` to skip crates (e.g. criterion) from
    transitive expansion
  - Partial version resolution: `57` resolves to highest `57.x.y`,
    `57.3` to highest `57.3.y`
  - Deduped crate counts when the same crate appears with different
    dependency kinds
- **`check-update` improvements**:
  - Prefer `@testing` repo (authoritative metadata) over side tag
  - Auto-detect testing branch from EPEL side tag names and Bodhi
    release metadata
  - Warn on stale side tag repos
  - Document EPEL 10 `@testing` limitation
- Parallelize fedrq queries with rayon (~4x speedup on 4 cores)
- Check for `fedrq` and `koji` availability at startup with clear
  error messages

### hs-relmon

- Reopen closed GitLab issues with matching title instead of creating
  duplicates

### sandogasa-bodhi (**breaking**)

- Add `from_side_tag` field to `Update` struct
- Add `branch` field to `Release` struct
- Add `update_by_alias()` for single-update API lookup

### Workspace

- External tool dependency checks: tools that shell out to fedrq or
  koji now verify availability at startup
- Move tool configs to top-level `configs/` directory
- Add source file ordering convention to CLAUDE.md
- Add dependency management guidelines to CLAUDE.md

## v0.7.0

### New: sandogasa-depfilter library crate

- Shared RPM dependency filtering for cross-branch analysis
- Classifies solib symbol version deps, soname deps, and RPM-internal
  deps (rpmlib, auto, config)

### ebranch

- Auto-exclude solib symbol version deps (e.g.
  `libc.so.6(GLIBC_2.38)(64bit)`) from installability checks — removes
  the need to manually `--exclude-install glibc` in most cases
- `--no-auto-exclude` flag to disable auto-exclusion
- Use shared dep filtering from sandogasa-depfilter

### koji-diff

- Fall back to build storage HTTP download when task logs have been
  garbage collected (requires build reference, not task reference)
- Retry with exponential backoff on transient server errors (502/503/504)
- **Breaking**: `BuildInfo` struct has new public fields (`name`,
  `version`, `release`)

### hs-intake

- Use shared solib detection from sandogasa-depfilter

### Workspace

- Fix all clippy warnings across workspace
- Add clippy cleanliness rule to CLAUDE.md

## v0.6.3

### New: koji-diff tool

- Compare buildroot and build logs between two Koji builds
- Accepts Koji build URLs, task URLs, or `build:<ID>`/`task:<ID>` refs
- Resolves builds to buildArch tasks via Koji XML-RPC API
- Downloads logs using `koji download-logs` with profile support
  (koji.fedoraproject.org, cbs.centos.org, kojihub.stream.centos.org)
- Parses installed packages from the DNF transaction table in root.log
  (supports both DNF4 and DNF5)
- Color-coded version change output using Rust semver rules:
  green (same version), yellow (compatible), orange (0.x minor break),
  red (major break)
- Shows mock_output.log for dependency resolution failures, build.log
  for rpmbuild failures
- `--json` flag for machine-readable output
- `--arch` to select architecture (default: x86_64)

### New: ebranch tool

- Build dependency resolver for cross-branch package porting
  (Rust rewrite of the Python ebranch tool)
- Compute build order for porting packages between branches
- `--koji` flag for chain build command output
- `--copr` flag for batch build script generation
- `--check-install` for subpackage installability verification

### New library crates

- **sandogasa-fedrq**: wrapper for the fedrq CLI tool (RPM repo queries)
- **sandogasa-rpmvercmp**: pure Rust implementation of RPM's rpmvercmp
  algorithm with epoch-version-release comparison
- **sandogasa-gitlab**: GitLab REST and GraphQL API client
- **sandogasa-repology**: Repology package version tracking API client

### Workspace

- Unify all tool versions to use `version.workspace = true`
- Integrate hs-intake and hs-relmon into the workspace, refactored to
  use shared library crates (sandogasa-fedrq, sandogasa-rpmvercmp,
  sandogasa-gitlab, sandogasa-repology)

## v0.6.2

### sandogasa-hattrack

- Display Discourse custom status (emoji + description) and expiration
  in the `last-seen` summary

## v0.6.1

### sandogasa-mailman

- Fix sender search to check all candidate email addresses per page
  instead of exhaustively scanning all pages for one address at a time

### sandogasa-hattrack

- Fix slow mailing list lookups for users who post from a non-primary
  email address

## v0.6.0

### New: sandogasa-hattrack tool

- Look up a Fedora contributor's activity across multiple services
- Subcommands: `discourse`, `bodhi`, `bugzilla`, `distgit`, `mailman`,
  `last-seen`
- `last-seen` summary shows the most recent activity from each service,
  sorted by date
- Discourse: profile info, timezone, location, custom status with
  rendered emoji, last post/seen timestamps
- Bodhi: last update submitted and last comment/karma
- Bugzilla: last bug filed and last bug changed
- Dist-git: daily activity stats (last 7 days), last PR filed,
  actionable PRs awaiting review
- Mailing lists: recent posts across all lists via HyperKitty API
- All timestamps include relative time ("3 days ago", "in 2 hours")
- `--json` flag for machine-readable output on all subcommands
- Email discovery via FASJSON (Kerberos) with `--email` override and
  `--no-fas` to skip authentication

### New: sandogasa-discourse crate

- Discourse forum API client for user profile data
- Fetch timezone, location, custom status, last post/seen timestamps

### New: sandogasa-fasjson crate

- FASJSON (Fedora Account System) API client via `curl --negotiate`
- Kerberos ticket management: status check, renewal, interactive
  acquisition with retry on timeout
- Read Fedora UPN from `~/.fedora.upn`

### New: sandogasa-mailman crate

- HyperKitty (Mailman 3) archive API client
- Find sender by email across list archives
- Fetch recent posts by sender across all lists

### sandogasa-bodhi

- Add `updates_for_user()` and `comments_for_user()` for user activity
  queries
- Add `Comment` and `CommentsResponse` models

### sandogasa-distgit

- Add `user_activity_stats()` for daily action counts
- Add `user_pull_requests()` for PRs filed by a user
- Add `user_actionable_pull_requests()` with pagination-aware total
  count
- Add `PullRequest`, `PullRequestsResponse`, and `Pagination` models

## v0.5.0

### fedora-cve-triage

- Add `cross-ecosystem` command to detect CVEs misattributed across
  ecosystems (e.g. JavaScript CVE filed against a Rust package with a
  similar name)
- Ecosystem detection from Fedora package names (`rust-*`, `nodejs-*`,
  `python-*`) with spec file fallback for ambiguous names
- Validate Bugzilla API key in `config` command via `valid_login` endpoint

### sandogasa-bugzilla

- Add `valid_login()` method for API key validation

### sandogasa-distgit

- Add `Ecosystem` enum and ecosystem detection functions
  (`is_js_package`, `is_rust_package`, `is_python_package`,
  `detect_ecosystem`) with quick name-based and full spec-based modes

### sandogasa-nvd

- Add NVD reference URL parsing (`CveReference`, `github_repos()`)
- Add `has_npm_references()` for detecting JavaScript packages via
  npmjs.com URLs
- Add npmjs.com reference check as 4th strategy in `targets_js()`
- GitHub repo language detection fallback for cross-ecosystem command

## v0.4.0

### New: sandogasa-pkg-acl tool

- View and manage Fedora package ACLs via the Pagure dist-git API
- Subcommands: `show`, `set`, `remove`, `apply`, `give`, `config`
- Batch ACL application from TOML config files across multiple packages
- `--strict` flag to downgrade access when target already has higher level
- Access checks: require admin for modifications, owner for transfers
- Owner protection: cannot downgrade or remove a package owner
- Username caching to avoid repeated token verification
- `--json` flag for machine-readable output on all subcommands

### New: sandogasa-config crate

- Shared config file management (`ConfigFile`) and interactive prompting
  (`prompt_field`) extracted from fedora-cve-triage for reuse across tools
- Email address validation helper

### sandogasa-distgit

- ACL management: `set_acl`, `remove_acl`, `get_acls`, `get_contributors`
- Ownership transfer: `give_package` via Pagure PATCH API
- User validation: `user_exists`
- Access level model with ordering, display, serde, and `FromStr`
- Access checking with direct and group membership support
- Token verification via `/api/0/-/whoami`

### Workspace

- Centralize all dependencies in `[workspace.dependencies]`
- Add `--json` requirement for non-interactive subcommands (CLAUDE.md)

## v0.3.1

- Fix --edit-bodhi to preserve existing bug references when adding new ones
- Convert to Cargo workspace with sandogasa library crates (bodhi, bugzilla, nvd, distgit)
- Move binary crate to tools/fedora-cve-triage for multi-tool workspace layout

## v0.3.0

- Add unshipped-tools command to detect CVEs for tools not shipped in RPMs
- Add Bugzilla email to config and prompt to reassign bugs when closing them
- Support filtering bodhi-check bugs by assignee (opt-in per-user triage)
- Add global -v/--verbose flag for progress on rate-limited API queries
- Fix bodhi-check false positives from mismatched NVD products:
  - Only compare versions when NVD product matches Fedora component
  - Use fedrq RPM provides to resolve name mismatches (e.g. django → python-django3)
  - Expand [epel-all] bugs to check all active EPEL releases

## v0.2.2

- Batch Bugzilla updates to close multiple bugs in a single API request
- Update project guidelines (code style rules, revised coverage threshold)

## v0.2.1

- Fall through to description heuristics when CPE has wildcard target_sw
- Hide API key input in config command

## v0.2.0

- Add bodhi-check subcommand to detect CVE bugs already fixed in Bodhi
- Add lag-tolerant tracker blocking for late-filed CVE bugs
- Add unit tests and enforce minimum coverage threshold

## v0.1.1

- Fix license text to MPL-2.0

## v0.1.0

- Initial release
- CLI with Bugzilla product/component/assignee/status filters
- js-fps subcommand to detect JavaScript/NodeJS false positives
- Three-strategy JS detection: CPE target_sw, CNA source, description keywords
- config command for Bugzilla API key setup
- Paginated Bugzilla search results
