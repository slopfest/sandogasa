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
- `--build-log-lines <N>` -- Number of build.log tail lines to show (default: 50)

### Examples

Compare two builds by URL:

```
koji-diff \
  https://koji.fedoraproject.org/koji/buildinfo?buildID=2970379 \
  https://koji.fedoraproject.org/koji/buildinfo?buildID=2970832
```

Compare two tasks with bare IDs:

```
koji-diff task:143889060 task:143927217 \
  --instance koji.fedoraproject.org
```

## How it works

1. Resolves each reference to a buildArch task for the requested architecture
2. Downloads `root.log` from both tasks
3. Parses installed packages from each `root.log`
4. Shows which packages were added, removed, or changed between the two builds
5. If either build failed, shows the tail of `build.log`

## License

MPL-2.0
