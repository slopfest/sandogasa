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
queue, how long they build, and which architecture actually gated
each build.

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
  the build. Rows sort by total gating delay, so the headline is
  literally "which arch costs the most".
- the same stats split **scratch vs official**, quantifying the
  PR-CI pain specifically.

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

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
