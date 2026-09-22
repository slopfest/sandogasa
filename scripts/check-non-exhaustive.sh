#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Fail when a library crate under crates/ has a public struct that
# derives Deserialize without #[non_exhaustive]. Such a struct is an
# API response model that callers could build with a struct literal,
# so every field the API grows and a tool starts reading would be a
# breaking release — as `Issue.updated_at` in sandogasa-forgejo was
# on 2026-09-22, when twelve crates then had to be marked in one go.
#
# Crates whose deserialized structs are our own file formats, built
# and written by the tools, are exempt: callers construct those on
# purpose. Add a crate here only for that reason.
set -euo pipefail
cd "$(dirname "$0")/.."

EXEMPT='sandogasa-closure sandogasa-inventory'

status=0
for crate in crates/*/; do
  name=$(basename "$crate")
  case " $EXEMPT " in *" $name "*) continue ;; esac
  # A derive line, any further attribute lines, then `pub struct`:
  # report the struct unless #[non_exhaustive] is among the attributes.
  hits=$(awk '
    /^#\[derive\(/ { attrs = $0; next }
    attrs != "" && /^#\[/ { attrs = attrs "\n" $0; next }
    attrs != "" && /^pub struct / {
      if (attrs ~ /Deserialize/ && attrs !~ /non_exhaustive/) print FILENAME ": " $3
      attrs = ""; next
    }
    { attrs = "" }
  ' $(find "$crate/src" -name '*.rs'))
  if [ -n "$hits" ]; then
    echo "$hits"
    status=1
  fi
done
if [ "$status" -ne 0 ]; then
  echo "error: public response models must be #[non_exhaustive] (see DEVELOPMENT.md); exempt a crate in $0 only for a file format the tools build themselves" >&2
fi
exit $status
