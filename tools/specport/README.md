# specport

Port an RPM spec to a new build system when upstream switches. The
mechanical part of the conversion is done for you; the old settings stay
in a comment above the new ones, with a `FIXME` line for each that has
no clean counterpart, for you to check before deleting the comment.

## Installation

```sh
cargo install specport
```

`diff` (diffutils) shows the changes, and `tar` reads a source tarball.

## Commands

### `autotools-to-meson`

```sh
specport autotools-to-meson --source PATH [--write | --json] [--color WHEN] SPEC
```

Converts a spec whose upstream moved from autotools to Meson, using the
new source's `meson.build` and `meson.options` (or `meson_options.txt`),
read from its unpacked tree or straight from its tarball:

- each `%configure` invocation becomes `%meson`, keeping the `%if` and
  `%ifarch` lines inside and around it. Its options map by the Meson
  option's type: `--enable-foo` gives `-Dfoo=enabled` for a feature,
  `-Dfoo=true` for a boolean, and `--with-foo=VALUE` the value for a
  string or one of a combo's choices. Directory options `%meson` sets
  itself are dropped, and so are autotools-only ones such as
  `--disable-static`;
- an option set in only some branches of a conditional is turned off in
  the others, adding an `%else` where there is none, and one set inside
  `%{?with_foo:…}` is turned off in a matching `%{!?with_foo:…}`;
- `%meson` passes `--auto-features=enabled`, which makes a feature
  option nothing sets required, so each of those is set to `disabled`
  at the end of the invocation, for you to enable the ones the package
  wants;
- make steps become `%meson_build`, `%meson_install` and `%meson_test`,
  `autoreconf` and the other generators are dropped from the scripts,
  and the autotools BuildRequires (and `make`, when nothing runs it any
  more) give way to `meson`.

Above each new invocation, a comment keeps the old one, with `%` written
as `%%` so rpm leaves it alone, followed by:

- a `FIXME` line for each option without a clean counterpart, naming
  candidate Meson options and where `meson.build` mentions it;
- the non-feature options left at their defaults, with their choices.

A spec with no `%configure` line to convert is left as it is.
`--write` keeps the original next to the spec, as `<spec>.orig` (or
`.orig.1` and on, never overwriting an earlier one).
Without `--write`, the changes are shown as a diff and the spec is left
alone; `--json` prints the converted spec and the notes instead. On a
terminal (or with `--color always`) the diff is colored: lines taken
out red and lines put in green, with an added `FIXME` line yellow and
the rest of the added comment gray. The notes, `FIXME` first and
yellow, go to standard error, followed by what to do next: the command
that writes the spec, or, after `--write`, the `FIXME` lines to check
and the build to run.

`%files` is left alone: a Meson build may install to other paths, and
the build shows which.
