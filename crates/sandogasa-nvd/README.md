# sandogasa-nvd

A Rust client for the [NVD (National Vulnerability Database) API v2.0](https://nvd.nist.gov/developers/vulnerabilities),
with helpers for analyzing CVE data.

## Features

- Fetch CVE details by ID
- Extract fixed versions from CPE match data
- Detect JavaScript/NodeJS-specific CVEs using three strategies:
  CPE target software, CNA source, and description keywords
- Extract affected tool/binary names from CVE descriptions

## Usage

```rust
use sandogasa_nvd::NvdClient;

let client = NvdClient::new();
let cve = client.cve("CVE-2026-12345").await?;

let fixed = cve.fixed_versions();
let is_js = cve.targets_js();
let tools = cve.affected_tool_names("bug summary text");
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
