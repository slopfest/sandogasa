# cpu-sig-tracker: Project Plan

## Purpose

Track state for the [CentOS Proposed Updates SIG][cpu-sig] — the group
that temporarily builds fixed packages while CentOS Stream catches up
to RHEL-first security fixes.

[cpu-sig]: https://sigs.centos.org/proposed-updates/

For each package the SIG ships, there is a tracking issue in the
[proposed_updates GitLab group][cpu-gitlab], which links to a
Merge Request against the CentOS Stream package (which in turn
links to the upstream JIRA issue). A proposed-updates build should
be retired (untagged) once the JIRA is closed and CentOS Stream has
caught up — or rebased on top of a newer Stream build if the JIRA
is still open.

[cpu-gitlab]: https://gitlab.com/groups/CentOS/proposed_updates/-/work_items

This tool automates the polling and nudging around that workflow.

## Workflow the tool supports

For each CentOS release (`c9s`, `c10s`, …) and its `<release>-proposed_updates`
Koji tag:

1. **Inventory what we have**. Build a `sandogasa-inventory` TOML
   from the set of packages currently tagged into
   `<release>-proposed_updates`.
2. **For each package, find its tracking issue** in the GitLab
   proposed_updates group. Emit a warning for any tagged build
   that has no tracking issue, and (on demand) file one.
3. **For each tracking issue, resolve the linked MR + JIRA**. Poll
   JIRA for the current status.
4. **Suggest next action** per package:
   - *JIRA closed* → prompt user to verify, then untag the build.
   - *JIRA still open, proposed_updates build older than Stream* →
     prompt user to rebase the MR on top of the newer Stream.
   - *JIRA still open, proposed_updates newer or equal to Stream* →
     no action; just tracked.

## Tool shape

### CLI subcommands

```
cpu-sig-tracker dump-inventory --release c10s -o inventory.toml
cpu-sig-tracker file-issue <mr-url> [--affected VER] [--expected-fix VER]
cpu-sig-tracker status -i inventory.toml [--release c10s]
cpu-sig-tracker sync-issues -i inventory.toml [--file-missing]
cpu-sig-tracker untag <nvr> [--release c10s]
```

- **`dump-inventory`**: enumerate packages tagged in
  `<release>-proposed_updates` (via `koji list-tagged`), emit a
  sandogasa-inventory TOML with one package per entry plus NVR
  metadata. Packages that already have tracking issues get linked.
- **`file-issue`**: given an MR URL (and optionally affected /
  expected-fix versions), file a standardized tracking issue in
  the proposed_updates GitLab group. If versions are omitted,
  compute them via `fedrq pkgs -b <release> <name>` + NVR bump.
  The issue body includes the MR link, JIRA link (auto-extracted
  from the MR description/linked-issues), and the affected /
  expected-fix NVRs.
- **`status`**: scan the inventory, for each package fetch the
  tracking issue → MR → JIRA, compare to current Stream build,
  report per-package next action. Human-readable + `--json`.
- **`sync-issues`**: for each inventory package, verify a tracking
  issue exists. With `--file-missing`, create one for any that
  don't. Useful after `dump-inventory` to close gaps.
- **`untag <nvr>`**: run the "JIRA closed → untag" path for a
  single build. Verifies the JIRA is closed, prompts the user, and
  calls `koji untag-build`. Non-interactive with `--yes`.

### Issue body format (for `file-issue`)

Standardized so `status` can reliably parse it back:

```markdown
## <package> <affected-nvr> → <expected-fix-nvr>

- **MR**: <url>
- **JIRA**: <url>
- **Release**: c10s
- **Affected build**: <nvr>
- **Expected fix**: <nvr>
- **Status**: open
```

`status`'s parsing is lenient — it only needs to find the MR and
JIRA links. The rest is cosmetic.

### Reused building blocks

- `sandogasa-koji` — `list_tagged_nvrs` (have), plus new
  `untag_build` (add).
- `sandogasa-gitlab` — `create_issue`, `list_issues`, `edit_issue`
  (have) for the proposed_updates group; plus MR-detail fetch
  (likely need to add).
- `sandogasa-inventory` — reuse the TOML data model for our
  inventory artifact.
- `sandogasa-fedrq` — `pkgs -b <release> <name>` for current
  Stream NVR lookups.
- `sandogasa-bugzilla` — probably not needed (this is all JIRA).

### New building block: JIRA client (`sandogasa-jira`)

We don't have a JIRA client yet. For this tool we only need:

- `issue(key) -> Issue` — status, resolution, summary.
- Optionally: `search(jql)` for bulk lookups later.

Red Hat JIRA is at `https://issues.redhat.com`. Anonymous access
works for public issues; config-stored API token for private ones.

Scope: add a minimal `sandogasa-jira` library crate alongside the
other API clients, following the same pattern as
`sandogasa-bugzilla` / `sandogasa-bodhi`.

## MVP scope (v0.1 of the tool)

1. New `sandogasa-jira` library crate (minimal: fetch one issue).
2. Add `untag_build` to `sandogasa-koji`.
3. Add MR-detail fetching to `sandogasa-gitlab` (get description,
   linked issues, source/target branches) if not already there.
4. `cpu-sig-tracker` crate with `dump-inventory`, `file-issue`,
   `status`, `sync-issues`, and `untag` subcommands.
5. Human-readable status output + `--json`.
6. Interactive prompts for untag/rebase actions, `--yes` to skip.

## Post-MVP

- Auto-rebase subcommand that drives the dist-git / MR workflow.
- Multi-release scanning in one invocation.
- Diff view between two status snapshots.
- CI-friendly mode that fails on missing tracking issues.

## Files to create / modify

- New crate: `crates/sandogasa-jira/` (Cargo.toml, lib.rs,
  client.rs, models.rs, README.md).
- New crate: `tools/cpu-sig-tracker/` (Cargo.toml, src/main.rs,
  src/subcommands.rs, src/issue_body.rs, README.md, PLAN.md,
  TODO.md).
- Modify `crates/sandogasa-koji/src/lib.rs` — add `untag_build`.
- Modify `crates/sandogasa-gitlab/src/lib.rs` — add MR fetch if
  needed.
- Root `Cargo.toml` — add both new crates to workspace and
  `[workspace.dependencies]`.
- Root `README.md` — tool entry + library entry.
- `CHANGELOG.md` — `Unreleased` entry.

## Open questions

1. Authentication: how do we get JIRA API tokens for users who
   want to query private issues? Probably reuse
   `sandogasa-config`'s `[jira]` section pattern. Anonymous
   works for most RHEL/RHBA-level public CVEs.
2. Koji profile: this is always `cbs` (CentOS Build System).
   Should we hardcode it or require `--koji-profile`? Probably
   default to `cbs` with override.
3. What does "rebase needed" mean exactly? The simplest heuristic
   is "Stream NVR > proposed_updates NVR by RPM vercmp". But
   sometimes Stream's version is lower (old RHEL backport). We
   may need to compare per-component or just trust the maintainer
   to decide.
4. Should the inventory TOML be keyed per release (one file per
   c9s / c10s) or multi-release (one file with release as a
   field)? Per-release is simpler; multi-release is fewer files.
   Start with per-release, revisit if it becomes annoying.
