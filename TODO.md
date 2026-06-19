# TODO

## dbranch

- (2026-06-20) **`update` subcommand** — update the Debian branch
  (`master`/`main`/`debian/unstable`) to a new upstream. A separate
  subcommand (not a `rebuild` mode — "rebuild" reads wrong for a
  new-upstream import) that **shares the build/lint/push/upload/tag
  pipeline**. Do this next; testable when updating a package by hand.
  - Instead of merge: `gbp import-orig --uscan --pristine-tar`, then
    `gbp dch -c -R` for the new-version entry (auto-commits + releases;
    **not** normalized — it's a real new-upstream entry, not a rebuild,
    so no `~codename+N` suffix / synthesized body).
  - Build suite decoupled from the changelog distribution: build
    against **testing** by default (less broken), with a choice of
    **unstable** (deps removed from testing force it). Ubuntu rebuilds
    still build against the codename. `pbuilder-dist <build-suite>`;
    overridable (e.g. `--build-suite`).
  - Upload: on the Debian branch, `dput` with no target works (no
    `--ppa`/`--upload-target` required); `--upload-target mentors` still
    allowed.
  - Out of scope: Debian's new PPA-like repo (try by hand first).
  - Refactor needed: extract the shared post-first-stage pipeline so
    both `rebuild` and `update` drive build→lint→push→upload→tag.

- (2026-06-19) Target-type / version-scheme abstraction — the rest of
  the big piece. Live-test it when updating `archlinux-keyring`'s
  `debian/trixie`. A per-target notion driving `changelog::
  rebuild_version` + `normalize_top_stanza`: Ubuntu PPA `~<codename>+N`;
  Debian backports `~bpoN+M`; proposed-updates `+debNuM`;
  unstable/testing = no suffix. Branch taxonomy → target: `master`/
  `main`/`debian/unstable` → Debian unstable (`dput` default, or
  mentors for a new pkg / proposed NMU); `ubuntu/*` or an Ubuntu-release
  codename → PPA; `debian/<codename>` (e.g. archlinux-keyring's
  `debian/trixie`) → special, kept current in stable. Also rename the
  `codename` value → `distribution` (what pbuilder/gbp/changelog call
  it), keeping "codename" only for the `~<codename>` version suffix.
  The build-suite decoupling means a target has both a changelog
  *distribution* and a *build suite*, on top of the version scheme.

Note (2026-06-19): bulk is deliberately **local-branch only** — a local
branch *is* the opt-in. To include a release, check it out once; to
drop it, delete the local branch. (Remote-inclusive bulk was
considered and rejected.)

Done (shipped): the `push`/`upload`/`tag` stages, per-job CI watch
progress, `git push -u`→plain-push simplification, `--quiet` mode, the
`--source` merge-source override, the `fixup` subcommand, stale-chroot
auto-refresh (`--refresh-chroot`/`--no-refresh-chroot`), grouped
`--help` sections, and the safer bulk run (Ubuntu-codename selection
via `ubuntu-distro-info`, EOL skip + `--include-eol`, newest-first
order, confirmation + `--yes`).

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

