# sandogasa-fedrq

A Rust wrapper around the [fedrq](https://github.com/gotmax23/fedrq) CLI tool
for querying Fedora and EPEL RPM repositories.

## Features

- Query subpackage provides, requires, and names for a source RPM
- Find reverse dependencies (whatrequires) for a set of packages
- Find source packages that BuildRequire a set of capabilities
- Query BuildRequires for source packages

## Usage

```rust
use sandogasa_fedrq::Fedrq;

let fq = Fedrq::for_branch("rawhide");
let provides = fq.subpkgs_provides("systemd")?;

// CentOS Stream 10 with a SIG's published repository read as well (-e)
let sig = Fedrq::for_branch("c10s").with_enabled(vec![
    "@baseurl:https://mirror.stream.centos.org/SIGs/10-stream/hyperscale/$basearch/packages-main/"
        .to_string(),
]);
```

`repo` (`-r`) replaces the branch's repositories with a repository
class; `with_enabled` (`-e`) adds repositories to the branch's own.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
