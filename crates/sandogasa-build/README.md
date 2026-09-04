# sandogasa-build

Build-script helper for the sandogasa tools: emits the `git describe
--tags --dirty --always` of the checkout a tool is built from, so its
`--version` reads `v0.22.0-74-ge11cff1-dirty` — the last tag, the
commits on top of it, the commit, and whether the tree had uncommitted
changes. Outside a checkout (a crates.io install, a distro build from
the tarball) it emits nothing and the tool shows its crate version.

```rust,no_run
// build.rs
fn main() {
    sandogasa_build::emit_git_describe();
}
```

The tool's clap command then uses `sandogasa_cli::version!()` for
`long_version` and `sandogasa_cli::banner!()` for its `--help` header.

## License

Apache-2.0 OR MIT
