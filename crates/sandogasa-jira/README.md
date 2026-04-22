# sandogasa-jira

Minimal JIRA REST API client, currently scoped to public issue
status lookup.

Red Hat's public JIRA instance is at `https://issues.redhat.com`.
Anonymous access works for public issues; set a Personal Access
Token via `JiraClient::with_api_key` for private ones.

## Usage

```rust
use sandogasa_jira::JiraClient;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let client = JiraClient::new("https://issues.redhat.com");
let issue = client.issue("RHEL-12345").await?;

if let Some(issue) = issue {
    println!("{}: {} ({})", issue.key, issue.summary(), issue.status());
    if issue.is_resolved() {
        println!("Resolved: {:?}", issue.resolution());
    }
}
# Ok(())
# }
```

Scope is deliberately minimal — additional endpoints (search,
transitions, comments) can be added as callers need them.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
