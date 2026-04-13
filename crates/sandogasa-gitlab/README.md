# sandogasa-gitlab

A Rust client for the GitLab REST and GraphQL APIs, focused on issue
management and work-item status tracking.

## Features

- Create, list, update, and close issues via REST API v4
- Add comments (notes) to issues
- Query and set work-item status via GraphQL
- Group-level issue listing with pagination
- URL parsing utilities for GitLab project and issue URLs

## Usage

```rust
use sandogasa_gitlab::Client;

let client = Client::new("https://gitlab.com", "group/project", "glpat-token")?;
let issues = client.list_issues("bug", Some("opened"))?;
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
