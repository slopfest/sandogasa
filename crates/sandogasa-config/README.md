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
  inspection (the flag-defaults lookup uses it). `save` lays the
  struct over what the user file already holds, so a table it does
  not model — a hand-written `[defaults]` — and comments on untouched
  keys survive a `config` command. A `config` command loads with
  `load_user`, the user file alone, so a value shipped in `/etc` is not
  copied into the user file when it saves. Either layer may
  be absent, and the system layer alone is enough — `load`
  succeeds from `/etc` with no user file present. `SANDOGASA_ETC`
  points the system layer at another root, so a config a repository
  ships under `configs/<tool>/` can be tried beneath the real user
  file before it is installed (`SANDOGASA_ETC=$PWD/configs`).
- **try_for_tool_file** — the same layered pair for a file other than
  `config.toml`, e.g. a run profile a package ships under
  `/etc/{tool}/` while users override it per key in `~/.config/{tool}/`.
- **Permissions apply to the user file only** — 700 on its
  directory, 600 on the file, corrected in place on read. The
  system file is read as-is, with no mode check, so a packaged
  `root:root 0644` is fine: the system layer is for shared,
  non-secret settings, with credentials left to the per-user file
  (or an environment variable), where the 600 enforcement applies.
- **secret** — secrets in the desktop keyring rather than the config
  file, through libsecret's `secret-tool`. An entry stands for the
  service it unlocks, not the tool, with attributes `application
  sandogasa`, `service <API host>` and, when a service needs one,
  `account <who>` (`SecretRef`), so every tool talking to gitlab.com
  reads one GitLab token. `resolve` finds a secret in the environment,
  then the keyring, then the config file; `keep` / `keep_for_file`
  store one in the keyring and say where it went, falling back to the
  file when there is no keyring to use. `SecretStore` is the seam:
  `SecretTool` for real use, `MemoryStore` for tests that must not
  touch the user's keyring. `ConfigFile::remove` takes a moved secret
  out of the file, which `save` alone never does.
- **prompt_field** — prompt the user for a config value with support for
  sensitive (hidden) input and optional sync validation.
  `prompt_optional_field` and `prompt_optional_field_validated` take
  an empty answer as "none". All of them apply ^H and DEL characters
  that reach them as erases, for terminals whose backspace sends the
  one the tty does not erase on.
- **Setting** and **ask** — the way a `config` command asks for each
  setting. A `Setting` names it and says whether it is sensitive
  (hidden, and a saved value is never shown), optional, what hint to
  print first, and how to validate it. `ask(setting, saved, review)`
  keeps a saved value without asking; asks for a missing one; reports
  a saved value its validator refuses and asks again; and with
  `review`, asks with the saved value as the default, where an empty
  answer keeps it, an answer replaces it, and `-` clears an optional
  one (unless the validator accepts a lone `-`).
- **validate_email** — basic email address validation for config
  fields; spaces, control characters and a lone `-` are refused.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
