# sandogasa-jira

Minimal JIRA REST API client, currently scoped to public issue
status lookup.

Red Hat's issue tracker is the Atlassian Cloud site
`https://redhat.atlassian.net` (`issues.redhat.com` redirects to it).
Anonymous access works for public issues. For anything else, a Cloud
site takes an API token from
<https://id.atlassian.com/manage-profile/security/api-tokens> with the
account's email, as basic auth (`JiraClient::with_api_token`) — sent
through Atlassian's API gateway, the only place a scoped token is
honoured: `cloud_id(site)` reads the tenant id and
`gateway_url(&id)` is the base URL to build the client on. A scoped
token needs `read:jira-user` for `myself()` and `read:jira-work` for
issues. A self-hosted Jira takes a personal access token as a bearer
(`with_api_key`). `myself()` names the account behind the credentials
and, by its 401, tells a rejected token from an unreachable server.
Point the client at the site's own host, never a redirecting alias: an
`Authorization` header is dropped on a redirect to another host.

## Usage

```rust
use sandogasa_jira::JiraClient;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let client = JiraClient::new("https://redhat.atlassian.net");
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
