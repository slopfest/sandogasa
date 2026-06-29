# sandogasa-review

Shared interactive **keep / explain / remove** resolution for
reviewer-curated findings.

Several sandogasa tools generate a list of findings (review issues,
installability problems, reverse-dependency breakage) and then let a human
reviewer decide, per finding, what to do before the result is posted:

- **keep** — the finding is real; it stays and still counts.
- **explain** — the finding is real but acceptable; the reviewer records a
  written justification, and it no longer counts against the item.
- **remove** — the finding is a false positive; it's dropped.

This crate provides the `Resolution` enum and the interactive
`resolve_interactive` driver behind that flow, so the mechanism is
identical across tools (e.g. `fedora-review-digest` and `ebranch
check-update`).

## Usage

```rust
use sandogasa_review::{resolve_interactive, Resolution};

// Only call when interactive (a TTY and not --yes); otherwise treat
// every finding as Resolution::Keep.
let findings = vec!["libfoo.so.1 removed — breaks 47 packages"];
let decisions = resolve_interactive(findings, |f| f.to_string())?;

for (finding, resolution) in decisions {
    match resolution {
        Resolution::Keep => { /* counts against the item */ }
        Resolution::Explained(why) => { /* record `why`, don't count */ }
        Resolution::Removed => { /* drop it */ }
    }
}
# Ok::<(), String>(())
```

Each finding is shown as `[i/total] <summary>` followed by
`(k)eep / (e)xplain / (r)emove [k]:` on stderr; Enter keeps (the safe
default), and `explain` requires a non-empty justification.
