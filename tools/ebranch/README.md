# ebranch

Build dependency resolver for cross-branch package porting. Given a set
of source packages, a source branch (e.g. `rawhide`), and a target branch
or repository (e.g. `epel10`, a Koji side tag), ebranch discovers which
BuildRequires are missing on the target, computes the full transitive
closure, detects dependency cycles, and produces a phased build order
for parallel execution.

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

### Useful flags

- `--verbose` / `-v` — print progress to stderr as packages are resolved
- `--max-depth N` — limit recursion depth (useful for exploring large
  dependency trees incrementally)
- `--koji` — output build-order as a Koji chain build string
- `--copr` — generate a Copr batch build shell script
- `--json` — machine-readable JSON output

## License

[MPL-2.0](https://mozilla.org/MPL/2.0/)
