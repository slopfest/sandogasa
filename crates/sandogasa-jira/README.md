# sandogasa-jira

Minimal JIRA REST API client: read an issue, search with JQL, list a
project's versions and the fields an issue type takes, and file an
issue.

Red Hat's issue tracker is the Atlassian Cloud site
`https://redhat.atlassian.net` (`issues.redhat.com` redirects to it).
Anonymous access works for public issues. For anything else, a Cloud
site takes an API token from
<https://id.atlassian.com/manage-profile/security/api-tokens> with the
account's email, as basic auth (`JiraClient::with_api_token`) — sent
through Atlassian's API gateway, the only place a scoped token is
honoured: `cloud_id(site)` reads the tenant id and
`gateway_url(&id)` is the base URL to build the client on. A scoped
token needs `read:jira-user` for `myself()`, `read:jira-work` for
issues and searches, and `write:jira-work` to file one. A self-hosted
Jira takes a personal access token as a bearer (`with_api_key`).
`myself()` names the account behind the credentials
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

Searching and a public project's versions work anonymously too:

```rust
# use sandogasa_jira::JiraClient;
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let client = JiraClient::new("https://redhat.atlassian.net");
let jql = r#"project = RHEL AND component = freetype AND text ~ "CVE-2025-27363""#;
for issue in client.search(jql, &["summary", "status", "versions"], 20).await? {
    let versions: Vec<_> = issue.fields.versions.iter().map(|v| &v.name).collect();
    println!("{} {} {:?}", issue.key, issue.status(), versions);
}
let versions = client.project_versions("RHEL").await?;
# Ok(())
# }
```

Filing needs credentials that may create issues in the project.
`create_metadata(project, issue_type)` lists the fields that issue
type takes, which are required, and what a choice field accepts;
`create_issue` files a `NewIssue` and answers its key, or Jira's own
objection (a required field left out, a version the project does not
have):

```rust
# use sandogasa_jira::{JiraClient, NewIssue};
# async fn demo(client: JiraClient) -> Result<(), String> {
let mut issue = NewIssue::new("RHEL", "Bug", "CVE-2025-27363: out of bounds write in FreeType");
issue.components = vec!["freetype".into()];
issue.versions = vec!["CentOS Stream 9".into()];
issue.priority = Some("Critical".into());
let created = client.create_issue(&issue).await?;
println!("filed {}", created.key);
# Ok(())
# }
```

Transitions and comments can be added as callers need them.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
