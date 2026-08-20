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

`createdBefore` takes either an epoch double or a `YYYY-MM-DD HH:MM:SS`
string. They returned identical rows at identical cost; we send the double,
since the string carries no timezone.

**It is not free with depth, though it was once recorded here as being so.**
That claim came from probing four months back. Measured again in August 2026
over a wider range, a 1000-row page costs:

| how far back `createdBefore` points | cost |
|:--|--:|
| one day | 0.6–1.6s |
| one month | 3.4–4.0s |
| thirteen months | 17.7–24.1s |

**But the cost is the seek, not the rows**, which is what makes deep history
affordable anyway:

| rows asked for | one day back | thirteen months back |
|--:|--:|--:|
| 1000 | 0.6–1.6s | 18.3s |
| 2000 | 0.8s | 19.8s |
| 4000 | 1.3s | 20.8s |
| 8000 | 3.5s | — |
| 16000 | 5.0s | — |

Four times the rows for 14% more time at depth. So `--page-size` defaults
to 4000: it cut July 2025's listing from an estimated five hours to
twenty-one minutes, and because pacing is a duty cycle rather than a fixed
pause, a larger page does not increase our share of the hub — it just asks
it to perform a quarter as many of the expensive seeks. The hub honours
limits up to at least 16,000 without capping, which matters for more than
speed: [`crate::sweep::step`] reads a short page as "nothing older exists",
so a silently capped limit would make every page look short and claim a gap
after one request.

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
- Folding 676MB of JSON datasets into the store — 517,233 builds and
  1,801,758 tasks — took 48s and produced a 402MB store, 40% smaller than
  the JSON. (The `import` command that did it has since been removed; the
  figure is kept as the cost of bulk-inserting rows this tool already has,
  which is what a database-dump ingester would pay.)

**Measure with the tool, never with a hand-written client — and the reason
is connection reuse, not the query.** Fedora's proxy stack stalls heavy
queries sent over a reused keep-alive connection while answering the same
query on a fresh one in seconds; that is why `HubClient` sets
`pool_max_idle_per_host(0)`, and the comment there records it.

A probe written in Python against `koji.ClientSession` does not know that.
It pools, and on 2026-08-20 it produced 3.9s and 7.1s on some calls and
349s, 307s and 303s on others, which is the documented fresh-versus-reused
split and nothing to do with depth or ordering. Two wrong conclusions were
drawn from it before the mechanism was spotted — first that the hub was
degraded twentyfold, then that the query's shape mattered a hundredfold.
Both were artefacts of the client doing the measuring.

So the rule is narrow and worth keeping: **time `sync`'s own `Pages`
implementation**, which is what `koji-lag probe` does. Anything else
measures the prober.

What the tool itself then showed, listing January 2025 at roughly twenty
months' depth: pages there can exceed the client's fixed **180s timeout**
(`xmlrpc.rs`), at which point the request is abandoned and retried — and
the retry pays the hub cost again from scratch. A deep backfill is
therefore not merely slow but capable of making no progress at all, and
the fix is smaller pages at depth rather than a longer timeout: a page
that fits inside the timeout is progress kept, where a page that does not
is work thrown away three times before the retries run out.

**Size the request timeout for a cold first page, not for the steady
state.** Measured on the January 2025 backfill at twenty months' depth:
nine consecutive 4000-row pages took 27, 30, 29, 34, 28, 29, 32, 27 and
34 seconds — a spread of 1.26x, tight enough to plan against — while the
first page into that stretch of history exceeded 180s and was abandoned
there. The steady state is what a casual measurement finds and it is the
wrong number to bound by; the two differ by most of an order of magnitude.

Depth is not the variable to scale the bound on, tempting though it
looks. Steady cost does rise with depth (about 7s a page recently against
30s at twenty months), but that rise is comfortably inside any sane bound.
What actually exceeds the bound is entering a region nobody has asked
about lately, and that is a question of what the hub has cached rather
than of how old the data is. A generous fixed bound plus an override
handles both; a formula in the age would tune the wrong thing.

The asymmetry is why the default errs high: too low discards work that was
nearly finished and charges the hub for it again, three times over with
retries, while too high only delays noticing a real hang — which the duty
cycle and the retry budget already tolerate.

Pacing is a duty cycle rather than a fixed pause: at 500ms between
half-second queries the hub sees us half the time, but when it is
struggling and the same query takes eight seconds a fixed pause means
occupying it 94% of the time, leaning hardest exactly when we should ease
off. `--duty-cycle` scales each pause to the last request's latency, so
the figures above roughly double in a real run and recover by themselves
when the hub does.

## Gotcha: reusing a connection makes Koji *slower*, not faster

Backwards, and it has now cost two wrong conclusions, so it is written
here rather than only in the code that works around it.

Fedora's proxy stack stalls a heavy XML-RPC query sent over a **reused**
keep-alive connection while answering the identical query on a **fresh**
connection in seconds. Reproduced originally with `curl --next`: fresh
connection 3-90s, reused connection times out. `HubClient` therefore sets
`pool_max_idle_per_host(0)` and `http1_only()` — it deliberately throws
away every connection, because a TCP and TLS handshake per request is
noise next to a multi-megabyte task page and next to the pacing we do
anyway.

Everything ordinary about HTTP performance points the other way, which is
what makes it a trap: keep-alive is the optimisation everywhere else, so
any client written without knowing this is *fast by accident and slow by
default* — the first call on a new connection is quick and the ones that
follow it stall.

**The consequence for measurement.** A probe written against
`koji.ClientSession` in Python pools connections, so its numbers are a
mixture of the two regimes with no way to tell which is which. On
2026-08-20 one produced 3.9s and 7.1s on some calls and 349s, 307s and
303s on others, and that spread was read first as "the hub is degraded
twentyfold" and then as "the query's shape matters a hundredfold". Both
were the prober measuring itself. `koji-lag probe` exists so this cannot
happen again: it times `sync`'s own `Pages`, on the client that disables
pooling.

If Fedora's proxy stack is ever fixed, pooling becomes worth having again
and this section is the record of why it was off — do not turn it back on
without re-running the `curl --next` comparison.

## Changing the schema, and paying for a new field

Two separate problems, with two separate mechanisms. Conflating them is
how a store ends up with a column nothing ever fills.

**Structure** is `MIGRATIONS` in `store.rs`: a list of DDL steps where the
index is the version. A step is applied to any store below its version and
never again, so **the list is append-only** — editing a step that has
shipped changes what new stores get while leaving existing ones alone, and
the two then differ with nothing to say so. Whitespace is the only safe
edit. A store recording a higher version than the binary knows is refused
rather than opened, because rows written by a newer binary may mean
something this one would misread; a lower one is migrated up, which is
tested. `data/store-schema.sql` is generated from a fresh store and
checked by `store_schema_up_to_date`, so a schema change cannot land
without appearing as a diff of the schema itself.

**Data** — the values of a new column for rows already stored — is what
the two generation constants are for, and what they cost differs by orders
of magnitude:

| the field comes from | bump | what it costs |
|:--|:--|:--|
| the build listing | `LISTING_GEN` | re-list: about an hour a year |
| the child queries | `CHILDREN_GEN` | re-fetch children: days a year |
| neither — rows are now wrong | `SCHEMA_VERSION`, and rebuild | a full sweep |

A `LISTING_GEN` bump makes every span recorded under an older generation
read as a gap, so the next sync re-lists it and the column fills itself. A
`CHILDREN_GEN` bump does the same one level down, per build. Neither
touches the other's work, which is the entire reason they are separate
columns rather than one "generation" — a new child method must not cost a
re-list, and a new listing field must not cost days.

## A build's package name comes from its children

Koji answers a `build` task's request with a git URL when the source came
from dist-git, and with an SRPM path only when someone uploaded one. So a
package name can be parsed from the request of about 2% of the builds on a
mass-rebuild day — and those are the days worth analysing. The children are
different: each was handed a specific SRPM, so 81% of them name their
package.

`fill_missing_packages` therefore derives a build's name from any child that
has one, after the children stage of every sync. It is a local `UPDATE`
against rows already stored — no request to the hub — and on a month of
Fedora it named 49,438 builds in 0.4s, taking July from 24% unnamed to 3%.
The remainder are builds whose children name nothing either: they failed
before a child ran, or their requests do not parse.

Two things follow for anyone extending this. Re-running `sync` over an
already-covered window is the repair path — it fetches nothing and fills
what it can, which is why no separate repair command exists. And `nvr`
cannot be recovered the same way: a child's request carries the full NVR but
only the package parsed out of it was ever stored, so repairing that would
mean asking the hub for every child again.

## A sync is two jobs, and they are tracked apart

Listing the build tasks over a span and fetching their children are
recorded separately, because they fail and resume separately. The listing
writes a `listed` span per page; the children mark their parent as swept
per batch. Either can be interrupted and re-run without re-asking for
what landed, and the second can be behind the first for a window that was
listed by an earlier run — which is why the children stage looks at every
build in the window rather than only the ones this run listed. It is also
why rows listed before some child method was collected can be completed in
place: the listing stands, and only the children are asked for again.

A parent is marked swept even when nothing came back. A build that failed
before it started an arch task genuinely has no children, and a sweep
that only marked non-empty answers would ask about those builds again on
every run, for ever.

## The grace margin, and why it is only on one side

A window selects builds by **completion**, so a build created earlier can
finish inside it: the listing must reach back past the window start by more
than the longest plausible build (eight days, `CREATE_GRACE_SECS`).

Eight, not the three it began as, because the store eventually grew large
enough to check. Of 1,220,010 builds, 13 exceeded three days and the
longest ran **6.76** — `python-dask`, task 146570209. The others over five
days were `gcc`, `llvm` and `rust`. A margin that misses those misses
precisely the builds worth studying, and it misses them *silently*: a build
never listed leaves nothing behind to notice.

The margin is nearly free where coverage is contiguous, since a
neighbouring period has already listed it and `gaps` subtracts what is
listed. It costs about 44 pages once, at the leading edge of an isolated
stretch. Raising it does invalidate completeness at existing edges — a
period whose margin is only partly listed stops counting as whole, which is
the honest answer rather than a regression.

There is no margin on the other side. A task created after the window
ends completed after it too, so nothing newer than the window's end can
belong to it. Anyone tempted to add symmetry here should note that the
asymmetry is what lets a sweep bound its own work.

## Filters belong to reports, not to sweeps

A sweep takes no `--owner` or `--package` filter. Everything is stored,
so narrowing is a query: `report --owner NAME` and `report --package NAME`
read the database and involve the hub not at all. A task whose parent build
is absent from the selection drops out of a narrowed report rather than
being counted into it — a child task records neither owner nor package, so
one that cannot be attributed cannot be shown to match. This also removes an inconsistency — `fetch` filtered
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

## One store, and what would make that stop being true

Measured 2026-08-17 against a 731MB store holding 939,740 builds and
3,181,577 tasks (March, April, June, July and early August of 2026):

| query | rows | cost |
|:--|--:|--:|
| one day's builds | 7,377 | 0.3ms |
| one day's builds joined to their children | 26,317 | 14.7ms |
| a month's builds joined to their children | 762,609 | 210ms |

Every one of those is an index `SEARCH`, never a scan, so the cost tracks
the rows a period holds and not the size of the store. At roughly 165MB a
month — about 177 bytes a row — a year is 2GB and a decade 20GB, against
SQLite's 281TB limit and a B-tree that deepens logarithmically. **Do not
split the store because it is large.** A daily report over a decade of data
would still take milliseconds.

What will eventually justify splitting is *backup*, not queries.
`VACUUM INTO` rewrites the whole file, so five years in one store means
re-uploading five years to capture one new day. Splitting by period would
make a completed period immutable and uploaded once. The trigger to watch
is therefore how long a backup takes, not how big the file is.

## If the store is ever split, do not union in SQL

The obvious approach is `ATTACH` plus `UNION ALL` views over the attached
schemas. Measured on two real shards, one query took **33.8 seconds** where
a single store answered the same question in 14.7ms: SQLite materialises
the union and then scans it, so the join loses `tasks_parent` entirely
(`SCAN b`, `SCAN t` in the plan).

Querying each store separately and merging the results was **5,600 times
faster** — 6.0ms total, with each store keeping a *covering* index on the
join — and is also less code: no `ATTACH` (so no ten-database limit), no
schema-qualified views, no cross-file joins. `Dataset::merge` and
`add_listed`'s span merging already do the merging half.

Two rules for whoever implements it:

- **A build and its children live in the same store**, whatever their own
  timestamps say. Children are selected by parent, so splitting a build
  from its arch tasks loses them from every report. Route by the *build's*
  completion period and let its children follow.
- **Coverage is creation time, rows are completion time**, so a listed span
  crossing a period boundary splits into one row per store. That is benign
  — spans merge on read — but it means a store's rows and its coverage are
  keyed differently, which is worth knowing before debugging it.

## Not in git

The database is a local working store and is ignored. What gets published
is reports, which are small and diff cleanly, plus whatever JSON or CSV
`export` produces for sharing. A SQLite file in git is rewritten whole on
every commit and diffs to noise.
