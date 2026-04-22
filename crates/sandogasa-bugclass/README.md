# sandogasa-bugclass

Classify issue-tracker bugs into a portable set of categories.

The [`BugKind`] enum is the shared vocabulary across trackers — CVEs,
FTBFS / FTI, update requests, etc. Per-tracker submodules implement
the classification logic for their tracker's specific conventions
(keywords, aliases, blocks relationships, labels, etc.).

Currently only Bugzilla is supported. GitLab / GitHub / other
trackers can be added alongside as new submodules.

## Usage

```rust
use sandogasa_bugclass::{BugKind, bugzilla};

# async fn demo(bz: &sandogasa_bugzilla::BzClient, bug: &sandogasa_bugzilla::models::Bug) {
let trackers = bugzilla::lookup_trackers(bz, &[45, 46], false).await;
let kind = bugzilla::classify(bug, &trackers);
assert_eq!(kind.as_str(), "security"); // or whatever
# }
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
