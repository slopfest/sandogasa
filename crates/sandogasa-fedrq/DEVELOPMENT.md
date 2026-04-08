# sandogasa-fedrq Development Notes

## Fedora Metalink Repository Names

fedrq uses Fedora's metalink service to locate repository mirrors.
The metalink URLs follow the pattern:

```
https://mirrors.fedoraproject.org/metalink?repo=REPONAME&arch=ARCH
```

### Authoritative sources

- **Prefix generation**: [MirrorManager's `repomap.py`](https://github.com/fedora-infra/mirrormanager2/blob/master/mirrormanager2/lib/repomap.py)
  defines how repo prefixes are generated from filesystem paths.

- **Live query**: send an invalid repo name to get the full list of
  valid `repo=NAME&arch=ARCH` combinations (returned as XML comments):

  ```
  curl -s "https://mirrors.fedoraproject.org/metalink?repo=INVALID&arch=x86_64"
  ```

- **Serving code**: [mirrorlist-server](https://github.com/adrianreber/mirrorlist-server)
  (Rust) is the actual server behind `mirrors.fedoraproject.org`.

- **Consumer-side .repo files**: defined in
  [fedora-repos](https://src.fedoraproject.org/rpms/fedora-repos/tree/rawhide) and
  [epel-release](https://src.fedoraproject.org/rpms/epel-release).

### Naming conventions

| Repo type | Pattern | Example |
|-----------|---------|---------|
| Fedora base | `fedora-{version}` | `fedora-44` |
| Fedora updates | `updates-released-f{version}` | `updates-released-f44` |
| Fedora testing | `updates-testing-f{version}` | `updates-testing-f44` |
| Rawhide | `rawhide` | `rawhide` |
| EPEL stable | `epel-{version}` | `epel-9`, `epel-10` |
| EPEL testing | `testing-epel{version}` | `testing-epel9` |

Note the asymmetry: EPEL stable uses `epel-{version}` (with hyphen)
but EPEL testing uses `testing-epel{version}` (no hyphen before
version number).

### Known gaps (as of 2026-04)

- `testing-epel10` does not exist in MirrorManager. EPEL 10+ testing
  repos are not yet available via metalink, so fedrq's `@testing`
  probe fails for EPEL 10. See the CLAUDE.md note on this limitation.
