# sandogasa-depcheck

Check what an RPM update does to the packages that depend on it.

Given the packages an update builds and the repositories its users
have, it lists:

- the Provides the update changes, each dropped or at another version;
- the Requires of the update's own packages that no longer resolve;
- the reverse dependencies whose runtime Requires (FTI, fails to
  install) or BuildRequires (FTBFS, fails to build) the update leaves
  unsatisfied, counting a capability another package still provides as
  satisfied.

Rich (boolean) dependencies are evaluated with their RPM semantics
(`if`/`else`, `unless`, `or`, `and`/`with`, `without`), and version
constraints compared as RPM does.

The repositories come through the `RepoQuery` trait, which fedrq's view
implements; a tool can supply its own, for builds no repository holds
yet. ebranch's `check-update` uses it for Fedora and EPEL updates.

## Installation

```sh
cargo add sandogasa-depcheck
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
