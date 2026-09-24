# cpu-sig-tracker

Track [CentOS Proposed Updates (CPU) SIG][cpu-sig] package state across
Koji, GitLab, and JIRA.

[cpu-sig]: https://sigs.centos.org/proposed-updates/

The CPU SIG temporarily ships fixed packages (typically CVE backports)
while CentOS Stream catches up to RHEL's security fixes. This tool
automates the polling and nudging around that workflow — classifying
each tracking issue's state, flagging rebase-against-newer-Stream
needs, keeping GitLab metadata (dates, work-item status) in sync
with reality, and driving the untag + retire flow once Stream has
caught up.

Per-package tracking issues live in the
[proposed_updates GitLab group][cpu-gitlab], one issue per
`(package, release)` pair. The tool keys off a
[`sandogasa-inventory`](../../crates/sandogasa-inventory/) TOML file
populated by `dump-inventory` (typically regenerated before each
status sweep).

[cpu-gitlab]: https://gitlab.com/groups/CentOS/proposed_updates/-/work_items

## Installation

```sh
cargo install cpu-sig-tracker
```

Requires the [`koji`](https://pagure.io/koji) CLI with a `cbs` profile
configured (CentOS Build System), and
[`fedrq`](https://github.com/gotmax23/fedrq) for CentOS Stream repo
queries. Both are looked up on `$PATH`.

## Configuration

Before any subcommand that talks to GitLab or JIRA:

```sh
cpu-sig-tracker config
```

Prompts for a GitLab personal access token (validated against
gitlab.com) and, optionally, Red Hat JIRA credentials. Red Hat's
tracker is an Atlassian Cloud site (`issues.redhat.com` redirects to
`redhat.atlassian.net`), so those are an API token created at
<https://id.atlassian.com/manage-profile/security/api-tokens> — for a
scoped token, `read:jira-user` and `read:jira-work`, plus
`write:jira-work` to comment — together with the email of the Atlassian
account it belongs to; both are checked against the site before being
kept. Authenticated calls go through Atlassian's API gateway, the only
place a scoped token is honoured; anonymous reads go to the site. Credentials are
written to `~/.config/cpu-sig-tracker/config.toml` with 0600
permissions, and can also be supplied via the `GITLAB_TOKEN`, and
`JIRA_EMAIL` with `JIRA_TOKEN`, environment variables. Anonymous JIRA
access works for public issues. Links to issues use the
`redhat.atlassian.net` host throughout.

## Subcommands

### `config`

Interactive token setup (see above).

### `dump-inventory`

```sh
cpu-sig-tracker dump-inventory --release c9s,c10s -o inventory.toml
```

Enumerates packages tagged into `proposed_updates<N>s-packages-main-release`
for each release and writes a `sandogasa-inventory` TOML file. Safe
to re-run — existing entries are preserved, newly-discovered packages
are added, and each release becomes its own workload.

Use `--prune` to drop packages from the workload that are no longer
tagged in either `-release` or `-testing`. Orphan `[[package]]`
metadata blocks are left in place so user-entered fields (poc,
reason, team, …) survive a re-build.

### `file-issue`

```sh
cpu-sig-tracker file-issue <mr-url> \
    [--affected VER-REL] [--expected-fix VER-REL] \
    [--jira RHEL-N] [--release cNs] [--type security] \
    [--note "context"] [--dry-run]
```

Given a CentOS Stream MR URL, files a standardized tracking issue in
the corresponding `CentOS/proposed_updates/rpms/<pkg>` project.
Derives package / release / JIRA key automatically from the MR
(overridable), applies `cpu-sig-tracker`, release, and optional
type labels, sets the GitLab work-item status to `In progress`, and
stamps `start_date` from the Koji build's creation time.

The issue body follows a canonical format that `status` parses back
(MR, JIRA, Release, Affected build, Expected fix).

### `list-issues`

```sh
cpu-sig-tracker list-issues [-i inventory.toml] [--release cNs] \
    [--package PKG,...] [--adopt] [--json]
```

The SIG's tracking issues, one row per release and package, with the
issue's URL and the upstream MR it names — the arguments the other
commands take. A row is `active` (open per-package issue carrying the
tool's `cpu-sig-tracker` label) or `hand-filed` (open per-package
issue with the release label only — filed by a person, so the other
commands do not see it). Without an inventory the releases come from
the issues' own labels. With `-i`, every inventory package is
classified as well, adding `proposed` (only in the central
`proposed_updates/package_tracker`) and `missing`; issues for packages
outside the inventory are still listed.

Hand-filed issues are offered the label at the prompt, or labelled
outright with `--adopt`, after which every command tracks them; an
unattended run only reports them. Otherwise read-only; file new
tracking issues explicitly via `file-issue`.

### `ping`

```sh
cpu-sig-tracker ping -i inventory.toml [--release cNs] [--package PKG,...] \
    [--days N] [--reping-days N] [-y] [--dry-run] [--json]
```

The SIG's counterpart to Fedora's `needinfo?`, for a change that three
people own: the SIG member who builds the Proposed Update, the author
of the upstream merge request, and the CentOS Stream maintainer who
reviews it. For every open tracking issue the command reads the
upstream MR, the SIG's `-testing` and `-release` builds and stock
Stream, and works out what the change needs from whom — each message
where its reader looks, in the order that makes each step actionable
for the next person:

- **landed** — the MR is merged, or stock's dist-git history names the
  change: a commit on the release branch carrying the tracking issue's
  RHEL key, the RHEL key in the MR's branch name, a CVE id from the
  MR's or issue's title, or the MR's title itself. The change is in;
  `retire` the update. A stock NVR that merely matches the SIG's
  without `~proposed` is not evidence — a mass rebuild reaches the
  same number.
- **rebase-build** — stock Stream has moved past the SIG's build. The
  SIG rebuilds first; a note on the *tracking issue* says so, once per
  stock build, and nothing goes upstream until the rebuild. When stock
  reached the SIG's release number without the change, the SIG's
  `~proposed` build sorts below stock and installs nowhere, and the
  note says to rebuild with a higher release.
- **no MR yet** — the tracking issue names no merge request: nothing
  upstream to nudge, while builds are still announced on the issue.
- **rebase-mr** — the MR no longer merges cleanly. A note on the *MR*,
  addressed to its author, says it is behind its target branch, once
  per head revision.
- **announce** — a SIG build has reached `-testing` or `-release` and
  no note has named it at that stage. A "for those watching" note
  gives the NVR and how to get it — the SIG repo for a release, the
  buildlogs testing repo for a testing build — on the MR and on the
  tracking issue, once per build and stage; the same NVR in both tags
  is announced as released only. Independent of the action.
- **ping** — the MR merges cleanly, the SIG build is current, and the
  MR has been quiet for `--days` (default 14). A note on the MR asks
  the maintainer what blocks review and names the tracking issue.
  While it stands unanswered the change is **waiting**; after
  `--reping-days` (default 30) it is asked again. GitLab is asked to
  recheck the MR's mergeability on every read, and while it reports
  the check as not done the ping is **held** rather than sent to an
  MR that may no longer merge.
- **respond** — someone upstream spoke last, so the SIG owes the
  reply; the last response (date, author, first line) is shown.
  **active** — touched within the window. **merged / closed** —
  upstream has taken the change or dropped it, so nothing is nudged;
  the line says who closed the MR and when, and whether stock is past
  the SIG's build: verify the change is in, then `retire`.

Nothing is posted unasked. After the report, each pending note is
offered one by one at the prompt, default yes — except on a change
where someone upstream is waiting on the SIG, where the default is no
and the prompt says a reply comes first. `-y` takes every default
without asking, for an unattended run; `--dry-run` never prompts and
never posts, and neither does a run with `--json` or without a
terminal. An open MR's line ends with GitLab's own merge status after
a recheck (`mergeable`, `not_approved`, `discussions_not_resolved`,
…). With more than one change the report ends with what the SIG has
to do next — retire, rebuild, reply, pings held — one change per line
with the link the next command takes (the tracking issue for `retire`,
the MR for a reply), and how many notes wait, and how many of those
wait behind a reply. Activity is the later of the MR's own `updated_at`
(pushes, labels, approvals) and its last human note; GitLab's system
notes do not count. The last response is shown flattened onto one
line, so a greeting on its first line does not stand for it. Every
note carries a hidden `<!-- cpu-sig-tracker: … -->` marker naming the
build, stage or revision it is about, which is how later runs
recognise it. "Us" is the token's login, from `GET /api/v4/user`. A
comment on the RHEL Jira issue, for its watchers, is the
announcement's third channel once the Jira crate can write.

### `retire`

```sh
cpu-sig-tracker retire <issue-url | package> [--release cNs] \
    [--reason landed|abandoned] [--stock-fix NVR] [--yes] [--force] [--claim]
```

Takes the tracking issue's URL, or the package name: a name is
resolved through the SIG's open tracking issues (what `list-issues`
shows), `--release` picking the release when the package is tracked in
several; without it, several matches are listed and chosen by number
at a terminal, and are an error in an unattended run.

A tracking issue closes for a reason, and the reason is the one
question you answer: is the problem fixed in stock, or is the SIG
giving up?

- **landed** — the problem is fixed in stock. Something has to say
  so: the SIG's MR merged; a commit on stock's release branch names
  the change (the tracking issue's RHEL key, the RHEL key in the MR's
  branch name, a CVE id from either title, the MR's title); the RHEL
  issue was resolved as Done; or, when none of those is on record, you
  confirm at the prompt that you verified it and name the stock build
  that carries the fix (`--stock-fix <nvr>` unattended). That build
  is recorded on the issue as a `- **Stock fix**:` line in your name,
  which `timeline` reads ahead of any inference, and the audit note
  says so too. Stock merely being past the SIG's build is
  not a reason — that is the rebase case `ping` reports — and the tool
  says so instead of suggesting anything.
- **abandoned** — the SIG gives up on the change, Bugzilla's CANTFIX.
  Nothing to verify beyond the build being untagged.

`--reason` names it; omitted, it is inferred from the MR, stock's
history and the RHEL issue and offered as the default at a terminal,
and taken as is with `--yes` (an unattended run with nothing to infer
from is an error). The build must be untagged from `-release` Koji in
every case (retire follows `untag`); `--force` bypasses the checks.
The RHEL issue's state is reported for the record and stamps
`due_date` from its `resolutiondate` when resolved, but decides
nothing on its own.

The issue closes with a label saying how it ended — `landed` when the
SIG's own MR merged, `superseded` when the fix reached stock another
way, `abandoned` — the work-item status `Done` or `Won't do`, and an
audit-trail comment naming the evidence. When the upstream MR is still
open it is told, in those words, that the SIG has retired the Proposed
Update; an abandoned change's MR is the SIG's own and is closed with
that note. A comment on the RHEL issue is not posted yet: it waits on
Jira write support, and will be made only when a stock commit names
that RHEL key.

It also offers to assign the issue to you as it closes it — triage is
work worth crediting. `--claim` takes it without asking; `--yes` alone
declines, since an unattended run must not reassign work nobody asked
it to; otherwise you are asked. Who "you" are comes from the token
(`GET /api/v4/user`), because GitLab assigns by numeric id.

### `status`

```sh
cpu-sig-tracker status -i inventory.toml \
    [--release cNs] [--package PKG,...] \
    [--refresh] [--include-closed] [--json]
```

Per-package report of every active tracking issue in the inventory's
releases: JIRA key + status, currently-tagged NVR, current Stream
NVR, and a suggested next action (`in-progress`, `rebase`,
`untag-candidate`, `retire-issue`, `not-yet-tagged`, `no-jira`, or
`—` once closed with no build tagged). `--json` emits a flat
serde-serialized array.

`--refresh` turns the read pass into a write pass: rewrites any body
that's drifted from the canonical format, backfills stale MR / JIRA
status lines, reconciles the GitLab work-item status against live
JIRA + Koji (Done / Won't do / In progress / To do), and sets
missing `start_date` / `due_date` via the GraphQL work-item API.
`--include-closed` extends the refresh scan to historical tracking
issues so their dates can be backfilled after the fact.

### `timeline`

```sh
cpu-sig-tracker timeline [--release cNs] [--package PKG,...] [--open-only] \
    [--json | --csv]
```

How long each Proposed Update took, and where the time went — the
dates that used to live in a spreadsheet, one row per tracking issue,
closed ones included (they are the history): when the RHEL issue, the
MR and the tracking issue were filed; the SIG's first `-release` build,
when it was built and when it entered `-release` (covered from); the
stock build carrying the fix, when it was built and when it became
compose-bound (`<release>-pending-signed`); Red Hat's advisory and its
date. Then three spans:

- **coverage** — from the SIG build entering `-release` until the
  stock fix is compose-bound: how long the SIG carried the change.
- **Stream against RHEL** — the stock fix's compose-bound date against
  the RHEL advisory's; negative when Stream was first.
- **shadow** — a stock build past the SIG's went compose-bound while
  the SIG build was still tagged and was not the fix, so the SIG's
  `~proposed` build stopped installing; how long until the SIG's next
  `-release` build or the issue closing, or "still" when neither has
  happened.

Everything is read from systems that keep history for good — GitLab,
Jira, CBS Koji, CentOS Stream's Koji (the `stream` profile centpkg
ships) and Red Hat's security data (by the CVE in the title) — off the
metadata the issue carries. The tracking issue's "expected fix" is a
guess and is not used: the stock fix is, first, the build a person
recorded on the issue (`retire --reason landed --stock-fix`, or a
`- **Stock fix**: <nvr>` line written by hand on an older issue); else
the build made from the stock commit that names the change, the way
`ping` and `retire` read evidence; else the first compose-bound build
after that commit (or after the MR merged). Each row says which. A
change none of that settles says so, and how to record what you
establish. `--csv` has the spreadsheet's columns plus the spans;
`--json` everything.

### `untag`

```sh
cpu-sig-tracker untag <package|nvr> --release cNs [--yes] [--force]
```

Removes a proposed_updates build from its CBS `-release` and
`-testing` tags after verifying the linked JIRA is resolved. Accepts
either a package name (auto-discovers currently-tagged NVRs across
both tags) or a specific NVR. Pair with `retire` to close the
tracking issue after the build is gone.

## Typical workflow

```sh
# Refresh the inventory from CBS.
cpu-sig-tracker dump-inventory --release c9s,c10s -o cpu-sig.toml --prune

# File a tracking issue for a new MR.
cpu-sig-tracker file-issue https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42 \
    --type security --affected xz-5.6.2-3.el10 --expected-fix xz-5.6.4-1~proposed.el10

# See what needs attention.
cpu-sig-tracker status -i cpu-sig.toml

# Nudge upstream MRs that have gone quiet; read the answers.
cpu-sig-tracker ping -i cpu-sig.toml --dry-run  # report only
cpu-sig-tracker ping -i cpu-sig.toml            # report, then ask per note
cpu-sig-tracker ping -i cpu-sig.toml -y         # unattended: the defaults

# Sync GitLab metadata (status, dates, body format).
cpu-sig-tracker status -i cpu-sig.toml --refresh

# Once Stream catches up: untag, then retire.
cpu-sig-tracker untag xz --release c10s --yes
cpu-sig-tracker retire xz --release c10s --reason landed --yes
```

## System-wide configuration

Settings are read from `/etc/cpu-sig-tracker/config.toml` first, then
overridden per key by `~/.config/cpu-sig-tracker/config.toml`, with
command-line flags overriding both. A system file alone is enough — no
per-user file is required — and either may also carry a `[defaults]`
table pinning flag defaults (see the root `DEVELOPMENT.md`).

`cpu-sig-tracker config` writes the user file only, with 700 on the
directory and 600 on the file. Nothing writes under `/etc`: a system
file is admin-authored, holds shared non-secret settings, and is
normally shipped `root:root 0644`.

Credentials belong in the per-user file, which is 600, or in an
environment variable — a token under `/etc` is readable by every local
user. For an unattended machine, give the job its own user and its own
600 config.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
