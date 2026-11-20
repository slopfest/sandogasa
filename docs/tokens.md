<!-- Generated from crates/sandogasa-config/src/credentials.rs by `make tokens`; edit the table there, not this file. -->

# Tokens and logins

What each tool needs from each service, and where to set it up. Each tool's `config` command says the same for its own tokens.

A token or key is kept in the desktop keyring (GNOME Keyring, or KWallet with its Secret Service support), one entry per service and host, labeled "sandogasa: <kind of token>", and shared by every tool that uses that service. Without a keyring it goes in the tool's own config file. A token one tool needs more from than another must grant the most any tool you run needs; if you run only one, grant what that one needs.

To replace one token, with one granting more or a new one: `<tool> config --token <host>` asks for it again, saying what to grant.

From a terminal:

- `secret-tool lookup application sandogasa service <host>` shows an entry
- `secret-tool clear application sandogasa service <host>` removes it, for every tool that uses it; to stop one tool using it, clear its token in that tool's `config` instead (enter `-`)

## By tool

What each tool needs; the sections below say where to create each token and what the other tools sharing it need.

| Tool | Tokens and API keys | Logins |
| --- | --- | --- |
| cbspkg | [GitLab](#gitlab): api; Maintainer in the SIG's group for sig-repo | CentOS Build System certificate |
| cpu-sig-tracker | [GitLab](#gitlab): api, in a legacy (not fine-grained) token<br>[Jira](#jira): read:jira-user, read:jira-work; write:jira-work to file issues | CentOS Build System certificate<br>centpkg token |
| dbranch | none | Debusine<br>glab |
| ebranch | [Bugzilla](#bugzilla): an API key (Red Hat's; RPM Fusion's for its rebuild requests)<br>[Forgejo](#forgejo): issue Read and Write, organization Read<br>[GitHub](#github): no scopes<br>[NVD](#nvd): an API key | bodhi CLI session<br>copr-cli<br>fedpkg |
| fedora-cve-triage | [Bugzilla](#bugzilla): an API key, to change bugs (reads work without one)<br>[GitHub](#github): no scopes<br>[NVD](#nvd): an API key | bodhi CLI session |
| fedora-review-digest | [Bugzilla](#bugzilla): an API key, for --post | none |
| fesco-chair | [Forgejo](#forgejo): issue Read and Write, repository Read, user Read | Fedora Kerberos ticket |
| hs-intake | none | none |
| hs-meetings | none | none |
| hs-relmon | [GitLab](#gitlab): api | CentOS Build System certificate |
| koji-diff | none | none |
| koji-lag | none | none |
| poi-tracker | [Bugzilla](#bugzilla): an API key, for the triage commands<br>[Fedora dist-git (Pagure)](#fedora-dist-git-pagure): the "Modify an existing project" ACL | none |
| sandogasa-hattrack | none | Fedora Kerberos ticket |
| sandogasa-pkg-acl | [Fedora dist-git (Pagure)](#fedora-dist-git-pagure): the "Modify an existing project" ACL | none |
| sandogasa-pkg-health | none | none |
| sandogasa-report | [Forgejo](#forgejo): issue Read, repository Read, user Read<br>[GitHub](#github): no scopes; repo (classic) for --private<br>[GitLab](#gitlab): read_api<br>[Sourcehut](#sourcehut): read-only; all services, or git.sr.ht, lists.sr.ht, todo.sr.ht and meta.sr.ht | Fedora Kerberos ticket |
| specport | none | none |
| timebook | none | Fedora Kerberos ticket |

## Tokens and API keys

### Bugzilla

An API key, created at `https://<instance>/userprefs.cgi?tab=apikey`. A Bugzilla API key has no scopes: it can do whatever your account can.

| Tool | Needs | For |
| --- | --- | --- |
| ebranch | an API key (Red Hat's; RPM Fusion's for its rebuild requests) | files, updates and closes branch and rebuild requests, sets needinfo |
| fedora-cve-triage | an API key, to change bugs (reads work without one) | closes, comments on and reassigns CVE bugs, sets needinfo |
| fedora-review-digest | an API key, for --post | comments on review bugs and sets their fedora-review flag |
| poi-tracker | an API key, for the triage commands | closes and comments on bugs, changes their priority |

For all of these tools:

- an API key for each Bugzilla you use (Red Hat's; RPM Fusion's for ebranch)

### Forgejo

An access token, created at `https://<instance>/user/settings/applications`. Each scope is a category, set to No access, Read, or Read and Write.

| Tool | Needs | For |
| --- | --- | --- |
| ebranch | issue Read and Write, organization Read | escalate files releng tickets with the tracker's labels |
| fesco-chair | issue Read and Write, repository Read, user Read | reads the agenda, comments on, labels and closes tickets |
| sandogasa-report | issue Read, repository Read, user Read | searches your pull requests and issues, reads your activity feed |

For all of these tools:

- issue: Read and Write
- organization: Read
- repository: Read
- user: Read

### GitHub

A personal access token, created at <https://github.com/settings/personal-access-tokens>. No tool writes to GitHub. A token with no scopes raises the rate limit (60 to 5000 requests an hour); classic tokens are at https://github.com/settings/tokens.

| Tool | Needs | For |
| --- | --- | --- |
| ebranch | no scopes | looks up security advisories; it has no prompt, and reads the entry another tool saved, or GITHUB_TOKEN |
| fedora-cve-triage | no scopes | looks up security advisories and repository languages |
| sandogasa-report | no scopes; repo (classic) for --private | reads your pull requests, issues and events; --private counts private repositories |

For all of these tools:

- no scopes
- repo (classic), to report on private repositories

### GitLab

A personal access token, created at `https://<instance>/-/user_settings/personal_access_tokens`.

| Tool | Needs | For |
| --- | --- | --- |
| cbspkg | api; Maintainer in the SIG's group for sig-repo | creates SIG repositories, comments on and labels tracking issues |
| cpu-sig-tracker | api, in a legacy (not fine-grained) token | files, labels and comments on tracking issues, opens merge requests |
| hs-relmon | api | files, labels and comments on issues, archives retired projects |
| sandogasa-report | read_api | reads your events, merge requests, issues, tags and releases |

For all of these tools:

- api, in a legacy (not fine-grained) token

### Jira

An Atlassian API token, created at <https://id.atlassian.com/manage-profile/security/api-tokens>. Entered with your Atlassian email; public issues read without one.

| Tool | Needs | For |
| --- | --- | --- |
| cpu-sig-tracker | read:jira-user, read:jira-work; write:jira-work to file issues | reads RHEL issues, and files them with jira --file |

### NVD

An API key, created at <https://nvd.nist.gov/developers/request-an-api-key>. Optional: a key raises the rate limit from 5 to 50 requests per 30 s.

| Tool | Needs | For |
| --- | --- | --- |
| ebranch | an API key | check-update judges CVE bugs by their NVD records |
| fedora-cve-triage | an API key | reads NVD records for the CVEs it triages |

For all of these tools:

- an API key

### Fedora dist-git (Pagure)

An API token, created at <https://src.fedoraproject.org/settings/token/new>.

| Tool | Needs | For |
| --- | --- | --- |
| poi-tracker | the "Modify an existing project" ACL | adopt takes orphaned packages; act orphans or gives them away |
| sandogasa-pkg-acl | the "Modify an existing project" ACL | sets and removes access, gives and takes packages |

For all of these tools:

- the "Modify an existing project" ACL

### Sourcehut

A personal access token, created at <https://meta.sr.ht/oauth2/personal-token>. Tick "read-only access token": every grant is then read-only.

| Tool | Needs | For |
| --- | --- | --- |
| sandogasa-report | read-only; all services, or git.sr.ht, lists.sr.ht, todo.sr.ht and meta.sr.ht | reads your repositories, patchsets, tickets and profile email |

## Logins the tools check

These are not kept by sandogasa: each is another program's login, or a ticket or certificate. The tools check them before working and say how to set them up when they are missing.

| Login | Used by | Setup |
| --- | --- | --- |
| bodhi CLI session | ebranch check-update --give-karma and --submit, update-bugs: karma, new updates, bug lists; fedora-cve-triage --edit-bodhi | Run any authenticated bodhi command once (`bodhi overrides query --mine`); its login is cached in ~/.config/bodhi/client.json. |
| CentOS Build System certificate | cbspkg build, promote, untag, upload-sources, add-pkg; cpu-sig-tracker untag; hs-relmon's untagging commands | `kinit <you>@FEDORAPROJECT.ORG`, then `centos-cert -u <you>`, which writes ~/.centos.cert; `centos-cert -v` checks it. cbspkg offers to run it when CBS refuses the login. |
| centpkg token | cpu-sig-tracker mr (`centpkg fork`) | An API token in ~/.config/rpkg/centpkg.conf, as centpkg documents. |
| copr-cli | ebranch's Copr pruning (`copr-cli delete-package`) | copr-cli's own API token, in ~/.config/copr. |
| crates.io | releasing sandogasa (`cargo ws publish`) | `cargo login`, which keeps the token in the desktop keyring when ~/.cargo/config.toml sets `global-credential-providers = ["cargo:libsecret"]`. |
| Debusine | dbranch --debusine uploads | Install debusine-client and run `debusine setup`. |
| Fedora Kerberos ticket | timebook cancel and edit, sandogasa-hattrack away (Fedora Calendar); fesco-chair votes, sandogasa-hattrack and sandogasa-report (FASJSON reads) | `fkinit -u <you>` or `kinit <you>@FEDORAPROJECT.ORG`, with your FAS username in ~/.fedora.upn. |
| fedpkg | ebranch file-requests (`fedpkg request-branch`) | fedpkg's own Pagure token (`fedpkg set-distgit-token`). |
| glab | dbranch on Salsa: pushes, CI, new projects | `glab auth login --hostname salsa.debian.org`; a gitlab.com login does not cover Salsa. |
