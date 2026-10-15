# sandogasa-forgejo

A small Rust client for the Forgejo / Gitea REST API (`/api/v1`),
scoped to what sandogasa tools need today: the merged pull requests
the token owner authored (for `sandogasa-report`'s activity reports),
issue create/search (for `ebranch`'s releng-ticket filing), per-repo
issue listing by state and label names plus single-issue fetch (for
`fesco-chair`'s agenda queries), and token validation.

Works against any instance — codeberg.org, a Fedora Forgejo, or a
self-hosted Gitea — by taking the instance root URL in full. The
response models (`Issue`, `PullRequest`, `TimelineEvent`, …) are
`#[non_exhaustive]`: read their fields by name, and build a test
fixture with `serde_json::from_value` on the JSON the API would return,
since a struct literal will not compile outside this crate. Fields the
API grows are then added without a breaking release. Designed
to mirror `sandogasa-github` and `sandogasa-gitlab` in shape so a
Forgejo domain in `sandogasa-report` looks structurally identical to a
GitHub or GitLab one (per-instance tokens and identities, an optional
owner/org filter).

The pull-request search filters by `created=true`, i.e. it reports the
activity of whoever owns the token — the self-reporting case. Point it
at your own instance token. `pull_requests_by` / `issues_by` search by a
named user instead (`created_by`), which needs no token on an instance
whose API answers anonymously, such as src.opensuse.org; pair them with
`Client::anonymous`.

## Token scopes

Forgejo tokens are scoped by category (read/write). Per operation:

- `my_pull_requests` / `my_issues` — `read:repository` + `read:issue`
  (the search lives under the `/repos` group and is an issue endpoint).
  `pull_requests_by` / `issues_by` need the same, or no token at all
  on an instance with anonymous API access.
- `validate_token` — `read:user` (it calls `/api/v1/user`).
- `create_issue` / `search_issues` — `write:issue` (create) /
  `read:issue` (search), plus `read:repository`.
- `label_ids` — `read:issue` + `read:repository`, and
  `read:organization` when a name is an organisation label.
- `repo_issues` / `issue` / `issue_comments` / `issue_timeline` —
  `read:issue` + `read:repository`.

## Labels

`create_issue` takes label **ids**, because that is what the API
accepts — `CreateIssueOption.labels` is a list of integers and never
names. `label_ids` turns names into them, searching the repository's
own labels and then the owning organisation's, a repository label of
the same name winning. Both scopes are searched because an issue may
carry either: Fedora's `releng/tickets` has `ops` on the repository and
`points/01` and `Priority/Low` on the `releng` organisation, and a
ticket wants all three.

A name found in neither scope is an error naming it, rather than a
label silently dropped — the issue API accepts a short list without
complaint, so a quiet miss would file a ticket the tracker's triage
never sees.

Note that a repository's issue template may declare `labels:`, but
those are applied by the web UI, not by the API: an issue filed through
this client carries only the labels it passes.

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
