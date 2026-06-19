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

- **Idempotency / resume.** The packaging adjustments (`fixup` and the
  in-merge adjust), `push` (`git push -u` then plain `git push`), chroot
  create/refresh, and host-key trust are all idempotent. The **`merge`
  stage is not** — it generates a changelog entry, so re-running it from
  the top can add a duplicate. Resume a failed run at the stage that
  stopped via `--stage`, not from the top.
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
