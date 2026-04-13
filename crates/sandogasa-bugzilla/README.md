# sandogasa-bugzilla

A Rust client for the [Bugzilla REST API](https://bugzilla.readthedocs.io/en/latest/api/),
targeting Red Hat Bugzilla.

## Features

- Search bugs with arbitrary query parameters, with automatic pagination
- Fetch individual bugs by numeric ID or alias
- Fetch comments on a bug
- Update single or multiple bugs in one request (requires API key)
- Bearer token authentication

## Usage

```rust
use sandogasa_bugzilla::BzClient;

let client = BzClient::new("https://bugzilla.redhat.com")
    .with_api_key("your-api-key".to_string());

let bugs = client.search("product=Fedora&status=NEW", 0).await?;
client.update(12345, &serde_json::json!({"status": "CLOSED"})).await?;
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
