# sandogasa-fedrq

A Rust wrapper around the [fedrq](https://github.com/gotmax23/fedrq) CLI tool
for querying Fedora and EPEL RPM repositories.

## Features

- Query subpackage provides, requires, and names for a source RPM
- Find reverse dependencies (whatrequires) for a set of packages
- Query BuildRequires for source packages

## Usage

```rust
use sandogasa_fedrq::Fedrq;

let fq = Fedrq { branch: Some("rawhide".to_string()), ..Default::default() };
let provides = fq.subpkgs_provides("systemd")?;
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
