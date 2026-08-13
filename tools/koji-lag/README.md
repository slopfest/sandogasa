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
credentials, no `koji` CLI. Datasets are plain JSON
([schema](data/koji-lag-dataset.schema.json)) and merge losslessly,
so maintainers and SIGs can sweep windows independently and pool
the results. All timestamps are UTC unix seconds.

## Installation

```sh
cargo install koji-lag
```

No external tools required.

## Usage

### Fetch

Sweep a completion-time window into a local dataset:

```sh
koji-lag fetch --days 7 -o week.json
koji-lag fetch --since 2026-07-01 --until 2026-07-14 -o sprint.json
koji-lag fetch --days 7 --inventory inventory.toml -o mine.json
```

Windows cover **whole UTC days** and select builds by
**completion time**. `--days N` means the last N complete days:
run at any hour, it ends at today's 00:00 UTC, so the partial
running day is never mixed in — fetching "a few days at a time"
composes without seams (pass `--until` with today's date to
explicitly include the running day, clamped to now). A build
still running when a window is fetched has no timing to report
yet; it is picked up by whichever fetch covers the day it
*finishes*, so periodic collection counts every build exactly
once.

`--instance` picks a known hub (`fedora` default; `cbs` and
`stream` are registered but not yet validated) or `--hub-url`
points anywhere. By default the whole window is swept — scoping
flags (`--owner`, `--package`, `--inventory`) narrow it, and such
datasets are marked as filtered so pooled reports can warn that
they under-represent the instance.

Sweeps are single-threaded and paced (`--sleep-ms`, default
500 ms between requests) out of politeness to the hub, and avoid
server-side completion filters entirely (that query class times
out on a loaded hub): parent `build` tasks are found by walking
pages newest-first by task id and windowing client-side, and the
per-arch tasks come from parent-batched queries that hit koji's
task index — both stay fast regardless of load. The window's upper bound is frozen at sweep start, so
re-running after a failure resumes cleanly (partial data is
saved, coverage is never overclaimed). Re-fetching an overlapping
window into the same file refreshes still-running tasks and
coalesces the coverage windows.

#### Reading the report

Per-arch wait and run times are reported for the whole window (`All
builds`) and then again for each shape of build, because the questions
differ. A build made for several arches has one that finished last, so
its delay can be attributed; a single-arch build has nothing to be slower
than; and a `noarch` package builds once on a machine the hub chooses,
where the arch that matters is the host's rather than the package's. Each
section carries median and p90 for both the queue wait and the run, so a
slow queue and a slow machine stay distinguishable.

The source rebuild every build begins with (`rebuildSRPM`) is reported as
its own section, keyed by host arch. Koji picks that host independently of
what the build targets, so a package can wait on a machine it does not
build for — and that wait is part of its wall clock either way.

Attribution deliberately ignores the source rebuild: it runs before the
per-arch builds rather than racing them, so counting it as an arch would
make every real arch look like the bottleneck by a margin that includes
work nothing waited on in parallel.

Datasets swept before host arches were recorded still report: their
`noarch` rows read `noarch (host unknown)` rather than guessing an arch or
dropping the tasks.

#### Reading the progress output

`-v` narrates both halves of a sweep. Fedora's hub carries several
thousand `build` tasks a day — around 7,700 measured in August 2026,
counting every branch, every side tag and every scratch build — so a
month-long window runs to hundreds of pages, and the lines are written to
show where in that a run has got to.

```
[koji-lag] build walk: page 3 (1000 task(s), 3000 so far),
    back to 2026-08-13 00:53 — 11% of the window, ~24 page(s) to go
[koji-lag] children: batch 47 (40 parent(s), 812 task(s))
    — 1880 of 5082 parent(s), 37%, ~80 batch(es) to go
```

For the **build walk**: the page number and the tasks on it, the running
total of tasks examined, and the creation time the walk has reached. The
walk runs newest-first from *now*, so a window in the past is reached only
after walking through everything more recent — fetching July while it is
August means walking August first, which is why the reached time can be
outside the window. The percentage is of the distance from the newest task
to the window's start, and the page estimate comes from the task density
seen so far, so it shifts as the walk crosses quiet weekends and busy mass
rebuilds.

For the **children**: the batch number, the parents asked about together
and the child tasks that came back, then how many parents are finished out
of the total the walk found. A batch whose answer fills a page is split in
half and retried, which says `splitting` and finishes no parents — so the
parent count, not the batch count, is the one that measures progress.

Because the walk always starts at now, one wide window costs far less
than many narrow ones: `--since 2026-06-01 --until 2026-07-31` walks the
history once, where fetching those days one at a time walks it again for
every day. Coverage windows coalesce on merge either way.

### Merge

Pool independently collected datasets (records dedupe by
instance + task id; the newest completion wins):

```sh
koji-lag merge alice.json bob.json sig.json -o pooled.json
```

Coverage gaps between the merged windows are reported so a
"quiet week" isn't mistaken for a healthy one.

### Report

```sh
koji-lag report pooled.json
koji-lag report pooled.json --since 2026-07-15 --arch s390x,ppc64le
koji-lag report pooled.json --scratch --json
```

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

## Dataset format

One JSON document (`data/koji-lag-dataset.schema.json`): `meta`
(schema version, fetch windows with their instance, bounds, and
filtered flag), `builds` and `tasks` keyed `"<instance>:<task_id>"`,
and `hosts`/`channels` id→name maps. Build and task IDs are only
unique per Koji instance, so records from different instances
(fedora, stream, cbs) coexist in one dataset.

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
