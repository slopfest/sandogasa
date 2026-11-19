# sandogasa-github

A small Rust client for the GitHub REST API, scoped to what
sandogasa tools need today: user identity lookup, token
validation, user-event listing for activity reports, the Search
API for pull requests, and per-repo authored-commit counts.

Through GraphQL it also reads, for activity reports over any
period:
- `contributions(login, from, to)`: the repositories committed to,
  with a count each, and the pull requests reviewed, a year per
  request at most;
- `issue_comments(login, since)`: the user's comments on issues and
  pull requests, newest edit first, back to a day;
- `recent_annotated_tags(repos)`: each repository's annotated tags
  among its 50 newest, ten repositories to a request, tagger dates in
  UTC;
- `commit_counts(login, repos, since, until)`: the user's commits on
  each repository's default branch, 25 repositories to a request.

A window that names 100 repositories, the most GitHub gives, is split
until none does. Items GitHub withholds from the token (an
organization refusing classic personal access tokens) are left out
and counted in `unreadable`, with GitHub's reasons; a batch that fails
is listed in `failures` while the others count.
`private_repos_pushed_since` lists the private repositories the
token's owner (`token_login`) can read that were pushed to since a
day, since contributions count those without naming them.

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
