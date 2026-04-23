# cpu-sig-tracker: TODO

Updated as work progresses. Items are deleted or checked off when
done; in-progress items get an `(in progress)` marker.

## MVP (v0.1)

### Dependencies / building blocks
- [ ] Add `untag_build(tag, nvr, profile)` to `sandogasa-koji`
- [x] MR detail fetch + `parse_mr_url` in `sandogasa-gitlab`
- [x] `sandogasa-gitlab::parse_issue_url` for the `/-/issues/` and
      `/-/work_items/` URL forms
- [x] `sandogasa-gitlab::set_work_item_dates` via GraphQL
      (`startAndDueDateWidget`)
- [x] `sandogasa-gitlab` `Issue`/`IssueUpdate` date fields
      (start_date, due_date, created_at)
- [x] `sandogasa-koji::build_creation_date` for start_date lookup
- [x] `sandogasa-jira` library crate (minimal issue lookup)
- [x] `sandogasa-jira::Issue::resolution_date` for due_date lookup
- [x] `sandogasa-fedrq::src_nvrs` batch NVR lookup for Stream

### Tool crate
- [x] Skeleton + workspace wiring

### Subcommands
- [x] `config` — interactive GitLab/JIRA token setup
- [x] `dump-inventory --release c10s,c9s -o inv.toml` — list
      Koji-tagged packages, emit sandogasa-inventory TOML
- [x] `file-issue <mr-url>` — standardized issue body, auto-extract
      JIRA from MR, apply release/type labels, set In progress,
      stamp start_date
- [x] `retire <issue-url>` — verify JIRA resolved + build untagged,
      prompt, leave audit note, set Done/Won't do, stamp dates,
      close the issue
- [x] `status -i inv.toml` — per-package report with JIRA state +
      Koji/Stream NVR compare + suggestion; `--refresh` rewrites
      bodies to the standard format, reconciles work-item status,
      backfills dates; `--include-closed` extends to closed issues;
      `-p, --package` narrows the scan
- [x] `sync-issues -i inv.toml` — report-only: active / proposed /
      missing per (release, package)
- [x] `untag <pkg|nvr> --release c10s` — verify JIRA resolved via
      the tracking issue, prompt, untag from both `-release` and
      `-testing`
- [x] `dump-inventory --prune` — drop workload entries that are no
      longer tagged in either `-release` or `-testing`; orphan
      `[[package]]` metadata blocks are preserved

Intentionally not shipping:
- ~~`sync-issues --file-missing`~~ — too easy to misfire (guessing
  which MR corresponds to a missing tracking issue across
  multiple open MRs is lossy). Files should continue to be filed
  explicitly via `file-issue`.

### Output
- [x] Human-readable `status` tabular output
- [x] `--json` for `status` and `sync-issues`

### Config
- [x] `config` subcommand stores GitLab + JIRA tokens in
      `~/.config/cpu-sig-tracker/config.toml`

### Tests
- [x] Unit tests for issue body parse / format round-trip
- [x] Mock tests for `sandogasa-jira`
- [x] wiremock + fake-koji integration tests for each
      subcommand's `run_inner` path

### Docs
- [x] Tool `README.md` (install, every subcommand, workflow example)
- [x] Root `README.md` entry (alphabetical)
- [x] `CHANGELOG.md` Unreleased entry

## Post-MVP

### Features
- [ ] `stale-proposed` classification in `sync-issues` + a `migrate`
      action to move a package_tracker entry into its `rpms/<pkg>`
      project once the MR lands
- [ ] `rebase <pkg>` — drive the dist-git / MR workflow to rebuild
      against the latest Stream
- [ ] Snapshot diff: compare two `status --json` outputs to show what
      changed
- [ ] CI-friendly `--strict` mode that fails on missing tracking
      issues

### Open questions to resolve
- [ ] Rebase heuristic when Stream NVR is lower than
      proposed_updates NVR
