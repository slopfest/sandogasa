# sandogasa-pkg-acl

View and manage Fedora package ACLs via the [Pagure](https://src.fedoraproject.org/)
dist-git API.

## Installation

```
cargo binstall sandogasa-pkg-acl
```

Or build from source:

```
cargo install sandogasa-pkg-acl
```

## Setup

```
$ sandogasa-pkg-acl config
No config found at /home/user/.config/sandogasa-pkg-acl/config.toml
Enter your dist-git API token:
Verifying token... OK (authenticated as salimma)
Saved to /home/user/.config/sandogasa-pkg-acl/config.toml
```

The token can also be passed via the `PAGURE_API_TOKEN` environment variable.

## Usage

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

### Set an ACL

```
$ sandogasa-pkg-acl set freerdp --user salimma --level commit
Set user 'salimma' to 'commit' on freerdp
```

Valid levels: `ticket`, `collaborator`, `commit`, `admin`.

If the target already has equal or higher access, the operation is
skipped. Pass `--strict` to downgrade access to the requested level.

### Remove an ACL

```
$ sandogasa-pkg-acl remove freerdp --user olduser
Removed user 'olduser' from freerdp
```

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

### Give package ownership

```
$ sandogasa-pkg-acl give dcavalca freerdp librdp
Gave freerdp to 'dcavalca'
Gave librdp to 'dcavalca'
```

The target username is validated before any transfers. Requires the
caller to be the package owner.

### JSON output

All subcommands support `--json` for machine-readable output:

```
$ sandogasa-pkg-acl --json show freerdp
```

## Access requirements

- `show` — no authentication required
- `set`, `remove`, `apply` — require admin access (direct or via group)
- `give` — requires package owner

Package owners cannot be downgraded or removed via `set`, `remove`,
or `apply`.

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
