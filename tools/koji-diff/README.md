# koji-diff

Koji build log differ -- compare buildroot and build logs between two Koji
builds.

## Usage

```
koji-diff <REF1> <REF2> [OPTIONS]
```

Each reference can be:

- A Koji build URL: `https://koji.fedoraproject.org/koji/buildinfo?buildID=2970379`
- A Koji task URL: `https://koji.fedoraproject.org/koji/taskinfo?taskID=143927217`
- A prefixed ID: `build:2970379` or `task:143927217` (requires `--instance`)

### Options

- `--instance <HOST>` -- Koji instance hostname (required for bare IDs)
- `--arch <ARCH>` -- Architecture to compare (default: `x86_64`)
- `--json` -- Output as JSON
- `--log-lines <N>` -- Failure log tail lines to show (default: 50)
- `--debug` -- Show diagnostic info (file listings, parse stats)

### Examples

Compare two builds by URL:

```
koji-diff \
  https://koji.fedoraproject.org/koji/buildinfo?buildID=2970832 \
  https://koji.fedoraproject.org/koji/buildinfo?buildID=2970379
```

Compare two tasks with bare IDs:

```
koji-diff task:143889060 task:143927217 \
  --instance koji.fedoraproject.org
```

## How it works

1. Resolves each reference to a buildArch task for the requested architecture
   via the Koji XML-RPC API
2. Downloads logs using `koji download-logs`
3. Parses installed packages from the DNF transaction table in `root.log`
   (supports both DNF4 and DNF5)
4. Shows which packages were added, removed, or changed between the two builds
5. If either build failed, shows the tail of `mock_output.log` (dependency
   resolution errors) or `build.log` (rpmbuild errors)

## Version change color-coding

Changed packages are color-coded by semver severity (using Rust semver rules):

- `=` green -- same upstream version, only release/dist differs
- `~` bright yellow -- compatible (patch change, or minor on >= 1.0)
- `!` orange -- minor version change on 0.x (breaking under Rust semver)
- `!!` red -- major version differs

## Supported Koji instances

- `koji.fedoraproject.org` -- default koji profile
- `cbs.centos.org` -- `koji -p cbs`
- `kojihub.stream.centos.org` -- `koji -p stream`

## License

MPL-2.0
