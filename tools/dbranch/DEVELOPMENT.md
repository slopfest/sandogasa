<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# dbranch — development notes

Maintainer-facing notes: the external-tool landmines we hit and the
design decisions behind the code. User docs are in `README.md`; the
`--explain`/`--dry-run` contract and coding rules are in `CLAUDE.md`.

## External-tool quirks

dbranch shells out to a lot of Debian/Ubuntu tooling. Several tools do
**not** behave as you'd assume — verify before trusting.

### Availability probes (`git::ensure_tools` → `sandogasa_cli::require_tools`)

Each tool is `(exe, install-hint, Option<probe-arg>)`: `Some(arg)` runs
`exe arg` and requires exit 0; `None` is a bare `$PATH` check. Not every
tool answers `--version`/`--help` with exit 0:

| tool | probe | why |
| --- | --- | --- |
| `git`, `gbp`, `debuild`, `lintian`, `glab` | `--version` | fine |
| `pbuilder-dist` | `--help` | `--version` exits 1 |
| `dh` | `None` (PATH only) | `--version` *and* `--help` exit non-zero |
| `dput` | `None` (PATH only) | `--version` support varies (dput vs dput-ng) |
| `ubuntu-distro-info` | n/a | called directly; errors with an install hint |
| `debian-distro-info` | n/a | `--series=<c> -r` → Debian major; for `debian/` targets only |

### pbuilder-dist + sudo under `--quiet`

`pbuilder-dist` (the `build` stage) runs `sudo` to manage/enter the
chroot. `--quiet` captures the command's I/O via `Command::output()`
(null stdin), so a `sudo` password prompt can't be answered and the
build hangs/fails. Document it (README): `--stage build --quiet` needs
passwordless `sudo` for `pbuilder-dist`, or a pre-authenticated `sudo`
timestamp in the same session.

### glab (CI watch + auth)

- **`glab ci status` is interactive** and `--live` needs a TTY (it
  degrades to a single snapshot otherwise, and pipes a `Choose an
  action` menu that blocks). So we **poll `glab ci list --sha <commit>
  -F json`** instead — targeting the exact pushed commit dodges the
  post-push race (where the branch's latest pipeline is still the
  previous commit's). Per-job progress polls `glab api
  projects/:id/pipelines/<id>/jobs`. glab is spawned with stdin on
  `/dev/null` as a backstop against prompts.
- **`glab auth status` misreports** on older glab (1.53 flags a valid
  token as "Invalid token provided"). Auth is also **per host** (token
  stored per host in glab's config). We capture its output and only
  surface it on failure, and scope the precheck to the repo's host.

### gbp

- **`gbp dch` refuses unless you're on gbp.conf's `debian-branch`**
  ("you are not on branch '<x>'"). So the merge stage sets gbp.conf's
  `debian-branch` to the PPA branch itself (via `adjust_branch_packaging`)
  **before** `gbp dch`, for existing branches too — not just new ones.
  We pass `--spawn-editor=never` and normalize the generated stanza
  deterministically afterward (don't depend on gbp's exact output).
- **No `debian/gbp.conf` at all? Create one on the rebuild branch.** When
  the rebuilder isn't the maintainer, the Debian branch is often kept
  clean (no `debian/gbp.conf`) so it can be contributed upstream. With no
  gbp.conf, `gbp dch` defaults `debian-branch` to the Debian branch and
  refuses on the PPA branch. `adjust_branch_packaging` now *creates* a
  minimal `debian/gbp.conf` (`debian-branch` = this branch, `debian-tag` =
  the branch-namespace format) when absent, committed on the rebuild
  branch only — the Debian branch stays clean. Kept minimal on purpose:
  plumbing keys (`upstream-branch`, `pristine-tar`) are left to gbp's
  defaults / `~/.gbp.conf`, not guessed. A brand-new file needs
  `git add` before `git commit <file>` sees it, so the create path stages
  first and shows the staged diff (`explain_diff_cached`).
- **`gbp dch`'s body is discarded, not used.** After a merge it lists
  the *entire merged Debian delta* (every commit), and `--since` can't
  cleanly scope to "PPA-side changes since the last rebuild" (it either
  includes the delta or accumulates old rebuild commits). So
  `normalize_top_stanza` **synthesizes** the body: `* Rebuild for
  <codename>` plus, when `adjust_branch_packaging` touched packaging
  files this run, a `* Create <files> for <codename>` line for files
  created from scratch and/or a `* Adjust <files> for <codename>` line
  for files edited (matching the per-file commit messages). Discarding
  gbp's body also drops any `UNRELEASED` it added.
- **`gbp dch --bpo` quirks (backports targets):** it puts a trailing
  period on its generated `Rebuild for <dist>.` line, and its body
  content depends on what commits exist since the branch point (a
  gbp.conf commit shows up as a bullet; with *no* commits it may leave
  an `UNRELEASED` distribution instead). All of that is provisional —
  normalization synthesizes the clean period-less entry regardless. A
  backports branch's gbp.conf only needs `debian-branch` (it lives in
  the `debian/` namespace, so gbp's default `debian/%(version)s` tag is
  already correct — don't set `debian-tag`).
- **`gbp import-orig` prompts for the upstream version** when it can't
  be sure of it (e.g. a `0~`-mangled date version like `0~20260612`,
  where the upstream tag is the bare date) — `What is the upstream
  version? [<guess>]`. dbranch runs gbp with a **null stdin** (via
  `Command::output()`), so that prompt hits `EOFError` and aborts the
  whole import. The fix is **`--no-interactive`**: gbp then uses its own
  guessed version (from the uscan tarball name, which is correct) without
  asking. Same flag also stops it prompting for the package name.
- **`gbp import-orig` refuses to re-import** an already-imported
  upstream: with `--no-interactive` it reaches the tag check and aborts
  with `Upstream tag 'upstream/<v>' already exists`.
  `plan::import_already_done` matches that message specifically — **not**
  bare `already exists`, because a failed download also reports `Failed
  to download …: … already exists`, which is a real error we must not
  paper over. The `update` flow self-heals the tag case: it captures
  import-orig's output and, if `plan::import_already_done` matches the
  `already imported` phrase, treats the refusal as success and falls
  through to `gbp dch` — so a run that imported the upstream but died
  before writing the changelog (e.g. a bad `gbp dch` that got reverted)
  recovers on a plain re-run instead of dead-ending. The phrase match is
  the version-dependent bit; it's a deliberate exception to the
  "normalize, don't parse output" rule because gbp offers no
  machine-readable signal, and any other failure still propagates.
- **`gbp tag` refuses a dirty tree**, and `debuild -S` leaves a
  generated `debian/files`, so the `tag` stage runs `dh clean` first.
- The codename is taken from the **branch name** (`codename_from_branch`),
  not gbp.conf's `debian-branch` — an unadjusted branch may still point
  that at the Debian branch.

### dput over sftp (paramiko)

dput-ng's sftp uploader uses paramiko, which **reads**
`~/.ssh/known_hosts` but its trust prompt does **not save** accepted
keys — so it re-prompts every run — and it **ignores** ssh_config's
`StrictHostKeyChecking` (so `accept-new` won't help). Fix: seed the key
once with `ssh-keyscan <host> >> ~/.ssh/known_hosts`. Under `--quiet`
the captured dput has no stdin, so an un-seeded host fails the stage
(rather than hanging). dbranch does not auto-accept host keys.

### dput to unstable needs a Debian host

`update`'s upload stage `dput`s to the Debian archive's default target
(unstable). This works **only on a Debian host** — Ubuntu's `dput` does
not understand the `unstable`/Debian-archive target — so the upload
stage must be run from a Debian environment (import/build/lint are fine
on Ubuntu). The host check (`host::is_debian`) gates the whole proposed-update flow
(see the rebuild target-type note above) and, for `update`, the
default-target upload: `update` hard-fails early on a non-Debian host
when the `upload` stage is selected with no `--upload-target`.
`import`/`build`/`lint` run fine on Ubuntu (only the upload is gated,
unlike proposed-updates where the whole flow is), an explicit
`--upload-target` is exempt, and a `--dry-run` is exempt.

### Launchpad / EOL

Launchpad rejects uploads to an **EOL series' PPA**, so bulk
`--include-eol` is local-only and is rejected together with the
`upload` stage.

### Launchpad PPA pre-flight (upload safety)

Before a PPA upload, `ppa_preflight` queries the Launchpad API via
`curl -sfG …getPublishedSources&source_name=<pkg>&exact_match=true` and
reads `total_size` (`plan::published_source_count`). `> 0` → already in
the PPA, proceed; `0` → not there yet; a non-zero curl exit / unparseable
body (a typo'd PPA name 404s, and `-f` makes that a clean failure) →
can't verify. The last two confirm before uploading (`confirm_default_no`)
— catching a wrong `--ppa`, SSH-known-hosts style; a real first upload is
confirmed once. Only `ppa:` targets are checked (`plan::ppa_owner_name`
returns `None` for a dput host or the default target). The prompt is
interactive-only: `--yes`/non-tty warns and proceeds, `--dry-run` narrates
the `curl`. A missing `curl` (`tool_exists`) skips the check rather than
blocking — it's a safety nicety, not essential to the upload, so it isn't
in `ensure_tools`.

### `source` is a stage of its own (`debuild -S` vs pbuilder)

`source` (`debuild -S`) and `build` (`pbuilder-dist`) are separate
stages, like `fedpkg srpm` against `fedpkg mockbuild`. They produce and
consume different things: `build` scratch-builds the `.dsc`, `upload`
dputs the `.changes`, and the source package can need rebuilding —
because the tree moved on, or because the destination needs the orig
tarball — without the chroot build being redone. Keep the source build
out of `build_stage`; `all` runs both, so the everyday path is
unchanged.

### What the source stage puts in the `.changes` (`-sa` / `-si`)

`-si`/`-sa`/`-sd` are **`dpkg-genchanges`** options and decide what the
`.changes` *offers for upload*. They do not touch the `.dsc`, which
references the orig tarball either way, so they never affect the local
pbuilder build. Read them from `dpkg-genchanges(1)`, not
`dpkg-buildpackage(1)`: the current rule for `-si` is "include the orig
if the upstream version differs from the **previous changelog
entry's**", not the older "Debian revision is 0 or 1" wording.

`orig_required` decides, and the question is **whether the destination
is backed by the Debian archive's pool** — not which suite it is:

- dak resolves a file the `.dsc` names but the `.changes` doesn't offer
  by looking it up **pool-wide** (`_add_dsc_files` → `get_file` by
  filename, then `_copy_file` into the target archive/component). An
  upload whose orig is already in the archive with a matching checksum
  is accepted without it.
- Unstable, proposed-updates **and backports** are all that one pool.
  Backports has been in the main archive, going through the regular
  upload queue with packages in the regular pool, since
  wheezy-backports in 2013 — the separate backports archive and its
  special dput host are long gone. All three get `-si`.
- Anywhere with no such pool to fall back on gets `-sa`: a PPA,
  mentors, a Debusine personal repository. A PPA is the sharp case —
  the rebuild version reuses the upstream version, so `-si` omits the
  tarball and Launchpad rejects an upload for a file it can't find.
- A **security** upload is the same rule, not an exception to it: the
  developer's reference says to build `-sa` because security.debian.org
  is its own archive, so it isn't that pool either. Reaching it via
  `--upload-target` lands in the `-sa` arm on its own.

Do **not** "play it safe" by forcing `-sa` everywhere: an orig that
doesn't match the archive's byte for byte is itself a reject, so
including a locally regenerated tarball where the archive already has
one adds a failure mode rather than removing one.

Watch the reasoning, not just the conclusion, if this is revisited:
"backports needs `-sa`" is widespread advice that was true before 2013
and is repeated in plenty of still-published guides.

**Confirmed against real archives**, both on 2026-08-13: a `-2`
revision uploaded to unstable with no `.orig.tar` line in its
`.changes` was accepted by ftp-master, so dak does resolve the tarball
from the pool as the `-si` arm assumes; and a `resolute` PPA rebuild
built `-sa` was accepted by Launchpad, exercising the `-sa` arm and the
source/`ensure_source`/upload flow end to end. What that establishes is
that what dbranch sends is accepted — not that `-si` would be refused
by a PPA, which is the claim the `-sa` arm rests on and which nothing
here tests.

**Not documented, reasoned:** mentors and Debusine. Neither's docs
mention `-sa` or the orig tarball — the `-sa` there follows from the
repository having no Debian pool to resolve against, so a sponsor
`dget`ing the `.dsc` would find a file that isn't there. Confirm
against a real upload before treating either as settled.

Both flags are passed explicitly rather than letting `-si` be implicit,
so the narrated command records what the run decided — the tool teaches
the command, and an invisible default teaches nothing.

### The source package a stage is about to consume (`ensure_source`)

Stages are independently selectable, so `--stage build` or `--stage
upload` can find a source package built from a tree that has since
moved on — or none at all. `source_state` classifies what is on disk
before either stage spends anything on it, and `ensure_source` offers
the source stage as the fix. Rules for anything touching this:

- Compare mtime against the **committer** date (`git log -1
  --format=%ct`, `git::commit_time`), not the author date: the question
  is when this tree state came into being, and an amend or a rebase
  makes the author date older than the files legitimately are. Strict
  `<`, so a build in the same second as the commit — the normal order —
  never reads as stale.
- Uncertainty must not invent work: an unreadable mtime, an
  unresolvable HEAD, or a `.changes` that can't be read all count as
  `Ready`. `changes_includes_orig` is deliberately loose for the same
  reason — its false positives leave the upload alone.
- Only the `.changes` is stamped for staleness. `debuild -S` writes it
  together with every file it lists, so it stands for all of them;
  don't parse the `Files:` stanza to check each one's mtime.
- **Missing and stale are different questions.** Stale (or orig-less)
  is a refresh: default yes, and declining or running non-interactively
  carries on with what is there. Missing is a sequencing mistake:
  default **no**, and a non-interactive run *fails*, naming the stage
  to run. An automated workflow that reaches `upload` without a source
  package has its stages in the wrong order, and quietly building one
  hides that; `--yes` is still an explicit yes to both.
- Skip the check entirely when the source stage ran in the same
  invocation — the artifact is fresh by construction.

### Debusine uploads (`--debusine`)

Uploading to a [Debusine personal
repository](https://wiki.debian.org/DebusineDebianNet#Repositories) is
still a `dput` upload, steered by two profile overrides:

```
dput -O debusine_workspace=r-<name>-<srcpkg> \
     -O debusine_workflow=publish-to-<suite>-<srcpkg> \
     debusine.debian.net <pkg>_<ver>_source.changes
```

Landmines and decisions:

- **The dput profile ships in `debusine-client`, not dput-ng** —
  `/usr/share/dput-ng/profiles/debusine.debian.net.json` (admin
  overrides live under `/etc/dput.d/`). Without the package, dput
  fails with an unknown-host error. Its defaults target the shared
  `developers` workspace with an `upload-to-{distribution}` workflow
  derived from the `.changes` distribution; the two `-O` overrides
  redirect that at the personal repository. The upload triggers
  workflows through the Debusine API, which needs the token `debusine
  setup` writes to `~/.config/debusine/client/config.ini`.
  `ensure_debusine_ready` pre-flights both files before any expensive
  work.
- **The workflow suite is the *base* release**, not the changelog
  distribution: a trixie backport (`~bpo13+1`, distribution
  `trixie-backports`) publishes via `publish-to-trixie-<srcpkg>` — the
  wiki's own example — and the Debian branch (unstable) via
  `publish-to-sid-<srcpkg>`. For rebuild targets this is the same value
  as the pbuilder build suite (`upload_dest_for` reuses
  `build_suite_for`).
- **The workspace/workflow embed a project name — by default the
  source package**, composed at upload time from the changelog
  (`r-<name>-<srcpkg>`); `--debusine` takes only the owner name, so it
  works across packages (and parsing a full workspace string back
  apart would be ambiguous — owner names may contain hyphens, e.g.
  `michel-slm`). `--debusine-project` overrides the project part for a
  shared workspace hosting several packages (Debusine's
  `r-YOURNAME-PROJECTNAME` naming doesn't require one workspace per
  package); it replaces `<srcpkg>` in **both** the workspace and the
  `publish-to-<suite>-*` workflow name.
- **Debian targets only**: debusine.debian.net hosts Debian suites, so
  `--debusine` is rejected for Ubuntu PPA targets and bulk runs (bulk
  selects PPA branches). The Debian-host guards are unchanged —
  debusine-client is a Debian package, and for `update` the `--debusine`
  upload is gated like the default-target upload.

### salsa-ci

salsa-ci builds against **Debian** (`RELEASE`), not Ubuntu (it doesn't
speak Ubuntu), and **defaults to sid** when `RELEASE` is unset — so
every rebuild target type needs a pin. The new-branch adjustment sets
`RELEASE` per type: `"unstable"` plus backports-style relaxations for a
PPA; the codename for a proposed-update; `"<codename>-backports"` for a
backport (an officially supported RELEASE whose image also enables the
backports apt repo) — the latter two with no relaxations. Keys are only
added when absent: a maintainer may pin `RELEASE` to an older Debian
suite for an old Ubuntu LTS (better signal), and that is left untouched.

- **No `variables:` block? Create one.** The current upstream template is
  just an `include:` of `recipes/debian.yml` (a single line — it replaced
  the older two-line `salsa-ci.yml` + `pipeline-jobs.yml` include) with no
  `variables:` block at all. `adjust_salsa_ci` used to bail ("unexpected
  format") in that case; it now appends a fresh `variables:` block instead
  of only extending an existing one.

### ubuntu-distro-info

`--all` lists codenames in release order (oldest first); bulk uses that
to process **newest release first**. `--supported` (includes the devel
release) is the not-EOL set; the complement within `--all` is EOL.

## Design decisions

- **`rebuild` vs `update`.** Both share the `build → lint → push →
  upload → tag` tail (`build_pipeline`); only the head differs.
  `rebuild` (Ubuntu PPA) merges the Debian branch and **normalizes** the
  changelog (synthesized body). `update` (Debian branch) imports a new
  upstream (`gbp import-orig --uscan --pristine-tar`) and runs
  `gbp dch -c -R -D unstable`, leaving the changelog **as gbp writes
  it** — a real new-upstream entry, so post-release commits (e.g. a
  fixup) show up as bullets and nothing is normalized away. The
  distribution is pinned with `-D unstable` because dch's release
  heuristic otherwise fills in the *host's* distribution (an Ubuntu
  devel codename like `resolute`), which fails Debian CI. `update` also
  decouples the
  build suite (`--build-suite`, default `testing`) from the changelog
  distribution, and uploads to dput's default target (no `--ppa`).
- **Rebuild target type (PPA vs proposed-update vs backports).**
  `rebuild`'s merge flow serves three target types, chosen by
  `classify_target_type` from the branch name: a `debian/<codename>`
  branch whose codename is a numbered Debian release
  (`debian-distro-info`, consulted only for `debian/` branches so plain
  PPA runs don't need it) is a Debian **proposed-update**; a
  `debian/<codename>-backports` branch (numbered release + suffix) is a
  Debian **backport**; everything else is an Ubuntu **PPA**. The type
  drives three things, all still funnelled through the same merge →
  normalize → build machinery:
  - *version* — `changelog::rebuild_version` (`~<codename>+<N>`) vs
    `changelog::proposed_version` (`~deb<N>u<M>`) vs
    `changelog::backports_version` (`~bpo<N>+<M>`, the official
    backports scheme). All take the base from the merged changelog and
    bump a counter; `debian_base` strips
    all three suffix shapes. The proposed-update suffix uses `~` (not the
    `+deb<N>u<M>` that `gbp dch --stable` emits) so it sorts *older*
    than the plain build — the stable package must never shadow
    testing/unstable on upgrade. dbranch normalizes gbp's `+` to `~`.
  - *changelog command* — `gbp dch --bpo -D <distribution>` (PPA and
    backports) vs `gbp dch --stable` (proposed-update). gbp's output is
    provisional either way:
    `normalize_top_stanza` rewrites the version and synthesizes the body
    (`* Rebuild for <codename>`), so the merge delta gbp lists is
    discarded. `--stable` needs no `-D` (it targets the stable suite
    itself) and may require a newer gbp than older Ubuntu ships.
  - *salsa-ci preset* — `adjust_salsa_ci(text, release, add_backports)`:
    PPA gets `RELEASE: "unstable"` + the backports relaxations;
    proposed-update gets `RELEASE: "<codename>"` and backports
    `RELEASE: "<codename>-backports"`, both with **no** relaxations
    (they're real Debian builds facing the normal checks). See the
    salsa-ci landmines section for why the pin is mandatory.
  A proposed-update or backports run is gated on a **Debian host**
  (`host::is_debian`
  via `/etc/os-release`): `gbp dch --stable` needs a newer gbp than older
  Ubuntu ships, and the stable build chroot + `dput`-to-stable are
  Debian-only. The check hard-fails early (before any work) for a real
  run; `--dry-run` is exempt so the flow can still be shown as a tutorial
  anywhere. This supersedes the upload-only host gate *for those
  targets* — the whole flow needs Debian, not just the upload.
  Note dbranch shows and runs `gbp dch --stable` honestly rather than
  faking a portable command, per the transparency contract. The upload
  stage goes to `dput`'s default target (the Debian archive) for a
  proposed-update — the "upload needs a target" precondition is enforced
  per-target and applies only to PPA branches; an explicit
  `--upload-target` is still honored.
- **Idempotency / resume.** The packaging adjustments (`fixup` and the
  in-merge adjust), `push` (`git push -u` then plain `git push`), chroot
  create/refresh, and host-key trust are all idempotent. The **`merge`
  stage is not** — it generates a changelog entry, so re-running it from
  the top can add a duplicate. The **`import` stage** (update) is *partly*
  self-healing: `gbp import-orig` won't re-import (see above), so a
  re-run picks up at the changelog step — but `gbp dch` itself isn't
  idempotent, so a re-run *after* a fully-successful import can still add
  a stray stanza. Resume a failed run at the stage that stopped via
  `--stage`, not from the top.
- **Bulk is local-branch only, by design.** A local branch *is* the
  opt-in: check one out to include it in bulk, delete it to drop it.
  Remote-inclusive bulk was considered and rejected.
- **Bulk selection by codename.** A branch is a PPA target iff its
  codename is a real Ubuntu release (`ubuntu-distro-info`); this excludes
  the Debian branch, `master`/`main`, Debian suites, and gbp plumbing
  without an exclude-list. `select_ppa_branches` does **not** also
  exclude the merge source — the Debian branch already fails the codename
  filter, so excluding the source only ever dropped a *PPA* branch that
  happened to be checked out (it silently vanished from the set). Instead
  `resolve_bulk_targets` refuses a PPA-branch source up front **for the
  merge stage** (you can't merge a PPA into its siblings / itself); other
  stages don't use the source and run from any branch.
- **Branch taxonomy → target type** (drives the planned target-type /
  version-scheme work): `master`/`main`/`debian/unstable` → Debian
  unstable (`dput` default, or mentors for a new pkg / proposed NMU);
  `ubuntu/*` or an Ubuntu-release codename → PPA (`~<codename>+N`);
  `debian/<codename>` (e.g. archlinux-keyring's `debian/trixie`) →
  special (kept current in stable). Normal Debian backports/proposed
  would be `debian/<codename>-backports` (`~bpoN+M`) /
  `-proposed-updates` (`+debNuM`).
- **Naming: `codename` → `distribution` (planned).** The value passed to
  `pbuilder-dist`/`gbp dch -D`/the changelog field is the toolchain's
  "distribution"; "codename" is correct only for the `~<codename>+N`
  version suffix. The rename is deferred to the target-type refactor,
  where the two roles actually diverge.
- **Stage pipeline order:** `merge → build → lint → push → upload →
  tag`. `all` = `merge,build,lint,push` (upload/tag are deliberate
  publish/release steps, opt-in).
