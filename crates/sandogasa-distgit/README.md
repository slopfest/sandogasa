# sandogasa-distgit

A Rust client for [Fedora dist-git](https://src.fedoraproject.org/) with
RPM spec file parsing utilities.

## Features

- Fetch spec files for a package on any dist-git branch
- Extract the package name from a spec file's `Name:` field
- List shipped binaries from `%{_bindir}` and `%{_libexecdir}` entries
  in `%files` sections, with `%{name}` macro expansion

## Usage

```rust
use sandogasa_distgit::{DistGitClient, spec};

let client = DistGitClient::new();
let spec_text = client.fetch_spec("libxml2", "rawhide").await?;
let binaries = spec::shipped_binaries(&spec_text);
// e.g. ["xmllint", "xmlcatalog"]
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
