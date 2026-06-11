# sandogasa-cli Development Notes

## Plaintext-credential guard (`ensure_secure_url`)

API clients that carry a secret (Bugzilla API key, GitLab/GitHub
tokens) call [`ensure_secure_url`] at construction, before any
request is made, so a token is never put on the wire over plain
`http`. The guard **fails closed**: if the base URL is plaintext
`http` to a non-loopback host, the constructor returns an error
and no request happens.

Allowed without error:

- any `https` URL;
- loopback hosts over `http` — `localhost`, `*.localhost`,
  `127.0.0.0/8`, `::1` (so wiremock/mockito tests and local
  development keep working).

### Override for testing / trusted networks

Set the environment variable **`SANDOGASA_ALLOW_INSECURE_URL`** to
any non-empty value to disable the guard, e.g. to point a tool at
a plaintext `http://` mock server on a non-loopback address, or a
trusted internal proxy:

```sh
SANDOGASA_ALLOW_INSECURE_URL=1 poi-tracker sync-distgit ...
```

Never set it for real credentials against a real service — it
re-enables sending the token in cleartext.

### Where it's wired in

Call it from any new constructor that pairs a base URL with a
token:

- `sandogasa-bugzilla`: `BzClient::with_api_key`
- `sandogasa-gitlab`: `Client::new`, `GroupClient::new`,
  `validate_token`
- `sandogasa-github`: `Client::new`, `validate_token`

Jira and Discourse clients also carry tokens but are not yet
guarded; add the same call to their token-attaching constructors
when convenient.
