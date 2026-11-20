# Development trajectory

sandogasa's size and pace, release by release: every `vX.Y.Z` tag measured
and drawn into one PDF, attached to each release by
`.github/workflows/trajectory.yml`.

```
make trajectory            # every release tag
make trajectory HEAD=1     # and the tree at HEAD
make trajectory PRIVATE=1  # and when commits were made, by weekday and hour
```

`trajectory.py` extracts each tag's tree with `git archive`, counts it with
tokei (15), and writes one JSON row per tag: lines by language, Rust code
and Rust tests apart (a file's `#[cfg(test)]` module, and `tests/`
directories), the number of library crates and tools, the commits and
lines changed since the previous tag, the days between them, a basic
COCOMO estimate (organic, with the `cocomo` tool's constants), and the
shares of the release's commits made at the weekend, in the daytime, the
evening and at night. `trajectory.typ` draws them. It needs git, Python
3.11, tokei and typst; nothing else.

- COCOMO's estimate assumes a human team at a human pace. Next to the
  actual days between releases, it measures this project's AI-assisted
  pace, not its value.
- Commit times are when changes were committed, not when the work was
  done.
- Each author's commits are read in the offset most of their commits carry
  that day: a commit made in a web interface set to an old zone, or from
  a machine in another, is converted. Run at a terminal, the script asks
  about each such commit (the majority is the default) and records the
  answer in `timezones.toml`, to be committed; without one it uses the
  recorded answers and the majority, and says which commits it decided
  that way. A day with no majority offset is asked about, or left as it
  is; a day the clocks changed on is left as it is.
- Commit times count every author, unless `--author` names some.
- Rust tests are the lines of each file's last `#[cfg(test)]` module and
  of `tests/` directories; the rest of a file is code. Doc tests (code
  blocks in `///` and `//!` comments) are comments to tokei and count as
  neither; there were 84 lines of them on 2026-11-20.
- The public report shows only those coarse shares. `PRIVATE=1` adds the
  weekday x hour heatmap, into `trajectory-private.pdf`; it describes a
  person's working pattern, so it is never attached to a release.

`make trajectory-test` runs the script's tests.
