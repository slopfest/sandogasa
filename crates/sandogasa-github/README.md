# sandogasa-github

A small Rust client for the GitHub REST API, scoped to what
sandogasa tools need today: user identity lookup, token
validation, user-event listing for activity reports, the Search
API for pull requests, and per-repo authored-commit counts.

Designed to mirror `sandogasa-gitlab` in shape so a GitHub
domain in `sandogasa-report` looks structurally identical to a
GitLab one (token resolution, group/org filter, per-instance
identities).

## Usage

```rust
use sandogasa_github::Client;

let client = Client::new("https://api.github.com", "ghp_token")?;
let user = client.user_by_username("octocat")?;
let prs = client.search_pull_requests(
    "type:pr author:octocat created:2026-01-01..2026-03-31",
)?;
```

## Installation

```sh
cargo add sandogasa-github
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
