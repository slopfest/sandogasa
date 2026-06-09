# sandogasa-distgit Development Notes

## Pagure per-user project queries are unusably slow (as of 2026-06)

Fetching "all RPM packages a *user* has access to" from
`src.fedoraproject.org` (Pagure) has no fast path. Both user-scoped
endpoints fail at Fedora scale:

- `GET /api/0/user/<name>` returns **HTTP 500** for prolific users
  (observed for `salimma`). It also only reports owned/forked repos,
  not `commit`/`collaborator`/`ticket` ACL membership, so it would be
  the wrong data even if it worked.
- `GET /api/0/projects?namespace=rpms&username=<name>` — the endpoint
  `user_projects()` actually uses — returns **HTTP 504** when
  unfiltered: the query is too expensive for Pagure to answer within
  its (~60s) gateway timeout. Adding a `pattern=` filter (e.g. `a*`)
  shrinks the result set enough to return in a few seconds.

**What we do and don't know.** The two facts above are *observed*
(the HTTP status codes). We do **not** know Pagure's internal reason
for the slowness. Earlier docs asserted it "scans every project's
ACLs server-side" and that there is "no cheaper API that covers all
ACL types" — both overstated what we actually verified. Treat the
internal cause as unknown; only the observed behaviour is reliable.
(See the v0.12.1 erratum in the repo `CHANGELOG.md`.)

**Workaround.** `poi-tracker sync-distgit --user` defaults to
scanning one name prefix at a time (`a*`–`z*`, then `0*`–`9*`) via
the `pattern=` filter and merging the results. `list_branches()` and
the per-branch `is_retired()` calls are unaffected — they hit
cheap per-package endpoints.

**Why group syncs need no workaround.** Group syncs use
`GET /api/0/group/<name>?projects=true`, which reads the projects
already attached to the group record — a bounded, indexed lookup
that returns quickly regardless of how many packages the group is
on. The asymmetry is endpoint-level, not something the client can
paper over: there is simply no equivalent fast "projects for this
user" lookup.
