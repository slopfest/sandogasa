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
