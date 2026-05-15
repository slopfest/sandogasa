# sandogasa-report TODO

Deferred work that's been considered but not yet built. See
`DEVELOPMENT.md` for design rationale on what *is* shipped.

## GitHub pushed-vs-authored commit counting

The GitLab integration reports two commit numbers per project:
`commits_pushed` (sum from push events; credits the pusher,
including mirror pushes of others' work) and `commits_authored`
(from a per-project commit query filtered by author). The gap
between the two flags mirror activity at a glance.

GitHub deliberately ships only `commits_authored` for v1.
Reasons it's deferred:

- GitHub's `PushEvent` payload includes a `commits` array with
  full commit metadata, but the array is capped at 20 commits
  per push regardless of how many were actually pushed. The
  per-event count (`distinct_size` / `size`) is the only
  reliable source for total commits-pushed, and it doesn't
  decompose by author.
- GitHub's user-events endpoint is capped at the user's 300 most
  recent public events. For an active contributor we already
  burn that budget on PR/issue events; aggregating push counts
  on top would be noisier than useful.
- The phenomenon GitLab's dual count catches — `git push
  --mirror` of someone else's repo crediting every commit to
  you — is much rarer on GitHub because GitHub's UI emphasises
  PRs over direct pushes, and mirroring tools more often go
  through dedicated bot accounts.

If users do start hitting the mirror-credit problem on GitHub,
the natural fix is to surface `commits_pushed` next to
`commits_authored` in `GithubReport` (matching the GitLab data
shape) and update the formatter to render both, gated behind a
`--show-pushed` or similar so the noise stays opt-in.

## GitHub reviewed/commented capped at 300 events

Reviewed and commented PRs come from `/users/{user}/events`,
which serves at most the user's 300 most recent public events.
For a heavy reviewer over a long reporting window this can
silently under-report — events that fell out of the 300-event
sliding window before we fetched aren't recoverable.

The Search API alternative (`reviewed-by:`/`commenter:` with
`updated:` filter) over-reports the opposite way: it surfaces
any PR the user has ever reviewed/commented on that had any
update in the window, even by someone else. We picked
under-reporting via events because the failure mode is more
honest — you can tell when you're missing data by looking at
the report scope — whereas the Search false positives blend
in with real activity.

The path back: for each repo the user pushed to (we already
have these from PushEvents), additionally pull
`/repos/{owner}/{repo}/issues/comments?since=<date>` and
filter by author. That captures comments without the 300-event
ceiling, at the cost of one extra paginated request per
touched repo.

## Resumable / cached report generation

See DEVELOPMENT.md for the original write-up. Still applies —
both forges now hit external APIs sequentially and a late
failure wastes earlier successful fetches.
