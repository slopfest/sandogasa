# sandogasa-copr

A small Rust client for [COPR](https://copr.fedorainfracloud.org/)'s
public `api_3`, scoped to what sandogasa tools need today: the
read-only `monitor` endpoint (every package's latest build per chroot,
with state and version), which lets `ebranch check-update` treat a
staging COPR project as an update source without authentication.

Also ships the pure helpers around it: mapping a Fedora-ecosystem
branch to a COPR chroot prefix (`epel9` → `epel-9-`, `rawhide` →
`fedora-rawhide-`, `c10s` → `centos-stream-10-`), and extracting the
NVR list of succeeded builds for one chroot (x86_64 preferred). The
provides analysis itself runs through fedrq's `@copr:` repo class,
not this client.

## Usage

```rust,no_run
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let copr = sandogasa_copr::Copr::new();
let packages = copr.monitor("@rust", "uutils-and-nushell")?;
let prefix = sandogasa_copr::chroot_prefix("epel9").unwrap();
for nvr in sandogasa_copr::nvrs_for_chroot(&packages, &prefix) {
    println!("{nvr}");
}
# Ok(())
# }
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
