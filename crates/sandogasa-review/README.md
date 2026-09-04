# sandogasa-review

Shared interactive resolution of a list of items — a finding, a bug, a
package — one question each, in the tool's own words.

Several sandogasa tools generate a list and then let a human decide,
per item, before anything is written. The three answers are always the
same shape, and their meaning is the tool's:

| resolution              | finding review (default words) | `poi-tracker kondo`         | `fedora-cve-triage`                 |
|-------------------------|--------------------------------|-----------------------------|-------------------------------------|
| `Resolution::Keep`      | **keep** — real, still counts  | **cull** the package        | **apply** the planned bug action    |
| `Resolution::Explained` | **explain** — accepted, with a justification on record | **essential** — file it into an inventory | **explain** — apply, with a note on the bug |
| `Resolution::Removed`   | **remove** — a false positive  | **skip** — undecided for now | **skip** — leave the bug alone      |

Enter picks the first answer, the safe default; the second takes its
text on the same line (`e <why>`) or asks for it next.

## Usage

```rust
use sandogasa_review::{Prompt, Resolution, Vocabulary, Word, resolve};

// Only call when interactive (a TTY and not --yes); otherwise treat
// every item as Resolution::Keep.
let words = Vocabulary {
    keep: Word::new('a', "apply"),
    explain: Word::new('e', "explain"),
    remove: Word::new('s', "skip"),
    explanation: "note",
};
let bugs = vec!["bug 2418496 — CVE-2025-13466 cachelib: body-parser DoS"];
let decided = resolve(bugs, |b| b.to_string(), &Prompt { vocab: words, ..Prompt::default() })?;
for (bug, resolution, _note) in decided {
    match resolution {
        Resolution::Keep => { /* apply the plan */ }
        Resolution::Explained(note) => { /* apply it, comment `note` */ }
        Resolution::Removed => { /* leave it alone */ }
    }
}
# Ok::<(), String>(())
```

Each item is shown as `[i/total] <summary>` followed by, for the
vocabulary above, `(a)pply / (e)xplain <note> / (s)kip [a]:` on
stderr. `Prompt::default_explanation` is what Enter records at the
text prompt when set (a destination inventory, say); `Prompt::notes`
also accepts `<keep-letter> <note>`, keeping with words that come back
as the third tuple element. `resolve_interactive(items, summary)` is
the default-words, no-notes shortcut returning `(item, resolution)`
pairs.

## Any other question: `Menu` and `ask`

The three-answer review is built on a general one-question primitive,
for prompts whose answers are not keep/explain/remove — `poi-tracker
act` (`(y) orphan / (g)ive <user> / (u)ncull [inventory] / (s)kip /
(q)uit [s]:`) and `reconcile`'s ride-or-essential, keep-or-demote
questions:

```rust
use sandogasa_review::{Answer, Arg, Choice, Menu, ask};

let choices = [
    Choice::new('y', "orphan"),
    Choice::with('g', "give", Arg::Required("user")),
    Choice::with('u', "uncull", Arg::Optional("inventory")),
    Choice::new('s', "skip"),
];
let menu = Menu { choices: &choices, default: Some('s'), all: false, quit: true, default_arg: None };
match ask("old-toy (owner) — nothing essential needs it", &menu)? {
    Answer::Pick { key: 'g', arg } => { /* give it to arg */ }
    Answer::Pick { key: 'u', arg } => { /* uncull, filing into arg if given */ }
    Answer::Pick { .. } => { /* skip, or 'y' */ }
    Answer::All => { /* the default, here and for the rest */ }
    Answer::Quit => { /* stop */ }
}
# Ok::<(), String>(())
```

A `Choice` is a `Word` (letter and full word, both accepted) plus what
may follow it: nothing, an optional argument (`u keep.toml`), or a
required one that `ask` prompts for on its own line when it was not
given (`g`, then `user:`), with `Menu::default_arg` as what Enter takes
there. `Menu::default` is what Enter picks; `all` opens `a`/`all` (this
answer for the rest of a batch) and `quit` opens `q`/`quit` — a choice
keyed `a` or `q` wins over them. Labels render as `(k)eep` when the word
starts with the letter and `(y) orphan` when it does not. `parse_answer`
is the pure half, for tests.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
