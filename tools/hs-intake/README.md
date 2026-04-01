# hs-intake

Hyperscale package intake tool. Compares RPM package metadata between
branches to help determine whether a package can be safely rebuilt or
backported.

Shells out to [fedrq](https://src.fedoraproject.org/rpms/fedrq) for
repository queries.

## Installation

```
cargo install hs-intake
```

Requires `fedrq` to be installed and available in `$PATH`.

## Usage

### Comparing packages between branches

```sh
# Compare Provides
hs-intake compare-provides systemd c9s f44

# Compare Requires
hs-intake compare-requires systemd c9s f44

# Compare BuildRequires
hs-intake compare-build-requires systemd c9s f44

# Include unchanged entries
hs-intake compare-requires systemd c9s f44 --show-unchanged

# JSON output
hs-intake compare-provides systemd c9s f44 --json
```

### Checking if a backport is safe

```sh
# Check if it is safe to backport libbpf from f44 to c9s
hs-intake safe-to-backport libbpf c9s f44

# Also check reverse dependencies on other branches
hs-intake safe-to-backport libbpf c9s f44 --also-check epel9,epel9-next

# JSON output
hs-intake safe-to-backport libbpf c9s f44 --json
```

The `safe-to-backport` command exits with a non-zero status when
concerns are found. Concerns include:

- BuildRequires or Requires that are added or upgraded (may not be
  available on the target branch).
- Provides that are removed or downgraded (may break other packages on
  the target branch).
- Reverse dependencies on the target branch that depend on affected
  Provides.

## License

[MPL-2.0](https://mozilla.org/MPL/2.0/)
