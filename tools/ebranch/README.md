# ebranch

Build dependency resolver for cross-branch package porting. Given a set
of source packages, a source branch (e.g. `rawhide`), and a target branch
or repository (e.g. `epel10`, a Koji side tag), ebranch discovers which
BuildRequires are missing on the target, computes the full transitive
closure, detects dependency cycles, and produces a phased build order
for parallel execution. It can also verify that all subpackages will
be installable after building, expanding the closure as needed.

Shells out to [fedrq](https://src.fedoraproject.org/rpms/fedrq) for
repository queries.

## Installation

```
cargo install ebranch
```

Requires `fedrq` to be installed and available in `$PATH`.

## Usage

At least one of `--source` / `--source-repo` and one of `--target` /
`--target-repo` is required. When both a branch and a repo are given,
fedrq combines them (e.g. `--target c10s --target-repo @epel` queries
CentOS Stream 10 base repos plus EPEL).

### Analyze a crates.io crate's dependencies

Use `check-crate` to check which dependencies of a Rust crate are
available in a target RPM repo, which are too old, and which are
missing entirely:

```console
$ ebranch check-crate semcode -b rawhide
Checking crate: semcode 0.14.0
Branch: rawhide

Dependencies (35 normal, 0 build, 16 dev):

Missing (8):
  - semcode-core ^0.14.0 (normal)
  ...

Too old (8):
  - tree-sitter ^0.26 (normal)
    have: 0.25.10, need: ^0.26
  ...

Satisfied (35):
  - libc ^0.2 (normal) — 0.2.182
  ...

Summary: 8 missing, 8 too old, 35 satisfied.
```

Specify a version (defaults to latest):

```sh
ebranch check-crate tokio 1.51.0 -b epel9 -r @epel
```

Use `--transitive` / `-t` to expand missing dependencies transitively,
showing the full set of crates that need to be packaged:

```sh
ebranch check-crate arrow 57.3.0 -b rawhide -t -v
```

By default, normal, build, and dev dependencies are expanded
(matching Fedora's `%check`-enabled builds). Add `--exclude-dev`
to skip dev deps, or `--include-optional` to also expand optional
deps. The output includes a phased build order showing which
`rust-*` packages to build first.

### Useful flags

- `--transitive` / `-t` — expand missing `check-crate` deps transitively
  (includes phased build order)
- `--exclude-dev` — exclude dev deps from transitive expansion
- `--include-optional` — include optional deps in transitive expansion
- `--include-unmet` — include unmet-version deps in transitive expansion
- `--exclude CRATE,...` — skip specific crates in transitive expansion
- `--dot` — output dependency graph in Graphviz DOT format
- `--toml PATH` — save full analysis to a TOML file for reuse
- `--verbose` / `-v` — print progress to stderr as packages are resolved
- `--max-depth N` — limit recursion depth (useful for exploring large
  dependency trees incrementally)
- `--check-install` — verify subpackage installability and expand the
  closure with any additionally needed packages
- `--exclude-install PKG,...` — exclude source packages from
  installability checks (deps they provide are treated as satisfied)
- `--no-auto-exclude-install` — disable automatic exclusion of solib symbol
  version deps (e.g. `libc.so.6(GLIBC_2.38)(64bit)`) from
  installability checks
- `-j N` / `--jobs N` — number of parallel fedrq queries
  (0 = number of CPUs, the default)
- `--phases` — group `resolve` output into parallel build phases
- `--koji` — output as a Koji chain build string (requires `--phases`)
- `--copr` — generate a Copr batch build script (requires `--phases`)
- `--refresh` — clear fedrq repo metadata cache before querying
- `--json` — machine-readable JSON output

### Link Bugzilla review requests

Use `check-pkg-reviews` to find and link Bugzilla package review
requests based on the dependency graph from `check-crate --toml`:

```sh
# 1. Run analysis and save to TOML
ebranch check-crate arrow 57 -b rawhide -t --toml arrow.toml

# 2. Find review bugs and show proposed Depends On changes
ebranch check-pkg-reviews arrow.toml --dry-run -v

# 3. Apply the changes (requires Bugzilla API key)
export BUGZILLA_API_KEY=your-key
ebranch check-pkg-reviews arrow.toml -v
```

Found bug IDs are cached in the TOML file under `[review_bugs]`,
so subsequent runs skip the Bugzilla search for already-found bugs.

The tool only links missing packages (not unmet-version deps that
already exist in the repo). It preserves existing Depends On links
to bugs outside the dependency set.

### Check if an update would break reverse dependencies

Use `check-update` to verify that a Koji side tag or Bodhi update
won't break packages that depend on the updated packages. It compares
old vs new subpackage Provides, classifying each as updated (version
bump) or removed, then finds reverse dependencies that would break:

```console
$ ebranch check-update epel9-build-side-134436 \
    -b c9s -r @epel -v
[check-update] updated packages: rust-tokio, rust-tokio-macros
[check-update] using @testing for new provides
[check-update] 4 changed provides (4 updated, 0 removed)
...
Updated Provides (4):
  - crate(tokio-macros) (2.6.1 -> 2.7.0)
  - crate(tokio-macros/default) (2.6.1 -> 2.7.0)
  ...

No packages depend on the changed Provides. No breakage expected.
```

The input can also be a Bodhi update alias or URL:

```sh
ebranch check-update FEDORA-EPEL-2026-f9eaa11e18 -b c9s -r @epel
ebranch check-update https://bodhi.fedoraproject.org/updates/FEDORA-EPEL-2026-f9eaa11e18
```

For new provides, ebranch checks these sources in order:
1. **@testing** — if the update has been pushed to testing
   (authoritative metadata, preferred)
2. **Side tag** — via `koji buildinfo` + `fedrq pkg_provides`
   (warns if the side tag repo is stale)
3. **Reverse deps only** — lists affected packages for manual review

For EPEL side tags, the testing branch is auto-detected from the
side tag name (e.g. `epel9-build-side-*` uses `epel9`). Use
`--testing-branch` to override if needed.

### Detect dependency cycles

```sh
ebranch find-cycles systemd util-linux \
    --source rawhide --target c10s --target-repo '@epel'
```

### Resolve the full dependency closure

```sh
ebranch resolve systemd --source rawhide --target c10s --target-repo '@epel'
ebranch resolve systemd --source rawhide --target c10s --json
```

Use `--phases` to group the closure into parallel build phases:

```console
$ ebranch resolve --phases rust-base64-simd \
    --source rawhide \
    --target-repo '@koji:epel10.3-build-side-133542'
Build order from rawhide to @koji:epel10.3-build-side-133542:

  Phase 1:
    - rust-const-str
    - rust-outref
  Phase 2:
    - rust-vsimd
  Phase 3:
    - rust-base64-simd

4 package(s) in 3 phase(s).
```

Add `--koji` for Koji chain-build output or `--copr` for a Copr
batch build script:

```sh
ebranch resolve --phases --koji rust-base64-simd \
    --source rawhide --target-repo '@koji:epel10.3-build-side-133542'

ebranch resolve --phases --copr rust-base64-simd \
    --source rawhide --target-repo '@koji:epel10.3-build-side-133542' \
    > build.sh
```

Use `--check-install` to verify that every subpackage in the closure
will be installable after building:

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
