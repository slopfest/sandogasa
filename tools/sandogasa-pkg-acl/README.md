# sandogasa-pkg-acl

View and manage Fedora package ACLs via the [Pagure](https://src.fedoraproject.org/)
dist-git API.

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
```

### Set an ACL

```
$ sandogasa-pkg-acl set freerdp --user salimma --level commit --token <token>
Set user 'salimma' to 'commit' on freerdp
```

Valid levels: `ticket`, `collaborator`, `commit`, `admin`.

The `--token` flag can also be provided via the `PAGURE_API_TOKEN`
environment variable (the flag takes precedence).

### Remove an ACL

```
$ sandogasa-pkg-acl remove freerdp --user olduser --token <token>
Removed user 'olduser' from freerdp
```

### Batch apply from config

Create a TOML config file:

```toml
package = "freerdp"

[users]
ngompa = "admin"
salimma = "commit"
olduser = "remove"

[groups]
kde-sig = "commit"
old-group = "remove"
```

Then run:

```
$ sandogasa-pkg-acl apply -f acls.toml --token <token>
Set user 'ngompa' to 'admin' on freerdp
Set user 'salimma' to 'commit' on freerdp
Removed user 'olduser' from freerdp
Set group 'kde-sig' to 'commit' on freerdp
Removed group 'old-group' from freerdp
```

The value `"remove"` removes all ACLs for that user or group. Any other
value sets the corresponding ACL level.

### JSON output

All subcommands support `--json` for machine-readable output:

```
$ sandogasa-pkg-acl --json show freerdp
{"access_users":{"owner":["ngompa"],"admin":["salimma"],...},"access_groups":{...}}
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
