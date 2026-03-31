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
[installability] resolving with 1 package(s)
[depth 1] resolving rust-uucore (0 queued, 0 resolved)
[installability] checking subpackages of rust-uucore
[installability] adding 7 package(s): rust-base64-simd, ...
[installability] resolving with 8 package(s)
...
[installability] adding 1 package(s): rust-const-str-proc-macro
[installability] resolving with 9 package(s)
...
rust-const-str rust-const-str-proc-macro rust-core_maths ...
```

Without `--check-install`, ebranch only checks BuildRequires.
Packages like `rust-const-str-proc-macro` that are only needed at
install time (as a Requires of a subpackage) would be missed.

### Useful flags

- `--verbose` / `-v` — print progress to stderr as packages are resolved
- `--max-depth N` — limit recursion depth (useful for exploring large
  dependency trees incrementally)
- `--check-install` — verify subpackage installability and expand the
  closure with any additionally needed packages
- `--koji` — output build-order as a Koji chain build string
- `--copr` — generate a Copr batch build shell script
- `--json` — machine-readable JSON output

## License

[MPL-2.0](https://mozilla.org/MPL/2.0/)
