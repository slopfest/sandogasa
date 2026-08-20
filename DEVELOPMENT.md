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

## The release gates fill target/, not day-to-day builds

`target/` reached 80GB in `debug` alone here and ran a machine out of
disk more than once. The instinct is to blame ordinary development, but
measurement said otherwise: at 0.19.3 the gates held 19GB
(`target/semver-checks`) and 6.5GB (`target/llvm-cov-target`) against
6.3GB for a complete cold `cargo build --workspace --tests`. Two rules
follow.

**Dev builds carry line tables, not full DWARF.** `[profile.dev] debug =
"line-tables-only"` took that 6.3GB to 3.3GB. Backtraces keep file and
line — check the binary, not the flag name: `.debug_line` is present and
`readelf --debug-dump=decodedline` still resolves our own sources. What
is given up is variable inspection under a debugger, which is not how
anything here gets diagnosed. Do not extend this to `[profile.release]`:
that is what distro packaging builds, and its debuginfo is packaged.

**Incremental compilation stays on.** It is 1.3GB of the 3.3GB, and it is
what makes the edit loop fast. Disk is cheaper to reclaim than time is;
if a build tree has to shrink further, delete it rather than crippling
it.

**Reclaiming is two tools, chosen by what it costs, not by what it
frees.** `make sweep` removes only caches whose baseline has already
moved on — the gate trees, `target/package`, stray `*.profraw` — so it
frees less than `cargo clean` but bills nothing: the next build is still
incremental. Run it after a release (the checklist ends there, since a
just-published version makes the cached semver baseline useless), when
the disk is tight mid-work, or after a one-off `make cov`. Do not run it
while iterating on coverage or a suspected semver break with the same
gate over and over, because then those trees are live caches and each
iteration becomes a full instrumented rebuild. `cargo clean` is for
stepping away from the repo, or for running out of space for real.

## Take a setting and its override together, or neither

If a crate adopts a tuneable that another crate already has — a timeout, a
retry budget, a page size — it must adopt the *switch* along with the
value. A hardcoded constant borrowed from a sibling leaves the caller with
a wall and no door, and the wall is invisible until it is in the way.

`sandogasa-kojihub` did exactly this. `sandogasa-koji` has
`SANDOGASA_KOJI_TIMEOUT` with a documented convention — seconds, and `0`
means wait forever — while the hub client carried a bare
`.timeout(Duration::from_secs(180))`. Both crates talk to the same Koji,
so anyone who had already raised the limit for one path reasonably
expected it to apply to the other, and it did not. The cost was not
theoretical: a deep `koji-lag` sweep abandons a page at the bound and the
retry pays the hub cost again, so the missing override could stop a
backfill progressing rather than merely slow it, with no way to say
otherwise short of editing the source.

Practically:

- **Reuse the sibling's variable name** unless the two really are
  different questions. One knob for "how long may a Koji request take" is
  easier to reason about than two that interact.
- **Reuse its conventions too** — the same units and the same meaning for
  edge values. `0` meaning unbounded in one crate and one second in
  another is worse than having no override at all.
- **Offer the flag as well as the environment** where a caller may know
  better per-run than per-shell, and let the flag win.
- **Say where the default came from** in the doc comment. A number with a
  measurement behind it can be revised by taking a new measurement; a
  number with nothing behind it never gets revised at all.

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
- **Flags that authorize a write cannot be defaulted.** `--yes`,
  `--claim`, `--apply`, `--prune`, `--submit` and `--give-karma`
  are refused with a hard error (`NEVER_DEFAULTED` in
  `defaults.rs`). A config file is exactly where such a setting
  gets forgotten, and then every run writes to Bugzilla, Bodhi or
  dist-git without asking. Add to that list when a tool gains a
  flag whose whole purpose is to skip a confirmation.

### Booleans need a `--no-<flag>` partner to be overridable

`false` in `[defaults]` is a no-op — a switch cannot be turned
*off* from the config — so a tool with `offline = true` pinned has
no way to refresh for one run except `--no-defaults`, which drops
every other default too. Where a boolean is plausible to pin, give
it a negative partner:

```rust
/// Report from the ledger without contacting anything.
#[arg(long)]
offline: bool,

/// Refresh from the services, overriding a config default.
#[arg(long, conflicts_with = "offline")]
no_offline: bool,
```

**`conflicts_with`, not `overrides_with`.** The conflict is what
the "conflicts resolve in the user's favor" rule above keys off:
giving `--no-offline` makes the mechanism skip injecting the
configured `offline = true` entirely. `overrides_with` looks like
the natural choice and quietly does the wrong thing — it is mutual
and order-sensitive, and injected defaults are appended *after* the
command line, so the injected `--offline` arrives last, wins the
override, and unsets `no_offline`. The flag then reads as if it was
never passed.

Write-authorizing booleans need no partner: they cannot be
defaulted at all, per the rule above.

When adding a new tool, use `parse_with_defaults` from the start;
when adding flags, nothing extra is needed — any long flag is
automatically defaultable.
