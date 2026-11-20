# Terminal recordings

Demos of the tools at work, recorded with
[asciinema](https://asciinema.org) (`sudo dnf install asciinema`). Each
release attaches an animated GIF of every recording it adds or changes,
rendered by `.github/workflows/casts.yml`; `make casts` renders them
locally with [agg](https://github.com/asciinema/agg)
(`cargo binstall --git https://github.com/asciinema/agg agg`;
the crate named `agg` on crates.io is another project). To watch one in a
terminal instead: `asciinema play casts/<name>.cast`.

| Recording | Shows |
| --- | --- |
| `cbspkg-build-promote.cast` | cbspkg 0.28.2 building rasdaemon 1.0.0-2 for CentOS Hyperscale 10, both variants, and promoting the builds to testing |

## Recording one

1. Size the terminal first, to about 100 by 30 or 120 by 35
   (`echo (tput cols)x(tput lines)` in fish); resizing during a
   recording plays back badly.
2. `asciinema rec -i 2 <name>.cast`, outside tmux or in a pane of its
   own. `-i 2` caps every pause at two seconds on playback.
3. Before committing, read the recording for anything private: email
   addresses, tokens, paths under your home directory. fish writes the
   working directory, with the host name, into an invisible escape
   sequence (OSC 7) at every prompt; strip those from the file.
