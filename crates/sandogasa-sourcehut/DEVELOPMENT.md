# sandogasa-sourcehut — development notes

Rationale and landmines for the sr.ht GraphQL client.

## Fetching sr.ht data: shell out to `curl`

sr.ht **UA-blocks some automated fetchers** — the assistant's built-in web
fetcher gets `502 Bad Gateway` from git.sr.ht's web UI. A plain `curl`
works fine (confirmed). So to pull schemas or poke the API by hand, use
`curl`, not a browser-simulating fetcher:

```sh
curl -sS https://git.sr.ht/~sircmpwn/git.sr.ht/blob/master/api/graph/schema.graphqls
```

The vendored schemas under `schema/` are refreshed by
`scripts/update-srht-schemas.sh` (in the repo root), which curls the raw
`schema.graphqls` for each service. They're **reference only** — not build
inputs; nothing compiles against them.

## API conventions

- **One GraphQL endpoint per service:** `https://<service>.<host>/query`
  (`git`, `todo`, `lists`; the client derives the subdomain from the
  configured host). Requests are `POST {query, variables}` JSON.
- **Auth:** `Authorization: Bearer <token>` — a personal access token from
  `meta.sr.ht/oauth2/personal-token`, which by default grants read access
  to all services (so one token covers git + todo + lists).
- **Pagination:** cursored resolvers return `{results, cursor}` (25/page);
  pass `cursor` back until it is `null`.
- **Time scalar:** RFC3339 UTC (`%Y-%m-%dT%H:%M:%SZ`). These sort
  lexicographically, so string comparison is a valid date comparison —
  the pagination "stop once older than `since`" loops rely on this.
- **Interface fields (`Event.changes`)** are queried with inline
  fragments (`... on Created { author ... }`); the JSON for each element
  then carries `eventType` plus whichever fragment's fields matched, so a
  single struct with `Option` fields deserializes all variants.

## Metric-mapping caveats

- **todo tickets are token-owner-only.** The opened/closed counts come
  from `Query.events` (the *authenticated* user's feed), because `User`
  only exposes the user's *own* trackers — which would miss tickets filed
  on others' trackers. So ticket metrics populate only when reporting on
  the token owner; for a report on a different user they stay empty
  (patches and commits still work for any user).
- **git commit attribution is limited by the single exposed email.**
  Only the user's *own* repositories are enumerable (`user.repositories`),
  and meta.sr.ht exposes only the account's *primary* email (`user_email`)
  — there's no secondary-emails list. So this crate returns every commit
  (via `commits_since`, which pages a repo log and stops once a commit's
  committer time predates the window) and leaves owner-vs-third-party
  attribution to the caller, which matches the author email against the
  primary plus any extra emails the user configures (see
  `sandogasa-report`'s `DEVELOPMENT.md`).
- **Patchset `status`** gives a proposed-vs-applied split (`APPLIED` ≈
  merged), a reasonable analog to opened-vs-merged PRs.
