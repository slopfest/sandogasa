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

## `username=` includes group-derived access and (by default) forks

Two more semantics of `/api/0/projects?username=<name>` worth
remembering (verified live 2026-06-12):

- The filter matches **any** access path, including membership in
  groups on the project's ACLs. A user in a large SIG (e.g.
  `go-sig`, ~2000 `golang-*` packages) gets all of the SIG's
  packages back. There is **no server-side "direct ACLs only"
  parameter**, so `sync-distgit --no-groups` necessarily pages
  through the group-derived results and filters them client-side.
- Without `fork=false`, the listing also includes the user's
  **forks** — and a fork is reported under its bare package name
  with the user as `owner`, indistinguishable from really owning
  `rpms/<pkg>` unless you read `fullname` (which our model
  doesn't). `user_projects()` therefore always passes
  `fork=false`. Before this fix, fork-only packages leaked into
  synced inventories as direct/owner entries; a re-sync with
  `--prune` cleans them out.

## The three ways to answer "what does this user maintain?"

This keeps causing confusion, so here is the full trade-off space
(measured live against dcavalca, 2026-06-12):

| Method | Cost | Coverage |
|--------|------|----------|
| `/api/0/projects?username=` prefix scan (`sync-distgit --user` default) | ~36 queries; downloads **every** group-derived package only to discard them under `--no-groups` (dcavalca: 1934 results for `g*` alone, 1837 via go-sig etc., 97 direct) | exact — all direct levels (owner/admin/commit/collaborator/ticket) and group access |
| `/extras/pagure_owner_alias.json` (`user_packages_fast`, `sync-distgit --fast`) | **1 request, ~3 MB, ~1 s** | direct **owner/admin/commit only** — collaborator and ticket grants are absent (dcavalca: 93 of his 97 direct `g*` packages; the 4 missing were all collaborator grants), and no group data at all |
| `/api/0/user/<name>` | n/a | returns HTTP 500 for prolific users; also only covers owned repos |

There is no exact-and-fast option: the API has no server-side
"direct ACLs only" filter (so `--no-groups` must filter
client-side after downloading everything), and the owner-alias
dump is the only single-request source but doesn't track
collaborator/ticket. Practical guidance: use `--fast` for routine
refreshes and the full scan occasionally to true up — and beware
that `--prune --fast` removes collaborator/ticket-granted packages
from an inventory that the full scan had added.
