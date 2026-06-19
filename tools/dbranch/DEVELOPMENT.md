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
- **`gbp dch`'s body is discarded, not used.** After a merge it lists
  the *entire merged Debian delta* (every commit), and `--since` can't
  cleanly scope to "PPA-side changes since the last rebuild" (it either
  includes the delta or accumulates old rebuild commits). So
  `normalize_top_stanza` **synthesizes** the body: `* Rebuild for
  <codename>` plus, when `adjust_branch_packaging` changed packaging
  files this run, a single `* Adjust <files> for <codename>` line.
  Discarding gbp's body also drops any `UNRELEASED` it added.
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

### Launchpad / EOL

Launchpad rejects uploads to an **EOL series' PPA**, so bulk
`--include-eol` is local-only and is rejected together with the
`upload` stage.

### salsa-ci

salsa-ci builds against **Debian** (`RELEASE`), not Ubuntu (it doesn't
speak Ubuntu). The new-branch adjustment sets `RELEASE: "unstable"` plus
backports-style relaxations — but only when absent: a maintainer may pin
`RELEASE` to an older Debian suite for an old Ubuntu LTS (better signal),
and that is left untouched.

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
  without an exclude-list.
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
