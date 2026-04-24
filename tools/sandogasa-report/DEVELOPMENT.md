# sandogasa-report Development Notes

Rationale for design choices that aren't obvious from the code.

## GitLab commit counting: pushed vs authored

GitLab's push events credit every commit in a push to the
pusher, regardless of who authored each commit. A single
`git push --mirror` of someone else's repo can therefore produce
a multi-hundred-commit spike in the "pushed" number that doesn't
reflect actual authorship — this bit a sibling tool's users and
is the reason both numbers are surfaced here.

### Data sources

- `commits_pushed` — sum of `push_data.commit_count` from
  `pushed to` / `pushed new` events on the user-activity
  endpoint. Cheap (comes as part of the user-events call we're
  already making).
- `commits_authored` — per-project count from
  `GET /projects/:id/repository/commits?author=<username>&since=…&until=…`.
  One extra API call per project the user pushed to, paginated
  at 100 commits per page.

`count_authored_commits` lives in `sandogasa-gitlab` so other
tools can reuse the primitive.

### Why both, and why no threshold

Early design considered dropping pushes with `commit_count`
above a threshold (e.g. 50) on the theory that "normal work
pushes are small." That was rejected for two reasons:

1. **Thresholds silently discard data.** A legitimate large
   feature-branch push (50+ commits of real work) looks
   identical to a mirror push by `commit_count` alone. Dropping
   either is surprising.
2. **The gap is a clearer signal than a cutoff.** Reporting
   both `pushed` and `authored` lets a reader see mirror
   activity directly: `0 authored / 187 pushed` is unambiguous.
   A threshold would hide that distinction.

Pushed counts also capture something real — who is operationally
moving code in the forge, even if they didn't author it — which
a SIG-wide report might legitimately want.

### Author-filter caveat

The `?author=` parameter on GitLab's commits endpoint matches
against commit author name and email, not the username. Today
we pass the resolved GitLab username and rely on it matching
the commit author name (common case on gitlab.com / salsa).
If a contributor commits under an email only and the server's
filter doesn't match, the authored count will under-report for
them. If that shows up in practice we'd resolve the user's
commit email via `/users/:id` and pass that instead, or filter
client-side after fetching commits.

## User-events endpoint date window

The events endpoint's `after` and `before` parameters are both
exclusive (`>` not `>=`, `<` not `<=`). We widen by one day on
each side and re-clamp with an inclusive in-range check
client-side so the report's `--since`/`--until` behave as
inclusive bounds everywhere else in the tool.

## Per-user config overlay

`sandogasa-report` layers a user-specific overlay at
`~/.config/sandogasa-report/config.toml` on top of the `-c` main
config via deep `toml::Value` merge. The main config is intended
to be checked in and shared; the overlay carries per-user
identities (profile, Bugzilla email, per-instance GitLab
usernames) and credentials (`gitlab_tokens`). Tables merge
recursively, scalars and arrays are replaced wholesale by the
overlay.

The `config` subcommand edits the overlay as a raw
`toml::Value` rather than round-tripping through `ReportConfig`
so unknown keys the user authored by hand survive the write.

## Future work

### Resumable / cacheable report generation

Right now a run executes Bugzilla → Koji → Bodhi → GitLab
sequentially and blocks on every backend. Any of them can be
slow (Bodhi `rows_per_page=500` routinely takes 30–60s, GitLab
authored-commit walks scale with the project count) and a
network blip late in the pipeline wastes everything that ran
earlier.

A per-section result cache keyed by
`(user, since, until, domain, source)` would let re-runs pick
up where the previous one failed. Sketch:

- After each sub-report (`BugzillaReport`, per-domain
  `KojiReport`, etc.) successfully builds, write its JSON to
  `$XDG_CACHE_HOME/sandogasa-report/<hash>/<source>.json`.
- At the top of each section, check for a fresh-enough cache
  entry and skip the network round trips if present.
- `--no-cache` to force a clean run; `--clear-cache` to wipe.
- Consider whether "fresh enough" means "same period, same
  user, within N minutes" or something stricter.

Also related: add a sensible request timeout to the shared
HTTP clients (Bodhi, Bugzilla, GitLab, …) so a hung connection
fails loudly at ~120s instead of blocking forever. Today the
default `reqwest::Client::new()` has no request timeout.
