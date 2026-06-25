# sandogasa-forgejo

A small Rust client for the Forgejo / Gitea REST API (`/api/v1`),
scoped to what sandogasa tools need today: the merged pull requests
the token owner authored (for `sandogasa-report`'s activity reports),
issue create/search (for `ebranch`'s releng-ticket filing), and token
validation.

Works against any instance — codeberg.org, a Fedora Forgejo, or a
self-hosted Gitea — by taking the instance root URL in full. Designed
to mirror `sandogasa-github` and `sandogasa-gitlab` in shape so a
Forgejo domain in `sandogasa-report` looks structurally identical to a
GitHub or GitLab one (per-instance tokens and identities, an optional
owner/org filter).

The pull-request search filters by `created=true`, i.e. it reports the
activity of whoever owns the token — the self-reporting case. Point it
at your own instance token.

## Token scopes

Forgejo tokens are scoped by category (read/write). Per operation:

- `my_pull_requests` — `read:repository` + `read:issue` (the search
  lives under the `/repos` group and is an issue endpoint).
- `validate_token` — `read:user` (it calls `/api/v1/user`).
- `create_issue` / `search_issues` — `write:issue` (create) /
  `read:issue` (search), plus `read:repository`.

## Usage

```rust
use sandogasa_forgejo::Client;

let client = Client::new("https://codeberg.org", "token")?;
for pr in client.my_pull_requests("closed", None)? {
    if pr.is_merged() {
        println!("{}#{} {}", pr.repo_slug().unwrap_or(""), pr.number, pr.title);
    }
}
```

## Installation

```sh
cargo add sandogasa-forgejo
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
