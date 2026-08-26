# sandogasa-pkg-acl

View and manage Fedora package ACLs via the [Pagure](https://src.fedoraproject.org/)
dist-git API.

## Installation

```
cargo install sandogasa-pkg-acl
```

## Usage

### Batch apply from config

Create a TOML config file:

```toml
[users]
ngompa = "admin"
salimma = "commit"
olduser = "remove"

[groups]
kde-sig = "commit"
old-group = "remove"
```

Then apply it to one or more packages:

```
$ sandogasa-pkg-acl apply acls.toml freerdp librdp
Set user 'ngompa' to 'admin' on freerdp
Set user 'salimma' to 'commit' on freerdp
Removed user 'olduser' from freerdp
Set group 'kde-sig' to 'commit' on freerdp
Removed group 'old-group' from freerdp
Set user 'ngompa' to 'admin' on librdp
...
```

The value `"remove"` removes all ACLs for that user or group. Any other
value sets the corresponding ACL level.

### Configure dist-git API token

```
$ sandogasa-pkg-acl config
No config found at /home/user/.config/sandogasa-pkg-acl/config.toml
Enter your dist-git API token:
Verifying token... OK (authenticated as salimma)
Saved to /home/user/.config/sandogasa-pkg-acl/config.toml
```

The token can also be passed via the `PAGURE_API_TOKEN` environment variable.

### Give package ownership

```
$ sandogasa-pkg-acl give dcavalca freerdp librdp
Gave freerdp to 'dcavalca'
Gave librdp to 'dcavalca'
```

The target username is validated before any transfers. Requires the
caller to be the package owner.

To orphan a package, give it to the `orphan` sentinel user:

```
$ sandogasa-pkg-acl give orphan ccze
Gave ccze to 'orphan'
```

### Remove an ACL

```
$ sandogasa-pkg-acl remove freerdp --user olduser
Removed user 'olduser' from freerdp
```

### Set an ACL

```
$ sandogasa-pkg-acl set freerdp --user salimma --level commit
Set user 'salimma' to 'commit' on freerdp
```

Valid levels: `ticket`, `collaborator`, `commit`, `admin`.

If the target already has equal or higher access, the operation is
skipped. Pass `--strict` to downgrade access to the requested level.

### Show current ACLs

```
$ sandogasa-pkg-acl show freerdp
Package: freerdp

Users:
  ngompa: owner
  salimma: admin
  dcavalca: commit

Groups:
  kde-sig: commit

Your access (salimma): admin
```

### Take an orphaned package

```
$ sandogasa-pkg-acl take ccze colorized-logs
ccze: orphaned: Important bug not fixed — https://bugzilla.redhat.com/2454279
Took ccze (point of contact: 'salimma')
colorized-logs: orphaned: Lack of time
Took colorized-logs (point of contact: 'salimma')
```

The orphaning reason is shown before each package is taken, since the
server drops it on adoption. A package that is not orphaned is refused
by name:

```
$ sandogasa-pkg-acl take bash
error: bash is not orphaned, so there is nothing to take
  (sandogasa-pkg-acl show bash names its owner)
```

Each package is attempted independently; a failure is reported and the
rest still run.

### JSON output

All subcommands support `--json` for machine-readable output:

```
$ sandogasa-pkg-acl --json show freerdp
```

## Access requirements

- `show` — no authentication required
- `set`, `remove`, `apply` — require admin access (direct or via group)
- `give` — requires package owner
- `take` — requires membership in the `packager` group, and the
  package must be owned by `orphan` and not retired (a retired
  package needs a releng ticket instead)

Package owners cannot be downgraded or removed via `set`, `remove`,
or `apply`.

## System-wide configuration

Settings are read from `/etc/sandogasa-pkg-acl/config.toml` first, then
overridden per key by `~/.config/sandogasa-pkg-acl/config.toml`, with
command-line flags overriding both. A system file alone is enough — no
per-user file is required — and either may also carry a `[defaults]`
table pinning flag defaults (see the root `DEVELOPMENT.md`).

`sandogasa-pkg-acl config` writes the user file only, with 700 on the
directory and 600 on the file. Nothing writes under `/etc`: a system
file is admin-authored, holds shared non-secret settings, and is
normally shipped `root:root 0644`.

Credentials belong in the per-user file, which is 600, or in an
environment variable — a token under `/etc` is readable by every local
user. For an unattended machine, give the job its own user and its own
600 config.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
