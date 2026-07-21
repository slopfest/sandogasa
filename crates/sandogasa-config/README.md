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
  inspection (the flag-defaults lookup uses it).
- **prompt_field** — prompt the user for a config value with support for
  sensitive (hidden) input and optional sync validation.
- **validate_email** — basic email address validation for config fields.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
