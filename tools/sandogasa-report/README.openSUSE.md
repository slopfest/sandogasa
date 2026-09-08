# sandogasa-report on openSUSE

How to report on openSUSE contributions with `sandogasa-report`, and
where each of openSUSE's services stands. Two of the five work today
and need no openSUSE credentials; the other three are documented here
so the setup is clear when they land. The tool's general configuration
is in [README.md](README.md); this page only adds what openSUSE needs.

| Service | Software | Status |
|---|---|---|
| bugzilla.opensuse.org | Bugzilla 5 | works, anonymous |
| src.opensuse.org | Gitea | works, anonymous |
| build.opensuse.org | Open Build Service | not yet: needs an account |
| code.opensuse.org | Pagure | not yet: API blocked by a challenge page |
| lists.opensuse.org | Mailman 3 / HyperKitty | not yet: archive API too slow |

## A domain for openSUSE

One domain carries every openSUSE source. In the main config:

```toml
[domains.opensuse.bugzilla]
instance = "https://bugzilla.opensuse.org"
products = ["openSUSE Tumbleweed", "openSUSE Distribution"]

[domains.opensuse.forgejo]
instance = "https://src.opensuse.org"
```

and in the per-user overlay (`~/.config/sandogasa-report/config.toml`),
the identities openSUSE knows you by:

```toml
[users.me]
fas = "yourfaslogin"

[users.me.bugzilla_emails]
"bugzilla.opensuse.org" = "you@example.org"

[users.me.forgejo]
"src.opensuse.org" = "your_opensuse_login"
```

Then:

```sh
sandogasa-report report -d opensuse --user me --since 2026-01-01 --until 2026-06-30
sandogasa-report report -d fedora -d epel -d opensuse --user me --period 2026Q1
```

The second form puts Fedora and openSUSE in one report: each Bugzilla
gets its own section (`## Bugzilla (bugzilla.redhat.com)`, `## Bugzilla
(bugzilla.opensuse.org)`), placed after the last domain that uses it.

## Bugzilla (bugzilla.opensuse.org)

Bugs you filed, and bugs assigned to you that were closed, in the
configured products, split into security, update requests, branch
requests and the rest. The Fedora-only sections — package reviews,
FTBFS/FTI — do not appear: openSUSE reviews packages in OBS, not in
Bugzilla.

- **Identity:** the email your Bugzilla account uses, under
  `bugzilla_emails."bugzilla.opensuse.org"`. There is no directory
  lookup (FASJSON only knows Red Hat's), so an unset email is an
  error naming the key.
- **Closed means RESOLVED.** A stock Bugzilla parks finished bugs at
  `RESOLVED` and seldom moves them to `CLOSED`, so `RESOLVED`,
  `VERIFIED` and `CLOSED` all count as closed here. Set
  `closed_statuses` on the domain's `bugzilla` table to change that.
- **Products** are yours to pick. `openSUSE Tumbleweed` and `openSUSE
  Distribution` hold packaging bugs; `openSUSE.org` holds OBS project
  and package requests (a new devel project, a package's home), and
  `openSUSE Backports` the Leap backports. Add whichever you file in.
- No credentials: the REST API answers anonymously for public bugs.

## Gitea (src.opensuse.org)

Pull requests you opened and merged, and issues you opened and closed,
across every repository on the instance — the `pool/` and `obs/`
packaging repositories included.

- **Identity:** your login on src.opensuse.org, under
  `forgejo."src.opensuse.org"`. It is not your FAS login unless you
  chose the same name; the tool falls back to the FAS login when the
  entry is missing and reports an unknown login as an error rather
  than an empty section.
- **No token needed.** The instance's search API answers anonymously,
  and the report searches by login (`created_by`). With a token in
  `forgejo_tokens."src.opensuse.org"` or `FORGEJO_TOKEN_SRC_OPENSUSE_ORG`
  the report switches to "what the token owner created", the same as
  on codeberg.org; that also covers private repositories you can see.

## Open Build Service (build.opensuse.org) — not yet

OBS is where openSUSE packaging actually happens: submit requests,
reviews, maintenance requests. It is the biggest missing piece.

- The API at `api.opensuse.org` answers `/about` anonymously but
  every request query (`/request?view=collection&user=…`) is 401
  without credentials, so this source needs an openSUSE account —
  the same login as build.opensuse.org — and an `osc`-style
  credential (password or SSH-signature auth).
- Planned shape: a `sandogasa-obs` client crate, `[domains.opensuse.obs]
  instance = "https://api.opensuse.org"`, a per-user login under
  `[users.me.obs]`, and the credential collected by `sandogasa-report
  config` like the forge tokens. The report would count submit
  requests created, accepted and reviewed in the window.
- Until then, build.opensuse.org's own pages show a user's requests:
  `https://build.opensuse.org/users/<login>`.

## Pagure (code.opensuse.org) — not yet

code.opensuse.org hosts openSUSE's infrastructure and tooling repos.
The Pagure API is the one `sandogasa-distgit` already speaks for
src.fedoraproject.org, so the client exists, but the site sits behind
an anti-bot proof-of-work challenge that answers API calls with a 403
and an HTML page asking for JavaScript. Until the API paths are exempt,
or a token bypasses the challenge, nothing can be fetched.

## Mailing lists (lists.opensuse.org) — not yet

The HyperKitty archive API is there and lists the lists
(`/archives/api/lists/`), but its per-list message endpoint
(`/archives/api/list/<list>/emails/`) did not answer within two minutes
for `packaging@`, and the threads endpoint took over thirty seconds,
where lists.fedoraproject.org answers the same call in under a second.
A per-run scan of several lists at that pace is not usable, so a
Mailman source — which would serve Fedora's lists too — waits on a
cheaper way to find one sender's posts, or on the archive speeding up.
