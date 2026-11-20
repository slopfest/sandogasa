# sandogasa-report

Activity reporting for Fedora, EPEL, CentOS SIG and openSUSE packaging
work. openSUSE's services, what works there and what each needs, are
in [README.openSUSE.md](README.openSUSE.md).

Generates Markdown or JSON reports summarizing a contributor's
packaging activity across multiple systems:

- **Bugzilla**: bugs filed and closed, split into CVE/security fixes,
  update requests, branch requests, FTBFS/FTI bugs and the rest, on any
  instance — Red Hat's for Fedora and EPEL, bugzilla.opensuse.org for
  openSUSE, each with its own products. On Red Hat's, also review
  requests submitted/completed and reviews done for others (the Fedora
  package-review flow)
- **Bodhi**: updates submitted, pushed to testing, pushed to stable,
  per-release breakdown
- **Koji CBS**: new packages and version updates in CentOS SIG
  release tags, with date-range comparison
- **GitLab**: MRs opened / merged / approved / commented on,
  issues and work items opened / closed / commented on, both
  pushed and authored commit counts per project, tags pushed, and
  GitLab Releases authored. The gap between pushed/authored commits
  flags mirror activity. Optionally scoped by group prefix
  (`CentOS/Hyperscale`, `CentOS/Hyperscale/rpms`, etc.), with
  `extra_groups` for the domain's work elsewhere on the instance.
  The opened list flags each MR's fate the way the Forgejo one
  does: `(merged)`, `(closed)`, or `(applied)`, rebased commits
  included
- **GitHub**: PRs opened / merged / reviewed / commented on, issues
  opened / commented on, authored commit counts per repo, annotated
  tags cut, and GitHub Releases published. Commits and reviews come
  from GitHub's contribution counts (commits on a repository's
  default branch, as the contribution graph counts them), comments
  from the user's comment list, and tags from each such repository's
  newest tags, so a period of any length is covered; the user's
  recent events add pushes to forks and releases. With `--private`,
  private repositories pushed to in the period are counted too, when
  the report is about the token's owner. A source GitHub fails to
  answer is a warning; the rest of the section stands. Organizations
  that refuse classic personal access tokens (meta,
  facebookincubator) withhold their items from one; the report says
  how many were withheld. Optionally scoped by organization. See
  `TODO.md` for why GitHub ships only the authored count today
  (mirror-pusher detection deferred). The opened list carries the
  same `(merged)` / `(closed)` / `(applied)` markers, rebased
  commits included
- **Forgejo / Gitea** (e.g. codeberg.org, Fedora Forge,
  src.opensuse.org): PRs opened / merged and issues opened / closed
  / commented on in the window, across every repo you contribute to.
  Sourced from the token owner's issue/pull search — or, with no
  token, the public search by username, on instances that offer one
  (Gitea does; Forgejo, Fedora Forge's, needs a token, and says so)
  — so it captures contributions to other people's repos, not just
  your own namespace; the issues commented on come from your
  activity feed. Optionally scoped by repo-owner.
  The opened list flags each PR's fate — `(merged)`, `(closed)`,
  or `(applied)` when a closed-unmerged PR's commit still landed
  on the target branch: as-is (a maintainer cherry-picked or
  fast-forwarded it instead of clicking merge), or rebased or
  reworded under a new sha, recognized by the same title *and the
  same author*. The same change redone by the maintainer stays
  `(closed)`.
- **Pagure** (e.g. Fedora's dist-git, src.fedoraproject.org): pull
  requests you filed, opened / merged in the window, with each
  opened one's fate (`(merged)`, `(closed)`). Read anonymously, by
  the profile's login for the instance (`[users.<key>.pagure]`),
  else the FAS name; a pull request merged in the window counts if
  it was opened up to 180 days before it.
- **Debian BTS** (bugs.debian.org, with `bts = true` on a domain):
  bugs filed in the window, and bugs written to in it, by the
  profile's `debian_emails`, through the tracker's SOAP interface. A
  bug counts as written to when its log has a message from one of
  those addresses dated in the window.
- **Sourcehut** (sr.ht): patches sent / applied (lists.sr.ht — the
  patch-based PR analog), tickets opened / closed (todo.sr.ht), and
  commits in your own repos (git.sr.ht), split into yours vs third-party.
  sr.ht exposes only your account's *primary* email, so list any other
  addresses you commit under in your profile's `git_emails` (or `["*"]`
  to count all) — see the config comments. Set up with
  `[domains.<d>.sourcehut] instance = "sr.ht"` and a per-user login
  under `[users.<key>.sourcehut]`; the token is a personal access token
  from meta.sr.ht/oauth2/personal-token. Because sr.ht has no unified
  PR model, each service is queried separately — and ticket metrics come
  from the *authenticated user's* activity feed, so they populate only
  when reporting on the token owner.

## Installation

```sh
cargo install sandogasa-report
```

Requires `koji` CLI for CentOS SIG reporting
(`sudo dnf install koji`).

## Usage

Four subcommands:

- `sandogasa-report config` — interactive setup of the per-user
  overlay (see [Configuration](#configuration)). Walks **every**
  forge instance used by the domains in the main config and prompts
  for the full credential set a report can need: your FAS username,
  Bugzilla email, git author emails (for Sourcehut commit
  attribution), the notes directories (see [Notes](#notes)), then
  per-instance usernames and API tokens for
  GitLab, GitHub, Forgejo/Gitea, and Sourcehut. Because it covers every
  domain in the config, one run sets you up for **any** `report -d`
  combination drawn from that config. Existing values show as
  defaults on re-runs, so only missing credentials need typing.
  Token input is hidden and needs a real terminal. Tokens go in the
  desktop keyring, one entry per instance (see
  [GitLab authentication](#gitlab-authentication)); the overlay holds
  them only where no keyring can be used. An instance whose token is
  turned off (`"-"` in the overlay) is skipped; `--all` goes over it
  too, offering the keyring's token again where it has one.
  `--missing` asks only for what is not set yet, after a new domain or
  instance is added to the main config: a field with a value and an
  instance with a stored token are kept, unchecked, and with a single
  profile it is completed without asking its name. It ends by saying
  how many settings it kept. `--token HOST` asks again only for the
  token of the instance on that host, even though one is saved, to
  replace it with one granting more or a rotated one; Enter keeps the
  saved one. The prompt says what the token needs; see
  [docs/tokens.md](../../docs/tokens.md).
- `sandogasa-report note` — record why a piece of work was done (see
  [Notes](#notes)).
- `sandogasa-report notes` — list recorded notes.
- `sandogasa-report report` — generate an activity report. Takes
  the date range, domain list, and output format.

```sh
# One-time interactive setup: asks for every credential the
# config's domains can need, so any later `report -d ...` works
sandogasa-report config -c config.toml

# Report on Fedora activity for Q1 2026
sandogasa-report report -c config.toml -d fedora \
    --user username --period 2026Q1

# Counts per repo or component under each heading; --detailed twice lists the items
sandogasa-report report -c config.toml -d fedora \
    --user username --period 2026Q1 --detailed

# Multiple domains in one report. Each domain is its own section
# (with Bodhi/Koji/GitLab/GitHub nested beneath), in the order
# given. Bugzilla is aggregated per instance into one section each,
# placed after the last domain that uses that instance.
sandogasa-report report -c config.toml -d fedora -d hyperscale \
    --user username --period 2026Q1

# Arbitrary date range (inclusive)
sandogasa-report report -c config.toml -d epel \
    --user username --since 2026-01-01 --until 2026-06-30

# Full year, half year, and quarter periods
sandogasa-report report -c config.toml -d hyperscale --period 2025
sandogasa-report report -c config.toml -d hyperscale --period 2025H2
sandogasa-report report -c config.toml -d hyperscale --period 2025Q4

# JSON output to file
sandogasa-report report -c config.toml -d fedora \
    --user username --period 2026Q1 --json -o report.json

# Skip specific data sources for faster testing
sandogasa-report report -c config.toml -d fedora \
    --user username --period 2026Q1 --no-bugzilla --no-bodhi
```

### Useful flags

- `-c, --config <PATH>` — path to config file (required)
- `-d, --domain <DOMAIN>` — domain(s) to report on (repeatable;
  per-domain sections are rendered in the order given)
- `-u, --user <USER>` — FAS username to report on
- `--period <PERIOD>` — reporting period (2026, 2026H1, 2026Q1)
- `--since <DATE>` / `--until <DATE>` — date range (inclusive)
- `--detailed` — counts per repo, project or component under each
  heading; `--detailed --detailed` lists the items themselves (every
  PR, bug, patch and ticket, and every build of a Bodhi update)
- `--by <AXIS>` — append a breakdown grouped that way (repeatable or
  comma-separated). `repo` is the only axis: one row per repository,
  summed across every domain and forge the report covers, busiest
  first. It regroups what was already collected, so it costs no extra
  queries. See [By repository](#by-repository).
- `--json` — output as JSON instead of Markdown
- `-o, --output <PATH>` — write output to file
- `--no-bugzilla` / `--no-bodhi` / `--no-koji` / `--no-gitlab` /
  `--no-github` / `--no-forgejo` — skip data sources
- `-v, --verbose` — print progress to stderr

### By repository

A report is collected domain by domain and service by service, which
answers "what did this person do on GitLab" well and "what happened to
this repository" not at all — the answer is spread across every
section. `--by repo` appends a table that regroups it:

```console
$ sandogasa-report report -c config.toml -d hyperscale --period 2026Q3 --by repo
```

```markdown
## By repository

| Repository | Opened | Merged | Commits |
| --- | ---: | ---: | ---: |
| `gitlab.com/CentOS/Hyperscale/rpms/perf` | 4 | 3 | 12 |
| `github.com/slopfest/sandogasa` | 2 | 2 | 61 |
```

A repository is keyed by host and path together, so the same
`owner/name` on two forges stays two rows. Columns no repository used
are dropped rather than printed as a line of zeroes. Issues counts the
issues opened or commented on (GitHub), opened, closed or commented on
(GitLab), and opened or closed (Forgejo/Gitea), each once. A project two
domains cover (one's `group`, another's `extra_groups`) is counted in
each. Commits are the ones pushed, which is what the per-forge sections
already report — a push can carry work the pusher did not write, so
pushed and authored counts are not added together.

To have it on every run, pin it as a flag default (see
[System-wide configuration](#system-wide-configuration)):

```toml
[defaults]
by = ["repo"]
```

`--no-defaults` ignores that for a single run. It is not on by
default: these reports are often committed and diffed, so the output
shape does not change under anyone who did not ask for it.

## Notes

A report lists what was done, not why: "reviewed to unblock X",
"rebuilt for Y's Change". `note` keeps that reason next to the item,
one file per note:

```sh
sandogasa-report note --url https://github.com/o/r/pull/12 \
    Reviewed to unblock the foo update
# No URL, for work no report lists; the text from standard input
sandogasa-report note --date 2026-11-17 - < mentoring.txt
```

The note is shown with the file it goes to and written once
confirmed, asked at the terminal even when the text comes from
standard input; `--yes` writes it without asking, and is needed where
there is no terminal at all. `--url` and `--by` take one line each. `--by` names who wrote the text
when it was not you, such as an assistant (`Claude Code:<model>`).

Each note is a file of header lines, a blank line, then the text:

```text
url: https://github.com/o/r/pull/12
date: 2026-11-18
by: Claude Code:claude-opus-5-5

Reviewed to unblock the foo update.
```

Files are named `<year>/<date>T<HHMM>-<host>.md` after the local time
they were written and the machine's short host name, with `-2`, `-3`
for more in the same minute, so machines syncing the directory never
write the same file. Headers other than `url`, `date` and `by` are
kept as they are. Notes go in `[notes] dir` from the per-user overlay (`config` asks for
it), an absolute path or one starting with `~/` for the home
directory, else
`~/.local/share/sandogasa-report/notes`:

```toml
[notes]
dir = "~/Nextcloud/projects/notes/sandogasa-report"
# dirs = ["~/team-notes"]           # more to read, after dir
# private = ["~/src/embargoed/notes"]
# unattached = "ask"   # include (default), exclude or ask
```

Notes are read from `dir`, then `dirs`, then `private`. A private
directory is one kept off any synced folder, for notes on work that must
not be public yet, such as an embargoed security fix; `config` asks for
them, comma-separated, and one inside a public directory, or holding
one, is refused. Its notes show only in a report run with
`--private`. `note --embargo <until>`, a day (YYYY-MM-DD) or
`disclosed`, writes an `embargo: until <until>` header and puts the
note in the first private directory, refusing a public one; `--dir`
picks which configured directory a note goes in, the first public one
by default.

Once an embargo has lifted, its day having come or its header edited to
`embargo: lifted <date>`, `notes --release` lists the notes in private
directories that it covers and, after asking (`--yes` does not ask),
moves them to the first public directory, the header saying
`lifted <today>` unless it already says when. `report --notes` says how
many such notes are still waiting.

`report --notes` adds the notes dated in the report's period. Each note
goes under the first item in the report that links its URL, or, for a
commit or a repository's own URL, under the repository's line in that
forge's commit counts, since reports count commits rather than list
them; items show only with `--detailed --detailed`. URLs match when they
point at the same item: the scheme, the host's case, a trailing slash, a
fragment and tracking parameters (`utm_*`) do not count, nor the path's
case on GitHub and GitLab; a Bugzilla bug matches in any form (`rhbz#N`,
`/N`, `show_bug.cgi?id=N` with other parameters), a GitHub API URL
matches the web page, and a GitLab work item (`/-/work_items/N`) its
issue page (`/-/issues/N`). Notes no item took close the report in a
section of their own, "Notes on items not in this report", grouped by
the host their URL points at, those without a URL last, so the sources a
report does not cover show at a glance. `--unattached-notes exclude`
leaves them out of the Markdown report and `ask` asks (including them
without a terminal); the default is `include`, or what `[notes]
unattached` in the config says. Below `--detailed --detailed`, some
items are counted rather than listed, so their notes land in this
section too. With `--json`, the notes come as a `notes` array beside
`report`, each with the `key` its URL is matched by and whether an item
took it (`attached`), whatever `--unattached-notes` says.

A report is made to be shared unless `--private` is given: a note then
shows only under an item the printed report shows (so it is no more
private than that item's line; with `--json`, under any item the JSON
carries), never one with an `embargo:` header unless it says `lifted`
or `until` a day that has come, and never one from a private directory. Notes about
anything else, and those without a URL, are left out, so a shared report has no section of leftover notes, and
the count is printed on standard error. At the default detail no items
are listed, so a shared report needs `--detailed --detailed` to carry
notes. `--private` carries them all, for your own use such as a
self-review.

On GitHub the same switch covers the items themselves. A shared report
searches for PRs with `is:public`, counts commits, reviews and
comments only in repositories that are public now, and keeps only
events (releases, and pushes to forks) that were public when they
happened and are in a repository that is public now; how many events
it left out is printed on standard error. `--private` includes them,
and counts commits in the private repositories pushed to in the
period. The GitLab, Forgejo/Gitea, Sourcehut,
Bugzilla, Bodhi and Koji sections include whatever your tokens can see,
with or without `--private`.

`notes` lists them oldest first, from every directory, marking those in
a private one, filtered with `--since`, `--until` and `--url`, or as
JSON with `--json` (with `private` for each). A file that cannot be
read as a note is skipped with a warning.

## Configuration

Configuration is layered: a **main config** passed via `-c` holds
the shared structure (domains, groups, koji tags, GitLab instance
URLs), and a **user overlay** at
`~/.config/sandogasa-report/config.toml` is auto-loaded and
deep-merged on top. Overlay values win at every nesting level;
missing overlay keys leave the main value unchanged.

This lets a team check in one shared `config.toml` and each user
keep their personal bits (GitLab usernames, Bugzilla emails,
instance-specific tweaks) in their own home config.

See `configs/sandogasa-report/config.toml` for a full main-config
example.

```toml
# FAS username → Bugzilla email mapping.
# If not set, looked up via FASJSON (requires Kerberos).
[users]
# username = "user@example.com"

# Domain presets define which data sources to query.
[domains.fedora]
bugzilla = true
bodhi = true
bodhi_releases = ["F*", "EPEL-*"]
fedora_versions = [42, 43, 44]

[domains.epel]
bugzilla = true
bodhi = true
bodhi_releases = ["EPEL-*"]

# Another Bugzilla: a table instead of `true`, with its products.
# A stock Bugzilla parks done bugs at RESOLVED, so that counts as
# closed there (Red Hat's closes outright); `closed_statuses`
# overrides either default.
[domains.opensuse.bugzilla]
instance = "https://bugzilla.opensuse.org"
products = ["openSUSE Tumbleweed", "openSUSE Distribution"]

[domains.hyperscale]
koji_profile = "cbs"
koji_tags = [
    "hyperscale{9,10}{,s}-packages-{main,facebook}-release",
]
[domains.hyperscale.gitlab]
instance = "https://gitlab.com"
group = "CentOS/Hyperscale"
# More prefixes this domain counts, for its work outside the group
# (a CentOS Stream merge request, a project in another group); `*`
# matches within a path segment (`CentOS/*/rpms/perf`). Without
# `group`, these alone are the domain's scope:
# extra_groups = ["redhat/centos-stream/rpms/perf"]
# user = "michel-slm"  # optional, if GitLab username ≠ FAS login

[domains.debian.gitlab]
instance = "https://salsa.debian.org"
# No group filter → all user activity on this instance counts.
user = "michel"  # salsa username often differs from FAS login

[domains.upstream.forgejo]
instance = "https://codeberg.org"
# No owner filter → PRs across every repo you contribute to.

# Package groups for categorical reporting.
# Group keys are prettified for headings (e.g. "developer-tools"
# becomes "Developer Tools"). Optional description appears below.
[groups.hardware-enablement]
description = "Hardware enablement and GPU support"
packages = ["intel-gpu-tools", "libdrm", "mesa"]

[groups.developer-tools]
packages = ["neovim", "helix", "fish"]
```

### Per-user overlay

A typical overlay at `~/.config/sandogasa-report/config.toml`
might look like:

```toml
# A profile ties together the usernames one person uses across
# services. `sandogasa-report report --user michel` then looks
# this up and queries each backend with the right identity.
[users.michel]
fas = "salimma"                       # default if omitted: the profile key
bugzilla_email = "michel@example.com" # optional; FASJSON fallback otherwise

# Emails on other Bugzilla instances, by host. No fallback: FASJSON
# only knows Red Hat's.
[users.michel.bugzilla_emails]
"bugzilla.opensuse.org" = "michel@example.com"

[users.michel.gitlab]
"gitlab.com" = "michel-slm"
"salsa.debian.org" = "michel"

[users.michel.github]
"api.github.com" = "michel-slm"

[users.michel.forgejo]
"codeberg.org" = "michelin"

# API tokens, only where no keyring can be used — otherwise
# `sandogasa-report config` keeps them in the keyring. Env vars
# and the keyring still win if set.
[gitlab_tokens]
"gitlab.com" = "glpat-..."
"salsa.debian.org" = "glpat-..."

[github_tokens]
"api.github.com" = "ghp_..."

[forgejo_tokens]
"codeberg.org" = "..."
```

### GitHub domain shorthand

The full `[domains.X.github]` block accepts both `instance`
(the API base URL) and `org` (a namespace filter). Both have
sensible defaults, so a minimal config can elide them:

```toml
# All your github.com activity, any owner.
[domains.upstream.github]

# Same, scoped to one org.
[domains.upstream.github]
org = "slopfest"

# A GHES instance — `instance` is only worth specifying when
# you're not on github.com.
[domains.work.github]
instance = "https://github.enterprise.example/api/v3"
org = "platform-team"
```

### Forgejo domain shorthand

`[domains.X.forgejo]` takes the instance root URL and an optional
`owner` (repo-owner filter, the Forgejo analogue of GitHub's
`org`). With a token, the report queries the **token owner's** pull
requests, so it captures contributions to anyone's repo — the usual
case for upstream work on codeberg.org — and the per-user login is
only for display. Without a token, it runs the public search by that
login instead (`[users.<key>.forgejo] "src.opensuse.org" =
"Pharaoh_Atem"`, falling back to the FAS login), which works on an
instance whose API answers anonymously, src.opensuse.org among them;
an unknown login is an error, not an empty report.

```toml
# Every repo you've opened/merged PRs in on codeberg.org.
[domains.upstream.forgejo]
instance = "https://codeberg.org"

# Scoped to one repo-owner (user or org).
[domains.kernel.forgejo]
instance = "https://codeberg.org"
owner = "ptesarik"
```

### Koji tag patterns

Tag patterns support shell-style brace expansion:
`hyperscale{9,10}{,s}-packages-main-release` expands to all
combinations (hyperscale9-packages-main-release,
hyperscale9s-packages-main-release, etc.).

### GitLab authentication

Each GitLab instance needs an API token. The tool looks up the
token from, in order:

1. `GITLAB_TOKEN_<HOSTNAME>` — instance-specific, hostname dots
   replaced by underscores and uppercased. For `gitlab.com` this
   is `GITLAB_TOKEN_GITLAB_COM`; for `salsa.debian.org` it's
   `GITLAB_TOKEN_SALSA_DEBIAN_ORG`.
2. `GITLAB_TOKEN` — the generic fallback shared with other
   sandogasa tools.
3. The desktop keyring's entry for the instance — where
   `sandogasa-report config` keeps the token, so it survives across
   shell sessions without re-exporting env vars. The entry is named
   for the instance's host and shared with every sandogasa tool
   talking to it; check it with
   `secret-tool lookup application sandogasa service gitlab.com`.
4. `gitlab_tokens.<hostname>` in the overlay — where `config` keeps
   the token when no keyring can be used.

Env vars win over the rest, so a one-off shell override still
works even with a persisted token. The overlay file is kept at
`~/.config/sandogasa-report/config.toml` with 0600 permissions.

Because the report is about a named user's own activity, and the
events endpoint returns only what the requesting token may see, use
that user's token. Create one at `Preferences → Access tokens` (on
gitlab.com, <https://gitlab.com/-/user_settings/personal_access_tokens>).

Grant **`read_api`**, and nothing more. Every call the tool makes is
a GET:

| endpoint | what it is for |
|---|---|
| `/api/v4/users?username=` | resolve the username to an id |
| `/api/v4/users/<id>/events` | the activity the report is built from |
| `/api/v4/projects/<id>` | the project path an event belongs to |
| `/api/v4/projects/<id>/repository/commits` | authored commit counts |
| `/api/v4/projects/<id>/repository/tags` | tag names for batched tag pushes |
| `/api/v4/projects/<id>/releases` | releases published |
| `/api/v4/user` | `sandogasa-report config` only, to validate the token |

`api` is read *and write*, and nothing here writes. `read_user` is
not needed either, despite `/user` and `/users` being the endpoints
its description names: GitLab allows any GET under `read_api` at the
top of the API, and an endpoint permits a token carrying any one of
its allowed scopes, so `read_api` already covers them.

### GitHub authentication

Same lookup shape as GitLab. The instance-specific env var is
`GITHUB_TOKEN_<HOSTNAME>` (for `api.github.com` →
`GITHUB_TOKEN_API_GITHUB_COM`); the generic `GITHUB_TOKEN`
matches the convention the `gh` CLI already uses. Then the
keyring's entry for the API host (`service api.github.com`, or a
GitHub Enterprise server's host), then the overlay's
`[github_tokens]` table.

**Where to create a token.** GitHub's settings tree buries
this; the direct URLs are easier:

- **Fine-grained PATs** (recommended, and the only kind some
  organizations, such as meta and facebookincubator, accept): <https://github.com/settings/personal-access-tokens>
  → *Generate new token*. Pick "All repositories" or a specific
  org/repo set, then under *Repository permissions* grant
  read-only on:
  - **Contents** — commit listing
  - **Pull requests** — PR search + reviews
  - **Metadata** — always required
  Account permissions can stay empty. Token starts with
  `github_pat_…`.
- **Classic PATs**: <https://github.com/settings/tokens> →
  *Generate new token (classic)*. No scope for public work, which
  the token only lets run at a higher rate limit; `repo` for
  `--private`, to count private repositories. Token starts with
  `ghp_…`. Both PAT types work identically
  against the API; fine-grained is preferable because revoking
  one org's access doesn't affect the others.

### Forgejo authentication

Same lookup shape as the other forges. The instance-specific env
var is `FORGEJO_TOKEN_<HOSTNAME>` (for `codeberg.org` →
`FORGEJO_TOKEN_CODEBERG_ORG`); the generic `FORGEJO_TOKEN` is the
fallback; then the keyring's entry for the instance, then the
overlay's `[forgejo_tokens]` table.

Because the report queries "PRs *I* created", the token's owner is
who the report is about — use your own token. Create one at your
instance's `Settings → Applications → Access Tokens` (on codeberg,
<https://codeberg.org/user/settings/applications>). A token is
optional where the instance's API answers anonymously: then the
report searches by login (see the domain shorthand above).

Forgejo tokens are **scoped by category** (activitypub, issue, misc,
notification, organization, package, repository, user — each
read/write). Grant, at read level:

- **`read:repository`** — the PR search lives under the `/repos` API
  group.
- **`read:issue`** — pull requests are issues; the search is an issue
  endpoint.
- **`read:user`** — the report reads `/api/v1/user` to find whose
  activity feed lists the issues you commented on, and `config` to
  check the token. Without it the report leaves that list out, with a
  warning.

(Forgejo reports a missing category as `token does not have at least
one of required scope(s)`.) [docs/tokens.md](../../docs/tokens.md)
lists what the other tools sharing the token need.

### FTBFS/FTI tracking

Bugzilla bugs that block known FTBFS (`F{ver}FTBFS`,
`RAWHIDEFTBFS`) or FTI (`F{ver}FailsToInstall`,
`RAWHIDEFailsToInstall`) trackers are classified separately.
Set `fedora_versions` on the domain to specify which Fedora
release trackers to look up.

## System-wide configuration

Settings are read from `/etc/sandogasa-report/config.toml` first, then
overridden per key by `~/.config/sandogasa-report/config.toml`, with
command-line flags overriding both. A system file alone is enough — no
per-user file is required — and either may also carry a `[defaults]`
table pinning flag defaults (see the root `DEVELOPMENT.md`).

`sandogasa-report config` writes the user file only, with 700 on the
directory and 600 on the file. Nothing writes under `/etc`: a system
file is admin-authored, holds shared non-secret settings, and is
normally shipped `root:root 0644`.

It asks for one profile's settings at a time, each with its saved value
as the default: Enter keeps it, a new value replaces it, and `-` clears
an optional one. The Bugzilla email is checked as an address. A saved
token is checked with its forge first and only asked for again if the
forge refuses it.

Credentials belong in the per-user file, which is 600, or in an
environment variable — a token under `/etc` is readable by every local
user. For an unattended machine, give the job its own user and its own
600 config.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
