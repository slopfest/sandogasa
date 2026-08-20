# koji-lag

Quantify Koji build queue lag and per-arch build-time drag.

Fedora's primary architectures build in lockstep, so one slow or
queue-starved architecture (s390x, historically, and to a lesser
extent ppc64le) delays every build — and scratch builds, which
gate dist-git PR CI (installability, rmdepcheck, rpminspect,
license-validate), run at lower priority still. Slow builders and
insufficient builders aggravate each other, costing maintainer
time and rewarding merging PRs without waiting for CI. koji-lag
measures the problem from Koji task metadata: how long tasks
queue, how long they build, and which architecture each build was
actually bottlenecked on.

Everything works anonymously over the hub's XML-RPC API — no Koji
credentials, no `koji` CLI. What it collects goes into a local SQLite
store, which remembers what it has already asked the hub for, so a sweep
costs what is missing from a window rather than the whole of it and can be
interrupted and resumed at will. Reports are rendered from the store at
whatever grain it covers — day, week, month — and are small enough to
publish and diff. All timestamps are UTC unix seconds.

## Installation

```sh
cargo install koji-lag
```

No external tools required.

## Usage

### Events

Find the windows worth looking at and write one directory per window:

```sh
koji-lag events --store lag.sqlite -o reports/ \
    --schedule ~/src/fedora-pgm-schedule
```

Two kinds are detected. A **mass rebuild** is found from who submitted each
day, so it is dated from what happened rather than from what was announced —
with `--schedule` both are reported, and they differ: F45's was announced
across four weeks and submitted in four days. A **stall** is one architecture
falling an order of magnitude behind the others, and each is labelled from
throughput: `congestion` when its builders were working harder than usual and
still fell behind, `outage` when they did less than usual while the queue
grew.

Output is a flat, chronological `events/` tree, because most stalls belong to
no release event and a per-release tree has nowhere to put them:

```
events/2026-05-06-s390x-outage/event.txt      # the summary below
events/2026-05-06-s390x-outage/event.json     # the same, for machines
events/2026-05-06-s390x-outage/report.txt     # the window's own report
events/2026-07-15-mass-rebuild/…
```

Each `event.txt` gives what was measured and, where somebody has written it
down, what caused it:

```
outage on s390x
============================================================

  when       2026-05-06 .. 2026-05-08 (3 days)
  release    f44
  worst wait 46.0h, against 1m for the other architectures (3779x)
  throughput 4.7 tasks at once, 0.0 at its worst, against 6.3 ordinarily
  queue      341 tasks waiting
  tasks      860 created, 129 never ran

  cause      storage
  ticket     https://forge.fedoraproject.org/infra/tickets/issues/13326
```

Causes come from `data/outages.toml`, which ships with the tool; `--annotations
FILE` merges in more. They are matched to events by overlapping dates rather
than exact ones, so a note keeps matching when a window's edges move. An
outage with no cause recorded says so, and an annotation matching no event is
reported rather than dropped.

### Export

Write the store's rows out as CSV, for analysis in other tools:

```sh
koji-lag export --store lag.sqlite --since 2026-07-01 --until 2026-07-31 -o csv/
```

Four files, mirroring the store: `builds.csv` (one row per build task),
`tasks.csv` (one row per child task — the per-arch builds and the source
rebuild), `hosts.csv` and `channels.csv`. The last two matter more than
they look: without them a `host_id` is a number, and the arch a `noarch`
build ran on is unrecoverable.

Whole days only, as with `report` — an incomplete day is left out and named
rather than written into a file that cannot warn its reader. Sync those days
and export again.

There is no JSON export. A store travels as itself — one SQLite file — so
nothing needs re-encoding to move it.

### Probe

Time what a listing page costs before committing hours to a backfill:

```sh
koji-lag probe --depth 1,600
```

It walks the cursor from each depth exactly as `sync` does, and reports the
first page apart from the rate the walk settles into — because the first page
into a stretch of history nobody has asked about lately costs several times
what the ones behind it do. It warns when a page approaches the client's 180s
timeout, which is the point at which a backfill stops making progress rather
than merely going slowly: an abandoned request is retried, and the retry pays
the hub cost again.

Probe with this rather than with a script of your own. Fedora's proxy stack
answers a heavy query on a fresh connection in seconds and stalls the same
query on a reused one, so a client that pools connections — which most do,
including Python's `koji.ClientSession` — produces numbers that say more about
the client than the hub. `HubClient` disables pooling; see the gotcha in
DEVELOPMENT.md.

### Report

```sh
koji-lag report --store lag.sqlite --since 2026-07-15 --until 2026-07-21
koji-lag report --store lag.sqlite --since 2026-07-15 --arch s390x,ppc64le
koji-lag report --store lag.sqlite --scratch --json
koji-lag report --store lag.sqlite --owner yourname --since 2026-07-15
koji-lag report --store lag.sqlite --package gcc,llvm --arch s390x
```

`--owner` and `--package` narrow to particular accounts or source packages,
comma-separated or repeated. Both come from the parent build, so a task whose
build is not in the selected period drops out rather than being counted in.
The account is whatever Koji records, which for a service is a long name:
`koschei/koschei-backend01.rdu3.fedoraproject.org`, not `koschei`. This is
what lets a published store answer "how badly am I affected" without anyone
publishing a per-person report.

`--since`/`--until` choose the period, and the store selects a build's
child tasks by the build, so a build finishing minutes before midnight
keeps its arch tasks instead of being split across two periods.

Only whole days are analysed. A day the store has listed but not finished
fetching holds builds whose arch tasks have not arrived, and statistics over
those read as a quiet day rather than an unfinished one — so such days are
left out, named so they can be synced, and a range with nothing complete in
it is refused. `reports` applies the same rule per period: it writes nothing
for a day, week or month until the store holds all of it.

Per architecture, over the selected window:

- **queue wait** (task created → builder started) and **build
  time** (started → completed) distributions: count, median, p90,
  max. FAILED tasks count toward queue wait but are excluded from
  build time unless `--include-failed`.
- **critical-path attribution**: for every build whose per-arch
  tasks all succeeded, which arch finished last and how much
  later than the runner-up — the marginal delay that arch cost
  the build ("builds tend to be bottlenecked on the s390x
  builders"). Rows sort by total bottleneck delay, so the headline
  is literally "which arch costs the most".
- the same stats split **scratch vs official**, quantifying the
  PR-CI pain specifically.

`--format` chooses the output forms, and takes the same values on `report`
and `reports`: `text`, `json`, `csv`, comma-separated or repeated.

```sh
koji-lag report --store lag.sqlite --since 2026-07-03 --out day/ --format text,json,csv
koji-lag report --store lag.sqlite --since 2026-07-03 --out day/ --format json
```

Writing to a directory defaults to `text,json` — a reader wants the table,
a machine wants the fields, and computing the report twice to get them
separately reads the store twice. `csv` is one file per table
(`all-builds.csv`, `srpm-rebuild.csv`, `multi-arch.csv`,
`single-arch.csv`, `noarch-by-host.csv`, and `official.csv`/`scratch.csv`
when the split applies), because a CSV holds one table where the other two
carry every table for the period together.

Printing to stdout takes one form: `--format text` (the default) or
`--format json`, for which `--json` is the shorthand every tool here
accepts. `--format csv` needs `--out`, having several tables to write, and
says so rather than picking one.

Those CSVs differ from the printed tables in three ways, all for a machine's
benefit: durations are plain seconds to the millisecond rather than `2.6m`,
nothing is withheld for having few samples, and every row repeats the
instance and the period it covers — so a year of daily files concatenates
into something that still knows which day each row came from.

Report tables are padded Markdown pipe tables: aligned for
terminal and plain-text reading, and pasteable as-is into
anything that renders Markdown (Pagure, GitLab, Forgejo,
Discourse). Column glossary (also printed under every report):

| column | meaning |
|---|---|
| `queued` / `built` | tasks counted in the wait / time statistics |
| `med-wait`, `p90-wait` | task creation → a builder picked it up |
| `med-time`, `p90-time` | builder start → completion |
| `bottleneck` | builds where this arch finished **last** — the whole build was bottlenecked on it |
| `med-delay` | per bottlenecked build, how long after the *second-slowest* arch this arch finished — the extra wall-clock time it alone cost that build (median) |
| `tot-delay` | the same marginal delay summed over every build it bottlenecked in the window |

Human output withholds statistics for rows with fewer than
`--min-samples` (default 5) samples; `--json` always carries the
full numbers plus counts so pooled data can be re-filtered.

#### Reading the report

Per-arch wait and run times are reported for the whole window (`All
builds`) and then again for each shape of build, because the questions
differ. A build made for several arches has one that finished last, so
its delay can be attributed; a single-arch build has nothing to be slower
than; and a `noarch` package builds once on a machine the hub chooses,
where the arch that matters is the host's rather than the package's. Each
section carries median and p90 for both the queue wait and the run, so a
slow queue and a slow machine stay distinguishable.

The source rebuild every build begins with is reported as its own
section, keyed by host arch. Koji names it `rebuildSRPM` when a scratch
build was submitted as an SRPM and `buildSRPMFromSCM` when the source
comes from dist-git; both are the same stage and are reported together,
with the official/scratch split telling the flows apart. Koji picks that host independently of
what the build targets, so a package can wait on a machine it does not
build for — and that wait is part of its wall clock either way.

Attribution deliberately ignores the source rebuild: it runs before the
per-arch builds rather than racing them, so counting it as an arch would
make every real arch look like the bottleneck by a margin that includes
work nothing waited on in parallel.

Datasets swept before host arches were recorded still report: their
`noarch` rows read `noarch (host unknown)` rather than guessing an arch or
dropping the tasks.

### Reports

Write a report for every period the store can answer for:

```sh
koji-lag reports --store lag.sqlite --reports-root reports
koji-lag reports --store lag.sqlite --reports-root reports --since 2026-07-01
```

Each day, each week and each month the store covers gets a report, filed
as `daily/YYYY/MM/DD`, `weekly/YYYY/MM/DD` (dated to its Monday, or to the
first of the month where a week is clipped by one) and `monthly/YYYY/MM`.
Weeks never cross a month boundary, so a month's figures are the sum of
its weeks.

A period is reported only when the store holds it whole: its creation span
listed, and every build in it with its child tasks. So syncing one more
day can bring a week — or a month — into range, and pooling is just
running this again. Coarser reports are written *beside* the finer ones,
never instead of them, and each grain is computed from that period's own
rows rather than averaged up from the finer grain, because percentiles do
not compose.

`--format` works here too, and identically: `--format text,json,csv`
writes all three forms into every period's directory, `--format csv` only
the per-table CSVs. A period counts as already reported when the forms
asked for are there, so adding `csv` to a tree that has text and JSON
writes the CSVs rather than skipping the period.

Rendering is cheap and sweeping is not, so existing reports are left alone
unless `--force` is passed — worth passing after a sync has filled in rows
a period was missing.

### Sync

Bring a store up to date with the hub:

```sh
koji-lag sync --store lag.sqlite --days 7 -v
koji-lag sync --store lag.sqlite --since 2026-04-01 --until 2026-04-30
```

A sync fetches only what the store is missing and records what it has
listed as it goes, so it can be interrupted and re-run freely: a window
already covered costs no requests, one covered in part costs the
remainder, and a window in the middle of a covered stretch costs nothing
even if it was never asked for by name. Reaching a window months old
costs no more per page than yesterday's — the walk positions itself by
creation time rather than paging from the newest task.

Two stages, reported separately. First the build tasks created over the
window, plus the margin before it for builds that started earlier
and finish inside it; then the child tasks of any build in the
window that has none, 200 builds to a query.

`--store` names the SQLite database, created if absent; it holds builds,
child tasks, hosts and channels for any number of instances and periods.
Nothing is filtered on the way in, so narrowing is something `report`
does over what is stored.

#### Reading the progress output

`-v` narrates both stages. Fedora's hub carries around 8,000 `build`
tasks a day — every branch, every side tag, every scratch build — so a
month-long window runs to a couple of hundred pages, and the lines say
where in that a run has got to.

```
[koji-lag] sync: 2 gap(s) to list, 28.0 day(s) of it; 5.0 day(s) already listed
[koji-lag]   gap 2026-04-16 00:00 to 2026-05-01 00:00 (15.0 day(s))
[koji-lag] sync: page 51 (1000 task(s)), listed back to 2026-04-23 20:53
    — 48% of the gap, ~57 page(s) to go
[koji-lag] sync: children of 8147 build(s), 200 at a time
[koji-lag] children: batch 21 (200 parent(s), 704 task(s))
    — 4200 of 8147 parent(s), 52%, ~20 batch(es) to go
```

The plan comes first: which creation spans are missing and how much of
the window is already listed. Then, per page, the creation time the walk
has reached and how much of the *gap* is left — a real fraction, since a
gap has known bounds, with the page estimate following from the rate so
far.

For the **children**: the batch number, the parents asked about together
and the child tasks that came back, then how many parents are finished
out of the total. A batch whose answer fills a page is split in half and
retried, which says `splitting` and finishes no parents — so the parent
count, not the batch count, measures progress.

Requests are paced by how long the hub takes to answer them.
`--duty-cycle` (default 50) is the share of one connection to aim for, so
each pause is scaled to the last request's latency: a hub under load is
asked less often, and one that speeds up is asked more, down to the
`--sleep-ms` floor.

One more thing worth expecting: the first page of a run can take over a
minute while the hub warms up its query plan, after which pages land in
about ten seconds. A sync that looks stuck on page one usually is not.
## Notebooks

`notebooks/s390x-lag.ipynb` works a store over rather than reporting from it:
rebuild windows detected from who submitted each day, queue wait per class of
build, service time per architecture, builder capacity from the hub's
configuration history, utilisation against the wait actually observed, the
long-build tail, per-package slowdown grouped by build toolchain, and
single-architecture stalls labelled congestion or outage.

```sh
KOJI_LAG_STORE=lag.sqlite jupyter lab notebooks/s390x-lag.ipynb
```

Needs `jupyterlab`, `python3-pandas` and `python3-matplotlib`; the toolchain
section also wants a checkout of Fedora's spec files, at `$FEDORA_SPECS` or
`~/src/fedora/rpm-specs`, and skips itself without one. Outputs are committed,
so the findings read without a store to hand.

## Queries the tool does not run for you

`queries/` holds SQL for analyses that have not (yet) become report
sections, runnable against any store with nothing but `sqlite3`:

```sh
sqlite3 lag.sqlite < queries/submitters-by-day.sql
```

| query | answers |
|:--|:--|
| `submitters-by-day.sql` | which accounts submitted a period's builds, per day — how a mass rebuild is dated from evidence rather than from a schedule |
| `package-build-hours.sql` | which packages consume an architecture's capacity in a window |
| `arch-load-vs-wait.sql` | how queue wait responds to how much work an architecture is given |
| `long-builds.sql` | builds that ran longer than the sweep's grace margin, with links |

Each file explains what it is for and what it showed, and carries the
window or architecture as an editable literal at the top. They also
document the schema by example — see `data/store-schema.sql` for the
tables themselves.

## Backing up the store

```sh
scripts/backup-store.sh lag.sqlite ~/backups/lag.sqlite   # from a checkout
```

Not `cp`, if a sync might be running. The store is in WAL mode, so
committed data lives partly in `lag.sqlite` and partly in `lag.sqlite-wal`
at any moment: copying the main file alone can produce a database missing
its most recent transactions, and copying the set mid-commit can produce an
inconsistent one. Neither failure announces itself — the copy opens and
queries perfectly, with rows quietly absent.

The script uses SQLite's `VACUUM INTO`, which reads one consistent snapshot
under ordinary read locking and writes a fully checkpointed file with no
sidecars. It verifies the result with `PRAGMA integrity_check` and reports
what it holds. A 580MB store took eight seconds while a sync was writing to
it, and came out 545MB for being rebuilt compactly.

Once nothing is writing, a plain `cp` of the single file is fine.

To reclaim that space in the working store rather than only in the copy:

```sh
scripts/vacuum-store.sh lag.sqlite
```

A store accumulates free pages because every build is inserted while its
window is listed and updated again when its children arrive — a row rewrite
per build, which on a 731MB store came to 48MB, reclaimed in 14 seconds.
Worth doing occasionally, not routinely.

It refuses if anything has the store open, because `VACUUM` rebuilds under
an exclusive lock for minutes and a sync running alongside would fail once
its busy timeout ran out. Three checks, because no one of them suffices: the
`-wal`/`-shm` sidecars (SQLite removes them when the last connection closes,
so their presence means one is open), `fuser` where available, and a write
lock probe — that last one alone would miss a sync, which writes in batches
and is idle between them.

## System-wide configuration

This tool keeps no settings of its own, but it does read a `[defaults]`
table — for pinning the flags you always pass — from
`/etc/koji-lag/config.toml` and `~/.config/koji-lag/config.toml`, the
user file overriding the system one per key and command-line flags
overriding both. Either path may be absent. See the root
`DEVELOPMENT.md` for the table format.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
