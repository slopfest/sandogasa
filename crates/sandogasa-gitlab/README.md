# sandogasa-gitlab

A Rust client for the GitLab REST and GraphQL APIs, focused on issue
management and work-item status tracking.

## Features

- Create, list, update, and close issues via REST API v4
- Add comments (notes) to issues
- Query and set work-item status via GraphQL
- Group-level issue listing with pagination
- Create a project in a group (`GroupClient::create_project`) and make
  it a pull mirror of another repository (`set_pull_mirror` with
  `PullMirror::alongside`, or `PullMirror::branches_matching` for a
  branch filter, `start_pull_mirror`, `pull_mirror` for its
  `update_status`), through the `/projects/:id/mirror/pull` API of
  GitLab 17.6 and later (Premium and Ultimate)
- URL parsing utilities for GitLab project and issue URLs

## Usage

```rust
use sandogasa_gitlab::Client;

let client = Client::new("https://gitlab.com", "group/project", "glpat-token")?;
let issues = client.list_issues("bug", Some("opened"))?;
```

Notes: `merge_request_notes` and `issue_notes` (oldest first, system
notes flagged), `add_merge_request_note` and `add_note` (issues).
Merge requests: `merge_request`, `merge_request_commits`.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
