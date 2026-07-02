# sandogasa-sourcehut

GraphQL client for the [Sourcehut](https://sr.ht) (sr.ht) API, covering
the activity `sandogasa-report` summarizes for a contributor.

Sourcehut has no unified pull-request model; activity is split across
independent services, each with its own GraphQL endpoint at
`https://<service>.<host>/query`. This crate wraps the three that matter
for activity reporting:

- **lists.sr.ht** — `Client::patches`: patchsets a user submitted (the
  mailing-list, patch-based analog of pull requests; `status` includes
  `APPLIED`, an accepted-vs-proposed split).
- **todo.sr.ht** — `Client::ticket_events`: the authenticated user's
  ticket-activity feed (opened / status-changed), the issues analog.
- **git.sr.ht** — `Client::repositories` + `Client::commits_since` (plus
  `Client::user_email` for the account's primary email): commits in the
  user's own repositories. sr.ht exposes only the primary email, so the
  caller attributes owner vs third-party against the primary plus any
  extra emails it's told about.

Authentication uses a personal access token from
`meta.sr.ht/oauth2/personal-token` (sent as `Authorization: Bearer`),
which by default grants read access to every service.

## Usage

```rust,no_run
use sandogasa_sourcehut::Client;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::new("sr.ht", "your-personal-access-token")?;
for p in client.patches("michel")? {
    println!("{} [{}] to {}", p.subject, p.status, p.list.name);
}
# Ok(())
# }
```

The host is passed in full (`sr.ht`, or a self-hosted host), so the client
works against any deployment. See `DEVELOPMENT.md` for API conventions and
the metric-mapping caveats.
