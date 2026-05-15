# sandogasa-github Development Notes

Rationale for design choices that aren't obvious from the code.

## Why one `PullRequest` type, not two

GitHub exposes pull-request data through two distinct endpoints:

- **Search Issues** (`GET /search/issues?q=type:pr+...`) — used
  by `Client::search_pull_requests`. Returns a narrower record:
  number, title, state, an optional `pull_request.merged_at`
  block, `html_url`, `repository_url`. No `head`/`base` refs,
  no `mergeable_state`, no `requested_reviewers`.
- **Pull Requests** (`GET /repos/{owner}/{repo}/pulls/{n}`) —
  not used here yet. Returns the full PR document.

The current `PullRequest` struct is shaped around the Search
Issues response because that's all this crate fetches. The
alternatives were:

1. Make `PullRequest` carry every field from `/pulls/{n}` as
   `Option<T>` so the same type serves both endpoints. Rejected
   — callers can't tell from the type which fields are
   populated when, and unused `Option<T>` fields rot.
2. Define both `SearchedPullRequest` (Search-Issues-shaped) and
   `PullRequest` (full) up front. Rejected — we don't fetch the
   full form today, so the second type would be dead code.

If a future caller needs the fuller model, the right move is to
rename today's struct (e.g. to `SearchedPullRequest`) and add
the richer `PullRequest`, not to extend this one. Search results
have a stable shape that's worth keeping its own type.

## `validate_token` three-state return

`validate_token` returns `Result<bool, Box<dyn Error>>`:

- `Ok(true)` — token authenticates
- `Ok(false)` — server returned 401 (token is invalid)
- `Err(...)` — anything else (network, 5xx, parse failure)

`sandogasa-gitlab::validate_token` collapses 401 and transport
errors into a single error path, which is fine for that
tool's "set up once, then use" flow. Here, `sandogasa-report
config` re-validates persisted tokens on every run and we want
to keep "the saved token has been revoked" distinct from "I
couldn't reach the server" — only the first should prompt for
a new token; the second should fail the run loudly. Hence the
extra discriminator.

## `count_authored_commits` swallows 404/409

A run-time fetch can hit a repo that's been deleted (404) or
that's empty (409) between the events scan and this call. The
function treats both as "0 commits" rather than an error so a
single gone repo doesn't abort the whole report. Real auth
failures targeting a single repo come back as 401/403 and still
surface as errors.

## `user_events` caps at 3 pages of 100

GitHub serves at most the user's 300 most recent public events
via `/users/{user}/events`. Asking for page 4+ returns an empty
list. The 3-page cap is hardcoded with a comment so readers
don't try to "fix" it by paginating further — there's nothing
behind page 3. If GitHub ever raises the limit the cap should
move; until then the current ceiling matches the documented
behaviour.

## Why blocking, not async

`sandogasa-gitlab` is blocking; this client matches it so a
single sandogasa-report run can drive both forges through the
same per-instance loop without an async boundary in the middle.
`sandogasa-bodhi` is async because it predates that pattern. If
the async story consolidates later, both clients can move
together.
