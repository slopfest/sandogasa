# TODO

## hs-relmon

- Prune CBS builds for archived-upstream packages, driven by the
  inventory's `archived_builds` markers (set by
  `sync-gitlab --mark-unshipped`). For each marked package, untag
  builds older than the version in stock CentOS Stream
  (c9s/c10s, for the `Ns` Stream tags) or AlmaLinux (al9/al10,
  for the bare-`N` RHEL tags); if a marked package's build is
  *newer* than stock, prompt for confirmation before untagging
  (the archived repo may still be the only source). Needs the
  stock-version lookup per release channel.

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

