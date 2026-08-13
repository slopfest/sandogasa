# sandogasa-koji

Koji build system CLI wrapper for the sandogasa workspace.

Provides functions for querying Koji tags and builds by shelling out
to the `koji` CLI. Supports multiple Koji profiles (e.g. `cbs` for
CentOS Build System).

## Functions

- `list_tagged(tag, profile, timestamp)` — list builds with NVR, tag,
  and owner (optional timestamp for historical queries)
- `latest_tagged(tag, package, profile)` — latest build of one
  package in a tag, following tag inheritance (`--inherit`); the
  right primitive for "does this release actually carry version X"
  checks, since side tags and `-candidate`/`-testing` tags are
  never in a release tag's chain
- `latest_tagged_all(tag, profile)` — the same for every package in the
  tag at once, for callers with enough packages that one large answer
  beats many small ones (measured crossover: about seven)
- `is_available()` — whether the `koji` CLI is on PATH, for callers
  that degrade gracefully
- `list_tagged_nvrs(tag, profile)` — list NVRs only (quiet mode)
- `build_rpms(nvr, profile)` — list binary RPM names from buildinfo
- `parse_nvr(nvr)` — split NVR into (name, version, release)
- `parse_nvr_name(nvr)` — extract just the package name from an NVR
- `tag_missing(error)` — whether an error from this crate says the tag
  does not exist, as opposed to saying nothing definite; only that
  licenses a caller to act on absence
- `hub_unresponsive(profile)` — whether this profile's hub has already
  failed to answer in this process, so a caller with many queries left
  can stop asking

## Timeouts

Queries are bounded at 30 seconds; `SANDOGASA_KOJI_TIMEOUT` overrides
that in seconds, and `0` waits indefinitely. The koji CLI has no timeout
of its own, so an unreachable hub would otherwise block a caller with no
output.

Operations that are *meant* to block — `regen_repo` and `tag_build`, both
of which pass `--wait` — get a separate bound of 30 minutes,
`SANDOGASA_KOJI_WAIT_TIMEOUT`. A repo regeneration runs for minutes by
design; one measured against Fedora's hub took 4m43s. Before starting
one, `hub_responds` asks `koji version`, a sub-second round trip needing
no authentication, so an unreachable hub costs the query timeout instead
of the wait bound. A wait that outruns its own bound does not mark the
hub unresponsive: that says the work is slow, not that the hub has
stopped answering.

The first timeout latches per profile: later calls fail immediately
rather than each paying the full bound, since a hub that did not answer
one query will not answer the next. Callers that query per tag and per
package should check `hub_unresponsive` and report from what they have.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
