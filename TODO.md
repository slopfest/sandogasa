# TODO

## dbranch

- (2026-06-18) Add more `rebuild` stages after `lint`:
  - `push` — `git push` the rebuilt PPA branch(es) to GitLab (the
    `salsa`/origin remote), respecting `--explain`/`--dry-run`.
  - `ci` — after pushing, poll the branch's GitLab CI/CD pipeline and
    report whether it passes (reuse `sandogasa-gitlab`; needs a
    pipeline-status query, which that crate doesn't have yet).
  - Wire these into the `--stage` selector (e.g.
    `merge,build,lint,push,ci` / `all`) in pipeline order.
  - (done) `lint` — `lintian` on the built source package, warns
    only.

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

