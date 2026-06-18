# TODO

## dbranch

- (done) Add more `rebuild` stages after `lint`:
  - (done) `push` — `git push origin <branch>`, then watch the
    branch's GitLab CI pipeline via the `glab` CLI (auto-detects the
    salsa host/project from the remote). `--nowait` pushes without
    waiting; `dbranch watch-ci [<branch>]` attaches to a live pipeline
    later. Chose `glab` over a `sandogasa-gitlab` pipeline query: it
    handles host/project detection, auth, and watching for free.
  - (done) Wired into the `--stage` selector
    (`merge,build,lint,push` / `all`) in pipeline order.
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

