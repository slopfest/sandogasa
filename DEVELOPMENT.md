# Development Notes

Cross-cutting gotchas for the whole workspace. Crate-specific notes
live next to the crate (e.g. `crates/sandogasa-distgit/DEVELOPMENT.md`,
`crates/sandogasa-fedrq/DEVELOPMENT.md`).

## Fedora infrastructure is flaky — code defensively

Fedora services (src.fedoraproject.org / Pagure, Bodhi, Koji,
Bugzilla, the mirror network) routinely return transient 5xx errors
or drop connections under load. Any client code talking to them must
assume a request can fail once and succeed on an immediate retry.
Concretely:

- **Retry transient failures on GETs.** Retry 500/502/503/504 and
  transport errors (connection reset, timeout, DNS) with backoff.
  `sandogasa-distgit` has `get_transient_retry`/`get_with_retry` for
  this; reuse or replicate the pattern in other HTTP clients.
- **Only a 404 means "does not exist".** Never fold "the request
  failed" into "the resource is absent": an existence check that maps
  any non-2xx status to `false` turns a Pagure hiccup into a
  confidently wrong answer. Observed live (2026-07):
  `sandogasa-pkg-acl set --user mikelo2 --level commit yq` reported
  "user 'mikelo2' does not exist on dist-git", and the identical
  rerun seconds later succeeded — the check had mapped a transient
  error to `false`.
- **Word errors as "check failed", not as a negative result.** Prefer
  "could not verify user 'x' exists on dist-git: 502 Bad Gateway"
  over "user 'x' does not exist", so users don't act on phantom
  state.
- **Don't blanket-retry mutating requests** (POST/PATCH): the failed
  request may still have taken effect server-side. Surface the error
  and let the user rerun. (Our ACL modifications happen to be
  idempotent, but don't assume that in general.)
