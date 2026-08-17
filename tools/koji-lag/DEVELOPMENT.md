# koji-lag development notes

Design decisions and the rules future work should follow. For what the
tool does, see [README.md](README.md); for what changed and why, the
[CHANGELOG](../../CHANGELOG.md).

## Storage is cheaper than asking Koji again

Every task the hub tells us about is kept, whether or not the window
being reported wants it. This is the rule the whole store is built
around, and the arithmetic behind it is lopsided:

- **Asking** costs minutes. One day of Fedora is around 8,000 build
  tasks and 25,000 child tasks; the children arrive 40 parents to a
  query, so a day is roughly 200 requests and, paced politely, about six
  minutes of hub time.
- **Keeping** costs megabytes. Those 33,000 rows are a few MB of SQLite.
  A year is on the order of a gigabyte — a rounding error against any
  disk, and less than the JSON tree it replaces.

So a task fetched for one window and never used again has still paid for
itself the first time a neighbouring window would have re-fetched it. Do
not add logic that discards rows outside the window of interest, or that
"tidies up" tasks whose builds fall outside a reporting period. The
expensive thing is the round trip, and it has already happened.

## What makes skipping sound: what was listed, not what was kept

A sweep may skip work only against a record of what it **listed**, never
against a record of what it **stored**.

The distinction cost real data. An earlier design bounded each sweep by
the oldest build id in the neighbouring window's dataset, reasoning that
nothing older could be wanted. But a dataset holds the builds it *kept*
— those completing inside its window — and their oldest sits inside the
previous window's grace margin, so the bound skipped creations the window
still needed. Measured against the hub, sweeping 2026-08-01 that way
missed 32 builds, every one created between 23:46 and 23:53 and finished
the same evening.

Hence the `listed` table: creation-time spans over which every build task
has been enumerated. It is written as each page lands, so an interrupted
sweep keeps credit for exactly the pages it read, and a later sweep
subtracts those spans from what it needs. `builds.children_swept` plays
the same role one level down: it records that a parent's children have
been asked for, so they are never asked for twice.

## Page by creation time, never by offset

`listTasks` accepts `createdBefore`, and it is the whole positioning
mechanism. Measured against Fedora's hub in August 2026:

| query | cost |
|:--|--:|
| unfiltered page, `order: -id` | 3.9s |
| `createdBefore=<a date in May>` | 7–10s |
| the same with `offset 20000` | 7.2s |
| unfiltered probe at `offset 1000000` | 81s |
| `countOnly` over a three-day creation window | 83s |

Two rules follow. **Walk with a cursor**: each page asks for tasks
created before the oldest creation the previous page returned, so the
walk moves backwards through history at a flat ~7s a page and can begin
anywhere. **Never page by offset from the top of the task list**: it
cannot reach far history at all. The design that did hit a cap at offset
500,000 — which in June 2026 was only six weeks back — and reported
`window starts deeper than offset 500000` before spending eight minutes
paging through data it already had.

Do not reintroduce a count query to size a job. It is not affordable, and
progress is better expressed as the creation time reached.

## The grace margin, and why it is only on one side

A window selects builds by **completion**, so a build created earlier can
finish inside it: the listing must reach back past the window start by
more than the longest plausible build (three days, `CREATE_GRACE_SECS`).

There is no margin on the other side. A task created after the window
ends completed after it too, so nothing newer than the window's end can
belong to it. Anyone tempted to add symmetry here should note that the
asymmetry is what lets a sweep bound its own work.

## Filters belong to reports, not to sweeps

A sweep takes no `--owner` or `--package` filter. Everything is stored,
so narrowing is a query: `report --owner` reads the database and involves
the hub not at all. This also removes an inconsistency — `fetch` filtered
client-side while `backfill` did not — and a hazard: a store holding a
mixture of filtered and unfiltered coverage silently under-reports, and
nothing in a row says which sweep put it there.

The hub *can* filter by owner server-side (`owner` takes a user id, which
`getUser` resolves from a name; ~3.6s a page, composes with
`createdBefore`). That is worth remembering if a narrow personal sweep is
ever wanted, but it must then be recorded as partial coverage rather than
mixed into a shared store.

## Reports are computed per period, never combined

A weekly report is a query over the week, not an average of its dailies.
Percentiles do not compose: on 2026-08-01 and 02 the s390x median queue
waits were 62.0s and 48.8s and the weekly figure is 56.0s, which is
neither their mean nor a count-weighted one; the p90s were 1312.4s and
65.9s against a weekly 1077.1s. Ranking all the waits together is the
only honest answer.

Reports are emitted at every grain the data supports and kept at all of
them: when the database covers a whole week, the weekly report is written
beside the dailies rather than replacing them. They are kilobytes, and a
daily report answers questions a monthly one has already averaged away.
Raw data needs no such pooling any more — one database holds it, and the
period is just a `WHERE` clause.

## Not in git

The database is a local working store and is ignored. What gets published
is reports, which are small and diff cleanly, plus whatever JSON or CSV
`export` produces for sharing. A SQLite file in git is rewritten whole on
every commit and diffs to noise.
