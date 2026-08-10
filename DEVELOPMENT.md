# Development Notes

Cross-cutting gotchas for the whole workspace. Crate-specific notes
live next to the crate (e.g. `crates/sandogasa-distgit/DEVELOPMENT.md`,
`crates/sandogasa-fedrq/DEVELOPMENT.md`).

## Fedora infrastructure is flaky — code defensively

Fedora services (src.fedoraproject.org / Pagure, Bodhi, Koji,
Bugzilla, the mirror network) routinely return transient 5xx errors
or drop connections under load. Any client code talking to them must
assume a request can fail once and succeed on an immediate retry.
Concretely:

- **Retry transient failures on GETs.** Retry 500/502/503/504 and
  transport errors (connection reset, timeout, DNS) with backoff.
  `sandogasa-distgit` has `get_transient_retry`/`get_with_retry` for
  this; reuse or replicate the pattern in other HTTP clients.
- **Only a 404 means "does not exist".** Never fold "the request
  failed" into "the resource is absent": an existence check that maps
  any non-2xx status to `false` turns a Pagure hiccup into a
  confidently wrong answer. Observed live (2026-07):
  `sandogasa-pkg-acl set --user mikelo2 --level commit yq` reported
  "user 'mikelo2' does not exist on dist-git", and the identical
  rerun seconds later succeeded — the check had mapped a transient
  error to `false`.
- **Word errors as "check failed", not as a negative result.** Prefer
  "could not verify user 'x' exists on dist-git: 502 Bad Gateway"
  over "user 'x' does not exist", so users don't act on phantom
  state.
- **Don't blanket-retry mutating requests** (POST/PATCH): the failed
  request may still have taken effect server-side. Surface the error
  and let the user rerun. (Our ACL modifications happen to be
  idempotent, but don't assume that in general.)
- **Heavy requests to Fedora infrastructure need three defenses**
  (all reproduced live with curl against koji.fedoraproject.org's
  multi-MB `listTasks` XML-RPC responses, 2026-07; encoded in
  `sandogasa_kojihub::xmlrpc::Client`):
  1. **Always send a User-Agent** — UA-less heavy requests are
     tarpitted (>140s, no response) where the identical request
     with a UA completes. reqwest sends no User-Agent by default;
     every client crate should set `<crate>/<version>`.
  2. **Don't reuse keep-alive connections for heavy queries** —
     the same query succeeds on a fresh connection (3–90s) and
     times out on a reused one (curl `--next` reproduces it).
     `pool_max_idle_per_host(0)` forces fresh connections.
  3. **Prefer HTTP/1.1** (`http1_only`) — h2 negotiation showed
     additional hangs during the same testing, and it matches the
     python koji CLI's behavior.
  Relatedly, print reqwest errors with their source chain (see
  `sandogasa_kojihub::xmlrpc::Error`) — the top-level Display is
  just "error sending request", hiding the timeout/reset detail
  that identifies problems like these.

## "Not there" and "did not ask" are different answers

The 404 rule above is one case of a wider one: whenever a lookup can
fail to produce an answer, the code has to keep *absent* apart from
*unknown*. Folding them together always fails in the same direction —
silently, and looking like a real negative result. Three instances
turned up in one day:

- **A field you deserialize is a field you must request.** Bugzilla's
  default field set omits `flags`, and `Bug` declares that field
  `#[serde(default)]`, so a read that did not ask for flags produced
  an empty `Vec` — indistinguishable from a bug that genuinely has
  none, and read as "this review is not approved" for every approved
  review. `BzClient` now sends `include_fields=_default,flags` on
  every bug read, with a wiremock test asserting it is on the wire.
  `_default` keeps the whole default set, so adding a field to that
  list can never take one away.
- **An identifier that matches nothing is not an empty result.**
  poi-tracker searched Bugzilla with `version=f43` where Fedora
  numbers its versions bare (`43`), so every numbered-branch
  retirement reported no open bugs. The fix was as much about
  `product_version_for_branch` returning `Option` as about the
  spelling: the old signature had to invent an answer for every
  input, including branches Bugzilla has no product for.
- **Return `Option` when "no such thing" is a real outcome.**
  `DistGitClient::project_branches` gives `Ok(None)` for a package
  with no repository and `Err` when the request failed, which is what
  lets `ebranch check-wip` say "not yet in dist-git" for the first
  and "not checked" for the second. A signature that returned an
  empty `Vec` for both would have quietly claimed packages were
  missing whenever the network hiccupped.

The shared test for new code: if this lookup fails, does the caller
report something indistinguishable from a genuine negative? If so, the
absence needs its own representation — `Option`, a separate variant, or
a timestamp recording *when* the answer was last actually obtained.

## Config files layer: /etc, then ~/.config, then the command line

Every tool's config is read in layers: an optional system-wide
`/etc/<tool>/config.toml` first, overridden per key (recursively
for tables) by the per-user `~/.config/<tool>/config.toml`, with
command-line flags overriding both. `ConfigFile::load` and
`read_merged` in sandogasa-config implement the merge, so every
tool gets it without extra code; `save` only ever writes the user
file. The system layer suits org-wide deployments (a distro
package or ansible dropping shared settings) while each user
keeps their own credentials and overrides.

Every tool reads the system layer, whether or not it has settings
of its own: the nine with a `config` subcommand (cpu-sig-tracker,
ebranch, fedora-cve-triage, fedora-review-digest, fesco-chair,
hs-relmon, poi-tracker, sandogasa-pkg-acl, sandogasa-report) load
their own keys from it, and the rest reach it through
`parse_with_defaults`, which reads the same layered files for the
`[defaults]` table described below.

Four consequences worth knowing, for packaging in particular:

- **Nothing ever creates the system file.** No tool writes under
  `/etc`; `save` writes the user file only. A system config is
  always authored by an admin (or shipped by a package), so an RPM
  wants to own the directory and mark the file `%ghost
  %config(noreplace)` rather than expect it to appear.
- **A system file alone is enough.** `read_merged` returns the
  system table when no user file exists, so `load` succeeds from
  `/etc` with no per-user setup. (The not-found error names the
  user path, which reads oddly in that case — it only fires when
  *neither* file exists.)
- **Permissions are enforced on the user file only** — 700 on the
  directory, 600 on the file, fixed in place on read. The system
  file is read as-is with no mode check, so `root:root 0644` is
  both what a package should ship and what the split assumes: the
  system layer is for shared, non-secret settings (koji tags, group
  and instance definitions, `[defaults]`), and credentials stay in
  the per-user file where the 600 enforcement applies and each
  token maps to one person.
- **A missing system file is not an error**, so shipping the
  directory empty is fine.

Keeping credentials out of the system layer is a design choice, not
just caution, because the alternatives are all worse. Restricting by
group barely works on Fedora, where each user gets their own group,
so there is no natural group to grant and the admin would have to
invent and populate one. setgid on the binaries would be both
discouraged in Fedora and a bad fit here, since these tools exec
`koji`, `fedrq`, `git` and `mock` and parse network data — a large
escalation surface for no gain. And a shared token defeats the audit
trail on the far end, where every action arrives as one identity.

Where a machine genuinely needs its own credential — a builder, a
cron job — give that job its own user with its own 600 config, or
pass the token in the environment: every credential has an env
override (`FORGEJO_TOKEN`, `GITLAB_TOKEN`, `GITHUB_TOKEN`,
`JIRA_TOKEN`, `BUGZILLA_API_KEY`, `PAGURE_API_TOKEN`,
`SOURCEHUT_TOKEN`, plus per-host variants such as
`FORGEJO_TOKEN_FORGE_FEDORAPROJECT_ORG`), so under systemd a
`LoadCredential` drop-in works without any config file at all.

## Flag defaults come from the config file — a common pattern

Every tool supports a `[defaults]` table in its config (the same
layered `/etc` + `~/.config` files that hold credentials — see
above) to pin flag defaults, so users don't have to retype the
flags they always pass — e.g. always narrating dbranch runs:

```toml
[defaults]          # tool-wide: applies wherever the flag exists
explain = true

[defaults.update]   # for one subcommand only
quiet = true
```

Keys are the flag's **long name** as typed on the command line
(dashes included). A top-level key covers global and top-level
flags, and also applies to any invoked subcommand that has a flag
of that name — subcommands without it just ignore the default, so
one `explain = true` line covers every dbranch subcommand with
`--explain`. Use a `[defaults.<subcommand>]` table to scope a
default to a single subcommand. `true` turns a boolean flag on;
strings and numbers become `--key value`; arrays repeat a
repeatable flag.

The mechanics live in one place —
`sandogasa_cli::parse_with_defaults` — and every tool's `main`
uses it instead of `Cli::parse()`:

```rust
let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
```

Guarantees the helper enforces (don't reimplement them per tool):

- **Command line always wins.** A config default never overrides a
  flag given explicitly (including via a flag's env var).
- **Conflicts resolve in the user's favor.** A default that
  `conflicts_with` an explicitly-given flag is skipped, not
  errored — `dbranch update -q` silently suppresses a configured
  `explain = true`.
- **`--no-defaults`** (added to every tool automatically) skips
  the whole table for one run.
- **Typos fail loudly.** An unknown flag or subcommand name in
  `[defaults]` is a hard error naming the config file, never
  silently ignored.

When adding a new tool, use `parse_with_defaults` from the start;
when adding flags, nothing extra is needed — any long flag is
automatically defaultable.
