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

### Compute parallel build phases

```console
$ ebranch build-order rust-base64-simd \
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

### Resolve the full dependency closure

```sh
ebranch resolve systemd --source rawhide --target c10s --target-repo '@epel'
ebranch resolve systemd --source rawhide --target c10s --json
```

### Detect dependency cycles

```sh
ebranch find-cycles systemd util-linux \
    --source rawhide --target c10s --target-repo '@epel'
```

### Koji chain build output

Use `--koji` with `build-order` to get output suitable for
`koji chain-build`:

```console
$ ebranch build-order --koji rust-base64-simd \
    --source rawhide \
    --target-repo '@koji:epel10.3-build-side-133542'
rust-const-str rust-outref : rust-vsimd : rust-base64-simd
```

### Copr batch build script

Use `--copr` with `build-order` to generate a shell script that uses
`copr build-package` with `--after-build-id` and `--with-build-id` to
preserve phase ordering:

```sh
ebranch build-order --copr rust-base64-simd \
    --source rawhide \
    --target-repo '@koji:epel10.3-build-side-133542' > build.sh
chmod +x build.sh
./build.sh @myuser/myproject --chroot epel-10-x86_64
```

The script takes the Copr repo as its first argument, and passes any
remaining arguments through to every `copr build-package` call.

### Installability check

Use `--check-install` to verify that every subpackage in the closure
will be installable after building. This checks that all Requires of
each subpackage are satisfiable by either the target repo or another
package in the closure. If additional packages are needed, ebranch
automatically expands the closure and re-resolves until everything is
installable:

```console
$ ebranch build-order rust-uucore \
    --source rawhide \
    --target-repo '@koji:epel10.3-build-side-133542' \
    --check-install --verbose --koji
[installability] resolving with 1 package(s): rust-uucore
[level] processing 1 package(s) (0 resolved so far): rust-uucore
[installability] checking 1 package(s): rust-uucore
[installability] adding 7 package(s): rust-base64-simd, ...
[installability] resolving with 8 package(s): rust-base64-simd, ...
...
[installability] adding 1 package(s): rust-const-str-proc-macro
[installability] resolving with 9 package(s): rust-base64-simd, ...
...
rust-const-str rust-const-str-proc-macro rust-core_maths ...
```

Without `--check-install`, ebranch only checks BuildRequires.
Packages like `rust-const-str-proc-macro` that are only needed at
install time (as a Requires of a subpackage) would be missed.

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

By default, normal and build dependencies are expanded. Add
`--include-dev` or `--include-optional` to also expand those kinds.
The output includes a phased build order showing which `rust-*`
packages to build first.

### Useful flags

- `--transitive` / `-t` — expand missing `check-crate` deps transitively
  (includes phased build order)
- `--include-dev` — also expand dev deps in transitive expansion
- `--include-optional` — also expand optional deps in transitive expansion
- `--exclude CRATE,...` — skip crates in transitive expansion
- `--verbose` / `-v` — print progress to stderr as packages are resolved
- `--max-depth N` — limit recursion depth (useful for exploring large
  dependency trees incrementally)
- `--check-install` — verify subpackage installability and expand the
  closure with any additionally needed packages
- `--exclude-install PKG,...` — exclude source packages from
  installability checks (deps they provide are treated as satisfied)
- `--no-auto-exclude` — disable automatic exclusion of solib symbol
  version deps (e.g. `libc.so.6(GLIBC_2.38)(64bit)`) from
  installability checks
- `-j N` / `--jobs N` — number of parallel fedrq queries
  (0 = number of CPUs, the default)
- `--refresh` — clear fedrq repo metadata cache before querying
- `--koji` — output build-order as a Koji chain build string
- `--copr` — generate a Copr batch build shell script
- `--json` — machine-readable JSON output

## License

[MPL-2.0](https://mozilla.org/MPL/2.0/)
