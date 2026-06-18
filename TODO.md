# TODO

## dbranch

- (2026-06-18) Allow an explicit merge source branch (e.g. `--source
  <branch>`), defaulting to the checked-out branch. Today the source
  is always `git::current_branch`; let the user override it so dbranch
  can run without first checking out the Debian branch (handy when a
  different branch is checked out). The source only needs to be a valid
  ref (merge and checkout-new take any ref), so this is mostly
  threading a resolved `source` through from a flag instead of
  `current_branch`, keeping the `target == source` guard and bulk
  exclusions on the resolved source.

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

- (2026-06-18) Watch should report per-job progress, not just the
  pipeline-level state. `watch_pipeline` already has the pipeline id
  (from `glab ci list --sha`); additionally poll its jobs
  (`glab api "projects/:id/pipelines/<id>/jobs" -F json` →
  `[{name, stage, status}]`), diff against the previous poll, and
  print each job as it reaches a terminal state (e.g. "✓ build source
  (build)"). Reuses the existing `serde_json` parsing. Preferred over
  `glab ci status`'s table, which is branch-scoped and reprints the
  whole list each time.

- (2026-06-18) Simplify the push command when upstream is already set.
  The push stage always runs `git push -u origin <branch>`; once the
  branch tracks `origin/<branch>` the `-u origin <branch>` is
  redundant. Detect tracking (`git rev-parse --abbrev-ref
  <branch>@{upstream}` succeeds) and, since the push stage has the
  target checked out, fall back to plain `git push` (or `git push
  <branch>`), reserving `-u origin <branch>` for the first push. Fits
  the learning-tool goal of showing the minimal correct command for
  the current state.

- (2026-06-18) New `upload` stage: `dput` the built package to its
  archive. Name it `upload`, not `publish` — "upload" is the idiomatic
  Debian term (dput's whole purpose) and works for both the Debian
  archive and a PPA; "publish" connotes apt-repo publishing
  (reprepro/aptly), a different operation.
  - PPA: take the PPA name via a single-purpose flag, e.g.
    `--ppa <user/name>`, tolerating but not requiring a `ppa:` prefix
    (strip it if present), then run
    `dput ppa:<name> ../<pkg>_<version>_source.changes` — the source
    `.changes` from `debuild -S`, epoch stripped (add a
    `changes_filename` plan helper next to `dsc_filename`).
  - Debian: also needs a dput target (default from dput config, or
    ftp-master / mentors). Consider a general `--upload-target` with
    `--ppa` as PPA sugar, or detect a `ppa:` prefix; settle the
    surface per the CLI-flag conventions (single-purpose, name the
    real object).
  - Add `dput` (the `dput` / `dput-ng` package) to the `require_tools`
    batch for this stage.
  - Pipeline order: after `build`/`lint`. Decide its place vs `push`
    (git push + CI watch) — likely push (let CI pass) then upload, or
    keep upload independent. Pairs with the bulk EOL-check TODO:
    uploading to an EOL release is rejected.

- (2026-06-18) New `tag` stage: `gbp tag` the release. Make it its own
  stage (not folded into `upload`) — distinct operation, fits the
  selectable-stage model, lets you tag without uploading. Order it
  after `upload` so only an actually-released build is tagged (a failed
  upload leaves no tag).
  - `gbp tag` refuses on an unclean work tree, and `debuild -S` (the
    build stage) leaves a generated `debian/files`. So the stage first
    cleans the tree — `dh clean` (≡ `debian/rules clean`) — then runs
    `gbp tag`. (`gbp tag --ignore-new` is a lazier alternative, but
    actually cleaning matches the by-hand workflow and is tidier.) The
    clean only drops in-tree build cruft, not the `../*.changes` the
    upload stage consumed, so ordering upload-then-tag is fine.
  - Add `dh` (debhelper) to the `require_tools` batch for this stage.
    The tag name follows gbp's `debian-tag` format (default
    `debian/<version>`), so PPA rebuilds get a per-rebuild tag.

- (2026-06-18) Quiet mode that swallows shelled-out tool output.
  A `--quiet`/`-q` flag suppressing the chatter from git, debuild,
  pbuilder-dist, lintian, glab, etc., leaving only dbranch's own
  narration/headings and errors. Implementation: thread a `quiet`
  flag into `Ui` and have `run_status`/`run_required` redirect the
  child's stdout/stderr (capture and show only on failure, like
  `run_capture` already does, rather than discard outright — so a
  failure is still diagnosable). Note the tension with the
  learning-tool transparency: `--quiet` is the opposite end from
  `--explain`, and the two should be mutually exclusive (or `--quiet`
  ignored under `--explain`/`--dry-run`).

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

