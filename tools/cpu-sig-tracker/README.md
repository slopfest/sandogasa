# cpu-sig-tracker

Track [CentOS Proposed Updates (CPU) SIG][cpu-sig] package state across
Koji, GitLab, and JIRA.

[cpu-sig]: https://sigs.centos.org/proposed_updates/

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
queries. Both are looked up on `$PATH`. `mr` also needs `git`, and `centpkg`
(`sudo dnf install centpkg`) to make the fork the first time, with an
API token in `~/.config/rpkg/centpkg.conf`.

## Configuration

Before any subcommand that talks to GitLab or JIRA:

```sh
cpu-sig-tracker config
```

Prompts for a GitLab legacy (classic) personal access token with the
`api` scope (validated against gitlab.com) and, optionally, Red Hat JIRA credentials. Red Hat's
tracker is an Atlassian Cloud site (`issues.redhat.com` redirects to
`redhat.atlassian.net`), so those are an API token created at
<https://id.atlassian.com/manage-profile/security/api-tokens> — for a
scoped token, `read:jira-user` and `read:jira-work`, plus
`write:jira-work` to comment or file an issue — together with the
email of the Atlassian account it belongs to; both are checked against
the site before being
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
    [--jira RHEL-N[,RHEL-N…]] [--release cNs] [--type security] \
    [--note "context"] [--dry-run]
```

Given a CentOS Stream MR URL, files a standardized tracking issue in
the corresponding `CentOS/proposed_updates/rpms/<pkg>` project.
Derives package / release / JIRA keys automatically from the MR
(overridable): one `- **JIRA**:` line per `Resolves:` line in the MR,
as an update can carry several fixes, each with its RHEL issue; an MR
without one gives the first `RHEL-` key it names. Applies
`cpu-sig-tracker`, release, and optional
type labels, sets the GitLab work-item status to `In progress`, and
stamps `start_date` from the Koji build's creation time.

The issue body follows a canonical format that `status` parses back
(MR, JIRA, Release, Affected build, Expected fix).

The project has to exist first: a pull mirror of the CentOS Stream
repo, with the change's branch pushed to it as
`<release>-proposed-<RHEL key>`. When it is missing, `file-issue` stops
before asking Jira anything and prints the steps that create it
(`--dry-run` prints them as a warning and still shows the issue), and
`mr` names them as the next step after opening the merge request.

### `jira`

```sh
cpu-sig-tracker jira <package> --branch c9s [--cve CVE-YYYY-N,...] \
    [--summary "..."] [--description "..." | --description-file PATH] \
    [--priority Major] [--type enhancement] [--file [--yes]] [--json]
cpu-sig-tracker jira <mr-url> [...]          # an MR already open
```

Finds the RHEL Jira issue a Proposed Update's merge request needs,
or drafts one. CentOS Stream's merge request template asks for an
approved ticket, named in the MR's description as `Resolves: RHEL-N`,
and Red Hat wants one issue per branch, so the issue comes first, one
per branch, before the MR is opened: given a package name, `--branch`
says which branch and `--cve` which CVEs (repeated or
comma-separated: a rebase often fixes several). For an MR already open
without one, its URL can be given instead, and the package, branch,
title and the CVEs it fixes (from its title and description) are
read from it.

It first searches RHEL for open issues on that package on any
release, once per CVE, as Product Security files one tracker per CVE
and release, or once for the package when there is no CVE. An issue
naming a version of the branch's RHEL major (`CentOS Stream 9`,
`rhel-9.6`, `rhel-9.6.z` for c9s) covers its CVE on the branch. The
report lists, per CVE, the issues covering it or that none is
visible, then the issues open for other releases only, and says whom
the search ran as (anonymously, or the configured account). When
every CVE is covered it prints the `Resolves: RHEL-N` lines the MR
needs, one per issue, and drafts nothing. Red Hat Product Security's CVE trackers are
restricted, invisible anonymously and to ordinary community accounts,
so for each CVE with none it also says what Red Hat's security data
lists for the package in that RHEL major: `Affected` (or `Fix deferred`, `Under
investigation`) means a tracker probably exists, and filing may
create a duplicate Red Hat will link, so name the CVE in the MR; a
fix by an advisory means CentOS Stream may already have it. Then it
drafts one issue covering every CVE without one:

- type Bug, as Red Hat files enhancement requests too, with an
  `RFE: ` summary for `--type enhancement`;
- a summary from `--summary`, else the MR's title; for a single CVE,
  else the CVE's title as Red Hat's bug for it reads (`rsync: Denial
  of Service via crafted checksums during delta transfer
  (CVE-2026-119741)`), or GitHub's advisory's when Red Hat has none.
  Several CVEs need `--summary` or an MR, as no one CVE's title names
  the change. Without a summary there is no draft, only the search;
- the package as the component;
- one affected version, `CentOS Stream N` (`rhel-8.10` for c8s): RHEL
  allows a single affected version, trims issues listing more in a way
  that keeps no predictable one, and sets the RHEL versions itself;
- a priority from the most severe CVE's severity, each taken from
  Red Hat's rating, else NVD's, else GitHub's advisory: critical → Critical, high (Red
  Hat's Important) → Major, medium → Normal, low → Minor. Blocker is
  left to Red Hat. A CVE Red Hat assesses as not applicable to the
  branch's RHEL major (`Not affected`: not built or reachable there,
  or for a platform RHEL does not run on) is still fixed and
  described, with Red Hat's assessment and a reminder to say in
  `--description` why it applies, but does not count toward the
  priority. A `--priority` that disagrees is confirmed at a terminal
  and kept with a note otherwise;
- a description of your text, then for each CVE its severity by
  every source that rated it, NVD's description and link, then the
  MR.

The draft is printed exactly as it would be filed, every field and
the whole description, because Red Hat's Jira cannot edit an issue
once it is filed, only take comments. Once filed, the `Resolves:`
lines list the issues found and the new one. With Jira credentials it is also
checked against the fields RHEL requires and the versions it offers.
`--file` files it after asking (default no); `--file --yes` files
without asking, for scripts. A public MR is read without a GitLab
token.

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

### `mr`

```sh
cpu-sig-tracker mr [BRANCH] [-C DIR] [--target cNs] [--title TEXT] \
    [--notes TEXT | --notes-file PATH] [--explain] [--dry-run] [--yes]
```

Opens the CentOS Stream merge request for a Proposed Update's branch,
from the package's dist-git checkout (the current directory, or `-C`):

1. If no remote points at a fork of the package in your own namespace
   (`<user>/centos_rpms_<package>`), `centpkg fork` makes one and adds
   it as a remote named after your GitLab user. GitLab copies a new
   fork in the background, so `mr` waits (a few minutes at most) until
   the copy is done before pushing to it.
2. `git push -u <remote> <branch>`. CentOS Stream's pre-push hook
   (`centpkg pre-push-check`) checks the spec against the `sources`
   file in the working tree, so when another branch is checked out,
   `mr` checks out `<branch>` for the push and switches back after. A
   checkout with uncommitted changes is not switched: `mr` stops before
   anything else and asks for them to be committed or stashed.
3. The merge request is opened into the CentOS Stream branch the
   branch is for: the one it tracks (`origin/c9s`), else its `cNs-`
   prefix, or `--target`.

The merge request is written in CentOS Stream's template from the
branch's commits beyond the target: their messages, trailers dropped,
as the summary of changes, and every `Resolves:`, `Related:` and
`Reverts:` line in them as its tickets. A branch with no `Resolves:`
line is refused, as CentOS Stream requires a Jira issue: find or file
it with `jira` first. The title is the subject of the branch's one
commit; a branch with several needs `--title`. Open merge requests
into the same branch about the same CVE (or the first Jira issue) are
listed first, so a duplicate is seen, but they do not stop it.

When any of the commits has an `Assisted-by: <agent>:<model>` trailer,
the description opens with a disclosure naming each agent and model,
and saying the maintainer is free to write their own fix and close the
merge request. Commits without the trailer get no such line.

`--notes` (or `--notes-file`, for longer text) adds paragraphs to the
end of the summary of changes, before the tickets, for what does not
belong in the commit messages and so in dist-git history: what the
backport deliberately leaves out, or how it was tested.

The merge request is shown in full and opened after a confirmation
(`--yes` skips it). `--explain` narrates each step with the command it
runs, pausing before each; `--dry-run` shows the steps and the merge
request without running anything.

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
    [--reason landed|declined|unanswered|abandoned] [--stock-fix NVR] \
    [--yes] [--force] [--claim]
```

Takes the tracking issue's URL, or the package name: a name is
resolved through the SIG's open tracking issues (what `list-issues`
shows), `--release` picking the release when the package is tracked in
several; without it, several matches are listed and chosen by number
at a terminal, and are an error in an unattended run.

A tracking issue closes for a reason, and the reason is the one
question you answer: is the problem fixed in stock, and if not, why
does the SIG stop carrying it?

- **landed** — the problem is fixed in stock. Something has to say
  so: the SIG's MR merged; stock carries every patch the MR adds, in a
  build at or past the SIG's (matched by upstream commit or by the
  change, as `status` describes); for an MR adding no patches, a commit
  on stock's release branch names the change (the tracking issue's RHEL
  key, the RHEL key in the MR's branch name, a CVE id from either
  title, the MR's title) or every RHEL issue was fixed, with a fix
  version or as `Done-Errata` (a `Done` alone, as for an issue closed
  for inactivity, does not count); or, when none of those is on record
  (or stock lacks a patch and you know it landed another way), you
  confirm at the prompt that you verified it and name the stock build
  that carries the fix (`--stock-fix <nvr>` unattended). That build
  is recorded on the issue as a `- **Stock fix**:` line in your name,
  which `timeline` reads ahead of any inference, and the audit note
  says so too. Stock merely being past the SIG's build is
  not a reason — that is the rebase case `ping` reports — and the tool
  says so instead of suggesting anything.
- **declined** — Red Hat said no: every RHEL issue was closed `Not a
  Bug`, `Won't Do`, `Obsolete` or the like, which is what it is
  inferred from and checked against.
- **unanswered** — Red Hat never responded: every RHEL issue was
  closed with no sign of a fix, as for inactivity, and the change went
  nowhere. Inferred from that; otherwise your judgement.
- **abandoned** — the SIG gives up for its own reasons, Bugzilla's
  CANTFIX. Nothing to verify beyond the build being untagged.

`--reason` names it; omitted, it is inferred from the MR, stock's
history and the RHEL issue and offered as the default at a terminal,
and taken as is with `--yes` (an unattended run with nothing to infer
from is an error). The build must be untagged from `-release` Koji in
every case (retire follows `untag`); `--force` bypasses the checks.
The RHEL issues' state is printed for the record, worded as `untag`
words it. A landed update's `due_date` is the day the evidence shows
it in stock (the stock commit that added the matched patch, the stock
commit naming the change, or the MR's merge), not Jira's, which can be
months later; it is recorded on the issue as a `- **Stock fix**:`
line, with what Jira said beside it when that differs (`… in stock on
2026-04-27 (stock commit 99e0f170 added …); Jira closed RHEL-170526 as
Not a Bug on 2026-10-23`). Otherwise `due_date` is when Jira settled
the RHEL issues.

The issue closes with a label saying how it ended — `landed` when the
SIG's own MR merged, `superseded` when the fix reached stock another
way, `declined`, `unanswered`, `abandoned` — the work-item status
`Done`, or `Won't do` for the last three, and an
audit-trail comment naming the evidence. When the upstream MR is still
open it is told, in those words, that the SIG has retired the Proposed
Update; the MR of a change retired without a fix is the SIG's own and
is closed with that note, saying whether Red Hat declined it, never
answered, or the SIG withdrew it. A comment on the RHEL issue is not posted yet: it waits on
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
releases: JIRA keys + statuses (every issue the update resolves, one
JIRA line each in the tracking issue), currently-tagged NVR, current Stream
NVR, and a suggested next action (`in-progress`, `rebase`,
`untag-candidate`, `retire-issue`, `not-yet-tagged`, `awaiting-stock`,
`not-in-stock`, `jira-declined`, `jira-unverified`, `no-jira`, or `—`
once closed with no build tagged).

While the SIG still ships a build, whether the change is in stock is
judged as `untag` and `retire` judge it, from stock's patches first:
the `.patch` files the update's merge request adds are matched against
stock's by the upstream commit they carry (the `From <sha>` line of
`git format-patch` output, or a `PatchN:` URL's commit), or by the
change itself when the ids differ, as a cherry-pick gets a new one.
Every patch carried, in a stock build at or past the SIG's, is
`untag-candidate` whatever Jira says; carried but not yet built past
the SIG's is `awaiting-stock`; any missing is `not-in-stock`, whatever
Jira or stock's commit messages say. The evidence is printed under the
table and in `--json` (`landed`). An update adding no patches (a
spec-only change) is judged by its RHEL issues and stock's history.

Each RHEL issue is judged by what its resolution says, not by having
one: **fixed** is a resolution meaning fixed (`Done`, …) with a fix
version, or `Done-Errata`, which names the erratum; **unverified** is
closed `Done` with no fix version, as an issue closed for inactivity
is; **declined** is `Won't Do`, `Not a Bug`, `Obsolete` and the like;
a **duplicate** is judged by the issue it duplicates when that is
visible. An update is an untag candidate only once every issue is
fixed; one with an issue declined is `jira-declined` (decide whether
to keep shipping it), one with an issue closed but unverified is
`jira-unverified` (check stock for the change before believing it).
The table marks an unverified, declined or duplicate issue's state. `--json` emits a flat
serde-serialized array, each row's issues under `jira`.

`--refresh` turns the read pass into a write pass: rewrites any body
that's drifted from the canonical format, backfills stale MR / JIRA
status lines, reconciles the GitLab work-item status against live
JIRA + Koji (Done / Won't do / In progress / To do), and sets
`start_date` / `due_date` via the GraphQL work-item API, correcting a
`due_date` that disagrees: the day the issue's `- **Stock fix**:` line
records, else the day the evidence shows the change in stock, else
when Jira settled it.
`--include-closed` extends the refresh scan to historical tracking
issues so their dates can be backfilled after the fact.

### `sync-updates`

```sh
cpu-sig-tracker sync-updates --file docs/content/updates-list.md [--dry-run]
```

Writes the list of updates the Proposed Updates site includes: one
section per release, newest first (`# CentOS Stream 10`, then 9), and
in each a table of the open tracking issues with a build in the SIG's
`-release` (`released`) or `-testing` (`testing`) tag, sorted by
package. Each row links the tracking issue and names the build, the
type label, the CVEs, the upstream merge request and the Jira issues. A
release with none reads "No updates at the moment."

The file is rewritten whole on each run and carries no timestamps, so
it changes only when the state does; `--dry-run` shows the lines that
would change instead. Nothing embargoed is listed: tracking issues are
read anonymously, so a confidential one is never seen, and an update
any of whose Jira issues cannot be read anonymously is left out, whatever
credentials are configured. Needs `koji` with the `cbs` profile.

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
cpu-sig-tracker untag <package|nvr> --release cNs [--explain] [--dry-run] \
    [--yes] [--force]
```

Removes a proposed_updates build from its CBS `-release` and
`-testing` tags once the change is in stock, on the same evidence
`retire` takes as landed: the SIG's MR merged; stock carrying every
patch the MR adds, in a build at or past the SIG's (see `status`); or,
for an MR adding no patches, a commit on stock's branch naming the
change (its RHEL key, a CVE, the MR's title) or every RHEL issue fixed
(a fix version, or `Done-Errata`; a `Done` alone does not do). A patch
stock lacks keeps it from counting, whatever Jira says.

The CBS login is checked first, with `koji -p cbs moshimoshi`, before
anything else is asked or touched. CBS logs in with a TLS certificate
(`cert = ~/.centos.cert` in the `cbs` profile); when it is missing or
no longer valid, `untag` says so and gives the commands that make one,
`kinit <user>@FEDORAPROJECT.ORG` (when `klist` shows no valid ticket)
and `centos-cert -u <user>`, the user taken from your Kerberos ticket.
At a terminal it offers to run `centos-cert` and carries on.

Its steps are koji commands you could run yourself. `--explain`
narrates each one, shows the exact command (`koji --profile cbs
untag-build -- <tag> <nvr>`) and waits for Enter before running it;
the in-stock check, which reads GitLab, stock's patches and Jira
rather than running one command, is narrated with its result.
`--dry-run` shows the same steps and untags nothing (the read-only
lookups still run, so the builds and the check are real). The RHEL issues' state is shown either way. Accepts
either a package name (auto-discovers currently-tagged NVRs across
both tags) or a specific NVR. Pair with `retire` to close the
tracking issue after the build is gone.

## Typical workflow

```sh
# Refresh the inventory from CBS.
cpu-sig-tracker dump-inventory --release c9s,c10s -o cpu-sig.toml --prune

# Before opening the MR: find or draft the RHEL Jira issue it will
# name, one per branch; file it once it reads right.
cpu-sig-tracker jira xz --branch c10s --cve CVE-2024-3094
cpu-sig-tracker jira xz --branch c10s --cve CVE-2024-3094 --file

# Push the fix branch to your fork and open the MR (it names the issue).
cd ~/src/centos/rpms/xz
cpu-sig-tracker mr c10s-CVE-2024-3094

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

`cpu-sig-tracker config` asks only for settings that are missing, or whose saved
value is not valid. `--all` asks for every setting again, with the saved
value as the default: Enter keeps it, a new value replaces it, and `-`
clears an optional one. A saved secret is never shown.

Credentials belong in the per-user file, which is 600, or in an
environment variable — a token under `/etc` is readable by every local
user. For an unattended machine, give the job its own user and its own
600 config.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
