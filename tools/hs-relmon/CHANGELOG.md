<!-- SPDX-License-Identifier: MPL-2.0 -->

# Changelog

## 0.1.0 - 2026-03-09

Initial release.

- Add `check-latest` command to query the latest version of a package
  across upstream (Repology), Fedora (Rawhide and stable), CentOS Stream,
  and Hyperscale (EL9 and EL10)
- Hyperscale builds report release and testing status separately via CBS
  Koji tag lookup
- Support filtering which distributions to check with `--distros`
- Table output by default, machine-readable JSON with `--json`
