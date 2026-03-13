# sandogasa-config

Shared config file management and interactive prompting for sandogasa CLI tools.

## Features

- **ConfigFile** — load and save TOML config files at
  `~/.config/{tool}/config.toml`, generic over any `Serialize`/`Deserialize`
  type.
- **prompt_field** — prompt the user for a config value with support for
  sensitive (hidden) input and optional sync validation.

## License

MPL-2.0
