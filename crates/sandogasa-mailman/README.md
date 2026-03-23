# sandogasa-mailman

A Rust client for [HyperKitty](https://gitlab.com/mailman/hyperkitty), the
Mailman 3 web archiver API.

## Features

- Find a sender's mailman ID by scanning recent list emails
- Fetch a sender's recent emails across all lists
- No authentication required

## Usage

```rust
use sandogasa_mailman::MailmanClient;

let client = MailmanClient::new();

// Find the sender's mailman ID by scanning a list
let id = client
    .find_sender_id("devel@lists.fedoraproject.org", "user@example.com", 3)
    .await?;

// Fetch their recent emails across all lists
if let Some(id) = id {
    let emails = client.sender_emails(&id, 5).await?;
    for email in &emails {
        println!("{}: {}", email.date.as_deref().unwrap_or("?"), email.subject);
    }
}
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
