# sandogasa-bugclass

Classify issue-tracker bugs into a portable set of categories.

The [`BugKind`] enum is the shared vocabulary across trackers — CVEs,
FTBFS / FTI, update requests, etc. Per-tracker submodules implement
the classification logic for their tracker's specific conventions
(keywords, aliases, blocks relationships, labels, etc.).

Currently only Bugzilla is supported. GitLab / GitHub / other
trackers can be added alongside as new submodules.

The `semver` module classifies pending version bumps (from
release-monitoring "X is available" bugs) by semver impact using
Cargo's compatibility rule — [`semver::Bump`], [`semver::classify`],
and the [`semver::version_at_least`] comparison helper. Shared by
poi-tracker's `semver-audit` and sandogasa-pkg-health's
`pending_update` check so both classify identically.

## Usage

```rust
use sandogasa_bugclass::{BugKind, bugzilla};

# async fn demo(bz: &sandogasa_bugzilla::BzClient, bug: &sandogasa_bugzilla::models::Bug) {
let trackers = bugzilla::lookup_trackers(bz, &[45, 46], false).await;
let kind = bugzilla::classify(bug, &trackers);
assert_eq!(kind.as_str(), "security"); // or whatever
# }
```

A search whose bugs go to `classify` asks for
`bugzilla::CLASSIFY_FIELDS` (`bz.search_with(query, 0,
bugzilla::CLASSIFY_FIELDS)`): the fields it reads beyond those every
bug comes with.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
