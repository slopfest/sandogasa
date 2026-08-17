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

### Import

Read datasets collected before the store existed, so nothing already
fetched has to be fetched again:

```sh
koji-lag import raw_data --store lag.sqlite
koji-lag import week.json month.json --store lag.sqlite
```

Each argument is a JSON dataset or a directory tree of them. Rows are
deduplicated by task id, so importing the same dataset twice changes
nothing and overlapping windows import cleanly.

An import claims the window each dataset covers, but not the three days
before it that the dataset's sweep also read: a build created in those
days and finishing after the window is not in the file, so a later sync
re-lists them. A dataset from a scoped fetch (`--owner`, `--package`)
contributes its rows and no coverage at all.

### Report

```sh
koji-lag report --store lag.sqlite --since 2026-07-15 --until 2026-07-21
koji-lag report --store lag.sqlite --since 2026-07-15 --arch s390x,ppc64le
koji-lag report --store lag.sqlite --scratch --json
koji-lag report shared-dataset.json          # or a JSON dataset
```

Over a store, `--since`/`--until` choose the period and the store selects
a build's child tasks by the build, so a build finishing minutes before
midnight keeps its arch tasks instead of being split across two periods.
Given JSON files instead, they are merged in memory and reported the same
way.

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
window, plus the three days before it for builds that started earlier
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
## Dataset format

The JSON shape `import` reads, and the shape data is shared in
(`data/koji-lag-dataset.schema.json`): `meta` (schema version, coverage
windows with their instance, bounds, and filtered flag), `builds` and
`tasks` keyed `"<instance>:<task_id>"`, and `hosts`/`channels` id→name
maps. Build and task IDs are only unique per Koji instance, so records
from different instances (fedora, stream, cbs) coexist in one document —
and in one store.

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
