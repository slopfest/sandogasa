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

## What the hub costs

Every number below was measured against Fedora's hub (`listTasks` over
anonymous XML-RPC) in **August 2026**, from a home connection. They are
recorded because they are slow to re-measure and because most of this
tool's design follows from them; re-measure before trusting them for a
different hub or a much later date, and add what you find here.

**Listing build tasks.** A page is `method: build`, `order: -id`.

| query | rows | cost |
|:--|--:|--:|
| no filter | 1000 | 3.9s |
| `createdBefore` (four months back) | 200 | 7.9–8.9s |
| `createdBefore` | 1000 | 10.1s |
| `createdBefore` + `offset 20000` | 1000 | 7.2s |
| `createdBefore`, first call on a cold plan | 200 | **80.6s** (8.2s repeated) |
| `owner` (a user id, not a name) | 1000 | 3.6s |
| `countOnly` over a three-day creation window | — | **83s** |

The cold-plan figure is worth knowing: the first `createdBefore` page of a
run can take a minute and a half while the hub's query plan warms, and
every page after it lands in ten seconds. A sync that looks hung on its
first page probably is not.

`createdBefore` costs the same wherever it points — that is the whole
reason the walk uses it — and takes either an epoch double or a
`YYYY-MM-DD HH:MM:SS` string. They returned identical rows at identical
cost; we send the double, since the string carries no timezone.

**Positioning by offset**, which is what the walk used to do:

| probe | cost |
|:--|--:|
| one row at `offset 0` | 0.56s |
| `offset 50000` | 1.6s |
| `offset 300000` | 2.8s |
| `offset 1000000` | **81s** |

So offsets are cheap until they are not, and the cliff was around six
weeks of history. Two rules follow. **Walk with a cursor**: each page asks
for tasks created before the oldest creation the previous page returned,
which moves backwards at a flat ~10s a page and can begin anywhere.
**Never page by offset from the top of the task list**: it cannot reach far
history at all. The design that did capped out at offset 500,000 and
reported `window starts deeper than offset 500000` before spending eight
minutes paging through data it already had. Offsets are still used, but
only to drain a crowded moment a cursor cannot step past.

Do not reintroduce a count query to size a job either. It is not
affordable, and progress against a gap of known bounds is arithmetic.

**Fetching children** by the indexed `parent` filter. Almost all of the
cost is the round trip, so the batch size matters far more than the row
count:

| parents per query | rows | cost | per parent |
|--:|--:|--:|--:|
| 40 | 162 | 1.14s | 28.4ms |
| 100 | 404 | 0.55s | 5.5ms |
| 200 | 758 | 0.76s | 3.8ms |
| 400 | 1900 | 1.69s | 4.2ms |

`PARENT_CHUNK` is 200: past a hundred or so the flat cost is amortised
away, and a bigger batch only raises the chance of an answer that
overflows the page and has to be split and refetched.

**A day of Fedora**, as of 2026: about 8,100 build tasks completing, four
child tasks each (3.4 to 3.5), so roughly 33,000 rows. One SRPM step per
build — 8,140 `rebuildSRPM` plus `buildSRPMFromSCM` against 8,147 builds
on 2026-04-15, the remainder being builds that failed before one ran.

**End-to-end**, for calibrating what to expect:

- One day of April, into a store that held nothing older than June:
  **18m52s** — 31 listing pages for the four days of creations a one-day
  window needs (10.8 min at ~21s a page, half of which is deliberate
  pacing), then 8,147 builds' children in 204 batches (8 min). The same
  run at `PARENT_CHUNK` 200 spends about a minute on the children.
- Importing 676MB of JSON datasets — 517,233 builds and 1,801,758 tasks —
  took 48s and produced a 402MB store.

Pacing is a duty cycle rather than a fixed pause: at 500ms between
half-second queries the hub sees us half the time, but when it is
struggling and the same query takes eight seconds a fixed pause means
occupying it 94% of the time, leaning hardest exactly when we should ease
off. `--duty-cycle` scales each pause to the last request's latency, so
the figures above roughly double in a real run and recover by themselves
when the hub does.

## A sync is two jobs, and they are tracked apart

Listing the build tasks over a span and fetching their children are
recorded separately, because they fail and resume separately. The listing
writes a `listed` span per page; the children mark their parent as swept
per batch. Either can be interrupted and re-run without re-asking for
what landed, and the second can be behind the first for a window that was
listed by an earlier run — which is why the children stage looks at every
build in the window rather than only the ones this run listed. It is also
why an imported dataset from before some child method was collected can be
completed in place: the listing stands, and only the children are asked
for again.

A parent is marked swept even when nothing came back. A build that failed
before it started an arch task genuinely has no children, and a sweep
that only marked non-empty answers would ask about those builds again on
every run, for ever.

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
