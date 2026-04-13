# poi-tracker

Package-of-interest tracker for Fedora, EPEL, and CentOS SIGs.

Manages TOML-based inventories of packages that an organization
tracks across distributions. Supports exporting to content-resolver
YAML and hs-relmon manifest formats.

## Installation

```sh
cargo install poi-tracker
```

## Usage

### Show inventory

```sh
poi-tracker show -i inventory.toml
poi-tracker show -i inventory.toml --domain hyperscale
poi-tracker show -i inventory.toml --json
```

### Add / remove packages

```sh
poi-tracker add systemd -i inventory.toml \
    --poc "Team <team@example.com>" \
    --rpm systemd-networkd \
    --domain hyperscale \
    --track upstream

poi-tracker remove systemd -i inventory.toml
poi-tracker remove systemd -i inventory.toml --rpm systemd-networkd
```

### Export to content-resolver YAML

```sh
# Writes to {inventory-name}.yaml by default
poi-tracker export content-resolver -i inventory.toml

# Filter by domain
poi-tracker export content-resolver -i inventory.toml \
    --domain hyperscale

# Custom output path
poi-tracker export content-resolver -i inventory.toml -o custom.yaml
```

### Export to hs-relmon manifest

```sh
# Merge multiple inventories into one manifest
poi-tracker export hs-relmon \
    -i inv-cloud.toml -i inv-hw.toml -o manifest.toml

# Filter by domain
poi-tracker export hs-relmon -i inventory.toml \
    --domain hyperscale -o manifest.toml
```

### Import from legacy JSON

```sh
poi-tracker import old-inventory.json -o inventory.toml \
    --private-fields poc,reason,team,task \
    --domain hyperscale
```

### Validate

```sh
poi-tracker validate -i inventory.toml
```

## Inventory format

```toml
[inventory]
name = "hyperscale-packages"
description = "CentOS Hyperscale SIG packages"
maintainer = "centos-hyperscale"
labels = ["eln-extras"]
domains = ["hyperscale"]
private_fields = ["poc", "reason", "team", "task"]

[[package]]
name = "systemd"
poc = "Linux Userspace <team@example.com>"
reason = "Core init system"
rpms = ["systemd-networkd"]
track = "upstream"

[package.arch_rpms]
x86_64 = ["systemd-boot-unsigned"]
aarch64 = ["systemd-boot-unsigned"]

[[package]]
name = "fish"
rpms = ["fish"]
track = "upstream"
```

### Fields

| Field | Level | Description |
|-------|-------|-------------|
| `name` | inventory/package | Name (required) |
| `description` | inventory | Human-readable description |
| `maintainer` | inventory | Maintainer (person or team) |
| `labels` | inventory | Tags for content-resolver |
| `domains` | inventory/package | Domain tags for filtering |
| `private_fields` | inventory | Fields stripped on export |
| `poc` | package | Point of contact |
| `reason` | package | Reason for tracking |
| `team` | package | Team responsible |
| `task` | package | Internal task/ticket |
| `rpms` | package | Binary RPMs to track |
| `arch_rpms` | package | Architecture-specific RPMs |
| `track` | package | hs-relmon tracking branch |
| `repology_name` | package | Repology name override |
| `distros` | package | hs-relmon distribution list |
| `file_issue` | package | File GitLab issues |

## License

[MPL-2.0](https://mozilla.org/MPL/2.0/)
