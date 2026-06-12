# TODO

## Workspace

- Major dependency bumps available (checked 2026-06-12, deferred
  from 0.13.0): reqwest 0.12->0.13 (touches every client crate;
  reqwest::Error appears in public signatures, so it is itself a
  breaking change — bundle with the next breaking release),
  toml 0.8->1.x, toml_edit 0.22->0.25, quick-xml 0.37->0.40.
  Fedora availability (checked 2026-06-12 via fedrq, rawhide +
  f42-f44 + epel9/epel10): reqwest 0.13, toml 1.1, and
  toml_edit 0.25 are shipped on every branch — only our own
  migration work blocks them. quick-xml 0.40 is rawhide-only;
  stable/EPEL branches ship 0.39 with 0.40 updates still in
  Bodhi testing. Re-check before bumping:
  https://bodhi.fedoraproject.org/updates/?search=0.40&packages=rust-quick-xml

## poi-tracker

- `sync-gitlab` analog of `sync-distgit --mark-unshipped`
  (landed 2026-06-12): GitLab-synced packages are CentOS Stream
  projects, so the retirement check needs a GitLab-aware signal
  (archived project? removed from the group?) rather than Fedora
  dist-git's `dead.package`.

## ebranch

- Second-level branch-request escalation: when a `needinfo?` ping
  (the level-1 escalation `escalate` already does) goes unanswered
  for another N days, file a releng ticket. Blocked on a Forgejo
  client — releng's tracker moved from Pagure to Forgejo, which
  sandogasa has no client for yet. Also needs the report's
  per-request escalation state to grow from `pinged: bool` to a
  level (none → needinfo'd → releng-filed) so `escalate` knows
  which step each request is on. Plan: add a `sandogasa-forgejo`
  crate (issue create/search), extend `BranchRequest`, and add the
  releng-filing branch to `escalate`.

## sandogasa-report

- Debug CVE/security bug reporting: the query may be too narrow or
  the keyword filter may not match Bugzilla's actual keyword values.
  Test with known CVE bugs and compare against manual Bugzilla search.

