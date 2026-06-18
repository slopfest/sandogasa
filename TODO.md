# TODO

## dbranch

- (2026-06-18) Refresh a stale pbuilder chroot before building. The
  build stage already creates `~/pbuilder/<codename>-base.tgz` when
  absent; additionally, when it exists but is old, run
  `pbuilder-dist <codename> update` first so the build isn't against
  stale packages. Default staleness threshold ~1 day (use the
  base.tgz mtime; there's already a `plan::pbuilder_base_tgz` path
  helper). Add a mutually-exclusive pair (clap `conflicts_with`):
  `--refresh-chroot` to force a refresh regardless of age, and
  `--no-refresh-chroot` to skip the auto-update entirely and build
  against the chroot as-is. Slots into `build_stage` right after the
  create check, before `debuild` / `pbuilder-dist <codename> <dsc>`.

- (2026-06-18) Group `rebuild`'s flags in `--help` for readability now
  that there are many. clap supports labeled sections via
  `#[arg(help_heading = "…")]` (or `#[command(next_help_heading)]` for
  a run of args); within a section args show in declaration order, so
  this both groups and orders. Proposed groups: Stages (`--stage`,
  `--source`, `--nowait`), Upload (`--ppa`, `--upload-target`), Output
  (`--dry-run`, `--explain`, `--quiet`), leaving `-C/--repo` under the
  default Options. Pure-annotation change.

- (2026-06-18) Bulk run should confirm the branch set before
  processing, and flag/skip EOL releases.
  - A no-argument `dbranch rebuild` (and now potentially a
    remote-inclusive variant, see below) can fan out across many PPA
    branches; before doing real work it should print the resolved
    list and ask for confirmation, so a stray/unwanted branch (e.g. an
    EOL Ubuntu release whose PPA upload would be pointless/rejected)
    isn't rebuilt silently.
  - Add an EOL check per target codename via `ubuntu-distro-info`
    (from the `distro-info` package; e.g. `ubuntu-distro-info
    --supported` / `--unsupported`, or `--days` for the EOL date).
    Add it to the `require_tools` batch when the check runs. Map each
    branch's codename and mark EOL ones in the confirmation list;
    consider defaulting to skipping EOL branches (with a note) and a
    flag to include them anyway.
  - Gating/flags: likely a single-purpose flag to skip the prompt
    (assume-yes) for scripted runs, plus a flag controlling EOL
    handling (skip vs include).
    Per the CLI-behavior rules, the interactive confirm must default
    to the safe choice and must NOT fire in `--json` or when stdin
    isn't a terminal (those keep warn-and-continue / fail-with-remedy
    behavior).
  - Safer selection: bulk should *positively* pick PPA branches (e.g.
    `ubuntu/*`) rather than "every local branch except a few", so it
    can't accidentally process the Debian branch — especially once a
    `--source` override means the Debian branch need not be the
    checked-out one — or unrelated branches like `master`/`main`. The
    prefix is target-type-dependent (Ubuntu PPAs are `ubuntu/*`;
    Debian backports branches differ), so this ties into the
    target-type abstraction.
  - Related scope: bulk runs currently consider only *local* branches
    (see the remote-only handling for explicit targets). A
    remote-inclusive bulk mode would make the confirm + EOL check even
    more valuable, since it would surface every PPA branch on origin.

Done (shipped): the `push`/`upload`/`tag` stages, per-job CI watch
progress, `git push -u`→plain-push simplification, `--quiet` mode, and
the `--source` merge-source override.

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

