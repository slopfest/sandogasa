# sandogasa-cbs

The CentOS Build System (CBS) conventions shared by the sandogasa tools
that work for CentOS SIGs: which collection a SIG's dist-git branch
builds into, that collection's build target and tags, the GitLab group
holding the SIG's package repositories, the mock config matching its
buildroot, and the login CBS's `cbs` koji profile needs.

Known SIGs: Hyperscale and Proposed Updates.

## Collections

A collection is one SIG's packages for one CentOS major release in one
repository. Every CBS name for it derives from
`<sig><major>[s]-packages-<repository>`:

| Name | Example |
|---|---|
| build target | `hyperscale9s-packages-main-el9s` |
| buildroot tag | `hyperscale9s-packages-main-el9s-build` |
| stage tags | `…-candidate`, `…-testing`, `…-release` |

```rust
use sandogasa_cbs::{Sig, Stage};

let c = Sig::Hyperscale.collection_for_branch("c9s-hs+fb").unwrap();
assert_eq!(c.target(), "hyperscale9s-packages-facebook-el9s");
assert_eq!(c.tag(Stage::Testing), "hyperscale9s-packages-facebook-testing");
assert_eq!(
    c.mock_config("x86_64").as_deref(),
    Some("centos-stream-hyperscale-facebook-9-x86_64")
);
```

## Branches

| SIG | Branch | Repository |
|---|---|---|
| Proposed Updates | `c<N>s-proposed-<RHEL key>` | `main` |
| Hyperscale | `c<N>s-hs`, `c<N>s-sig-hyperscale` | `main` |
| Hyperscale | `c<N>s-sig-hyperscale-v<N>` (older releases, for emergency builds) | `main` |
| Hyperscale | `c<N>s-hs+asahi` | `asahi` |
| Hyperscale | `c<N>s-hs+fb` | `facebook` |
| Hyperscale | `c<N>s-hsk` | `kernel` |
| Hyperscale | `c<N>s-hsx`, `c<N>s-sig-hyperscale-experimental` | `experimental` |
| Hyperscale | `c<N>s-sig-hyperscale-spin` | `spin` |

`Sig::detect` finds the SIG and collection for a branch of any of them.
Any other branch is an error that names it.

`Sig::own_branch_major` tells a SIG's own branches (Proposed Updates'
`c<N>s-proposed-<key>`, Hyperscale's `c<N>s-hs` and
`c<N>s-sig-hyperscale`) from the rest, and `Sig::newest_own_branch`
picks the one a SIG repo should open on: the newest Stream release's,
and of several, the one with the newest head commit.

Hyperscale's Facebook branches also build for the RHEL release
(`hyperscale9-packages-facebook-el9`), for systems that deploy RHEL
snapshots; `Collection::rhel_variant` names that collection.

A SIG's release package (`Sig::release_package`:
`centos-release-hyperscale`, `centos-release-proposed_updates`) is built
from any of the SIG's `c<N>s` branches into Extras' common collection,
`extras<N>s-extras-common` (`Collection::extras_common`).
`release_package_collection` gives that collection for any
`centos-release-*` package and branch (every SIG's goes there, whatever
it is named after, such as `centos-release-ceph-reef`), and `None` for
any other package. `stream_major` reads the CentOS Stream
release a branch starts from (10 for `c10s` or `c10s-proposed-RHEL-1`).

## Remotes

`remotes::Remotes::parse` reads `git remote -v` and knows each remote by
the project it points at, not its name: CentOS Stream's repo
(`redhat/centos-stream/rpms/<pkg>`), a SIG's (`Role::Sig`), or a
personal fork (`<user>/centos_rpms_<pkg>`). `find(role)` gives the
remote playing a part, `package()` the package they are for, and
`name_for(role)` the name to add one under: the SIG's name
(`proposed_updates`, `hyperscale`) for a SIG's repo.

## Mock configs

`Collection::mock_config` names the mock config whose buildroot
matches the collection: `centos-stream-<N>-<arch>` (mock-core-configs)
for Proposed Updates, `centos-stream-hyperscale[-experimental|-facebook|-spin]-<N>-<arch>`
(mock-centos-sig-configs) for Hyperscale. It returns `None` where mock
ships no matching config.

## Login

`login::ensure_login` checks that CBS accepts the login
(`koji --profile cbs moshimoshi`) before anything is written to it. A
failure names its cause, such as a missing or expired `~/.centos.cert`,
with the `kinit` and `centos-cert` commands that fix it, and at a
terminal offers to run `centos-cert`.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
