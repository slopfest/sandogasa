# sandogasa-config

Shared config file management and interactive prompting for sandogasa CLI tools.

## Features

- **ConfigFile** — load and save TOML config files at
  `~/.config/{tool}/config.toml`, generic over any `Serialize`/`Deserialize`
  type. Reads are layered: an optional system-wide
  `/etc/{tool}/config.toml` is merged beneath the user file (the
  user file wins per key, recursively for tables; command-line
  flags override both). `save` only ever writes the user file.
  `read_merged` exposes the raw merged TOML for generic
  inspection (the flag-defaults lookup uses it). Either layer may
  be absent, and the system layer alone is enough — `load`
  succeeds from `/etc` with no user file present.
- **Permissions apply to the user file only** — 700 on its
  directory, 600 on the file, corrected in place on read. The
  system file is read as-is, with no mode check, so a packaged
  `root:root 0644` is fine: the system layer is for shared,
  non-secret settings, with credentials left to the per-user file
  (or an environment variable), where the 600 enforcement applies.
- **prompt_field** — prompt the user for a config value with support for
  sensitive (hidden) input and optional sync validation.
- **validate_email** — basic email address validation for config fields.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
