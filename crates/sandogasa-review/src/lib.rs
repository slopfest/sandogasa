// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared keep/explain/remove resolution for reviewer-curated findings.
//!
//! Both `ebranch check-update` and `fedora-review-digest` present a list
//! of machine-generated findings and let the reviewer, per finding,
//! **keep** it (real, stays and still counts), **explain** it (real but
//! acceptable, with a written justification kept on record), or **remove**
//! it (a false positive, dropped). This crate is the shared interactive
//! primitive behind that flow.
//!
//! The caller is responsible for only invoking [`resolve_interactive`]
//! when interactive (a TTY and not `--yes`); a non-interactive run should
//! simply treat every finding as [`Resolution::Keep`].

use std::io::Write;

/// How a reviewer resolved one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Real / unresolved — the default. Still counts (blocks / downvotes).
    Keep,
    /// Accepted with a written justification — kept on record, but no
    /// longer counts against the item.
    Explained(String),
    /// A false positive — dropped entirely.
    Removed,
}

/// One answer at the prompt: the letter that picks it and the word,
/// also accepted in full. Shown as `(k)eep` — the letter in
/// parentheses, the rest of the word after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Word {
    pub key: char,
    pub word: &'static str,
}

impl Word {
    pub const fn new(key: char, word: &'static str) -> Self {
        Self { key, word }
    }

    /// `(k)eep` when the word starts with its letter, `(y) orphan`
    /// when it does not.
    pub fn label(&self) -> String {
        match self.word.strip_prefix(self.key) {
            Some(rest) => format!("({}){rest}", self.key),
            None => format!("({}) {}", self.key, self.word),
        }
    }

    fn matches(&self, answer: &str) -> bool {
        answer == self.key.to_string() || answer == self.word
    }
}

/// What an answer may carry after its letter, on the same line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    None,
    /// `u [inventory]`: taken when given.
    Optional(&'static str),
    /// `e <why>`: asked for on the next line when not given, where
    /// Enter takes the menu's `default_arg` if there is one.
    Required(&'static str),
}

/// One answer of a [`Menu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub word: Word,
    pub arg: Arg,
}

impl Choice {
    pub const fn new(key: char, word: &'static str) -> Self {
        Self {
            word: Word::new(key, word),
            arg: Arg::None,
        }
    }

    pub const fn with(key: char, word: &'static str, arg: Arg) -> Self {
        Self {
            word: Word::new(key, word),
            arg,
        }
    }

    /// `(g)ive <user>`, `(u)ncull [inventory]`, `(k)eep`.
    pub fn label(&self) -> String {
        match self.arg {
            Arg::None => self.word.label(),
            Arg::Optional(name) => format!("{} [{name}]", self.word.label()),
            Arg::Required(name) => format!("{} <{name}>", self.word.label()),
        }
    }
}

/// A question about one item: its answers, what Enter picks, and
/// whether `a` (this answer for the rest of the batch) and `q` (stop
/// asking) are open. A choice keyed `a` or `q` wins over those, so a
/// menu that uses the letters must not also open them.
#[derive(Debug, Clone, Copy)]
pub struct Menu<'a> {
    pub choices: &'a [Choice],
    /// The key Enter picks; `None` makes Enter re-ask.
    pub default: Option<char>,
    /// Accept `a` / `all`: the default for this and every remaining item.
    pub all: bool,
    /// Accept `q` / `quit`.
    pub quit: bool,
    /// What Enter records when a [`Arg::Required`] answer's text is
    /// asked for on its own line.
    pub default_arg: Option<&'a str>,
}

/// What the user answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Pick {
        key: char,
        arg: Option<String>,
    },
    /// `a`: the default, here and for the rest.
    All,
    /// `q`, or end of input.
    Quit,
}

/// Parse one line against `menu`: the letter or the full word, an
/// argument after whitespace where the choice takes one. `None` for
/// anything else (the caller re-asks). A required argument that is
/// missing still parses — [`ask`] prompts for it next.
pub fn parse_answer(line: &str, menu: &Menu) -> Option<Answer> {
    let line = line.trim();
    let (head, rest) = match line.split_once(char::is_whitespace) {
        Some((h, r)) => (h.to_ascii_lowercase(), r.trim()),
        None => (line.to_ascii_lowercase(), ""),
    };
    if head.is_empty() {
        return menu.default.map(|key| Answer::Pick { key, arg: None });
    }
    if let Some(c) = menu.choices.iter().find(|c| c.word.matches(&head)) {
        return match (c.arg, rest.is_empty()) {
            (Arg::None, false) => None,
            (_, true) => Some(Answer::Pick {
                key: c.word.key,
                arg: None,
            }),
            (_, false) => Some(Answer::Pick {
                key: c.word.key,
                arg: Some(rest.to_string()),
            }),
        };
    }
    if !rest.is_empty() {
        return None;
    }
    match head.as_str() {
        "a" | "all" if menu.all => Some(Answer::All),
        "q" | "quit" if menu.quit => Some(Answer::Quit),
        _ => None,
    }
}

/// The prompt line: `(y) orphan / (g)ive <user> / (s)kip / (q)uit [s]:`.
fn menu_line(menu: &Menu) -> String {
    let mut parts: Vec<String> = menu.choices.iter().map(Choice::label).collect();
    if menu.all {
        parts.push("(a)ll".to_string());
    }
    if menu.quit {
        parts.push("(q)uit".to_string());
    }
    match menu.default {
        Some(d) => format!("  {} [{d}]: ", parts.join(" / ")),
        None => format!("  {}: ", parts.join(" / ")),
    }
}

/// Ask about one item: `summary` on its own line (when not empty), then
/// the menu, re-asking on anything unrecognized and asking for a
/// required argument that was not given. End of input is
/// [`Answer::Quit`]. Prompts on stderr and reads stdin.
pub fn ask(summary: &str, menu: &Menu) -> Result<Answer, String> {
    ask_with(summary, menu, &mut read_line, &mut std::io::stderr())
}

fn ask_with(
    summary: &str,
    menu: &Menu,
    read: &mut impl FnMut() -> Result<String, String>,
    err: &mut impl Write,
) -> Result<Answer, String> {
    let line = menu_line(menu);
    loop {
        if !summary.is_empty() {
            let _ = writeln!(err, "{summary}");
        }
        let _ = write!(err, "{line}");
        let _ = err.flush();
        let answer = match read() {
            Ok(l) => l,
            Err(e) if e == "end of input" => return Ok(Answer::Quit),
            Err(e) => return Err(e),
        };
        let Some(answer) = parse_answer(&answer, menu) else {
            let keys: Vec<String> = menu
                .choices
                .iter()
                .map(|c| c.word.key.to_string())
                .collect();
            let _ = writeln!(err, "  answer one of {}", keys.join(", "));
            continue;
        };
        let Answer::Pick { key, arg: None } = &answer else {
            return Ok(answer);
        };
        let Some(Choice {
            arg: Arg::Required(name),
            ..
        }) = menu.choices.iter().find(|c| c.word.key == *key)
        else {
            return Ok(answer);
        };
        // The text this answer needs, on its own line — asked again
        // until given, unless Enter has a default to take.
        loop {
            match menu.default_arg {
                Some(default) => {
                    let _ = write!(err, "    {name} [{default}]: ");
                }
                None => {
                    let _ = write!(err, "    {name}: ");
                }
            }
            let _ = err.flush();
            let text = match read() {
                Ok(l) => l.trim().to_string(),
                Err(e) if e == "end of input" => return Ok(Answer::Quit),
                Err(e) => return Err(e),
            };
            if !text.is_empty() {
                return Ok(Answer::Pick {
                    key: *key,
                    arg: Some(text),
                });
            }
            if let Some(default) = menu.default_arg {
                return Ok(Answer::Pick {
                    key: *key,
                    arg: Some(default.to_string()),
                });
            }
            let _ = writeln!(err, "  a {name} is required");
        }
    }
}

/// What the three answers are called. The resolution they produce is
/// always the same — [`Resolution::Keep`], [`Resolution::Explained`],
/// [`Resolution::Removed`] — but what that *means* differs per flow, and
/// the prompt should say so: a finding review keeps, explains or removes
/// a finding; kondo culls a package, makes it essential or skips it;
/// fedora-cve-triage applies a planned bug action, explains it with a
/// note on the bug, or skips the bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vocabulary {
    /// Confirms the finding — the default, what Enter picks.
    pub keep: Word,
    /// Confirms it with text attached, given on the same line or asked
    /// for next.
    pub explain: Word,
    /// Sets the item aside.
    pub remove: Word,
    /// What the attached text is called (`explanation`; kondo:
    /// `inventory`; cve-triage: `note`).
    pub explanation: &'static str,
}

impl Vocabulary {
    /// keep / explain / remove — a review of findings.
    pub const DEFAULT: Vocabulary = Vocabulary {
        keep: Word::new('k', "keep"),
        explain: Word::new('e', "explain"),
        remove: Word::new('r', "remove"),
        explanation: "explanation",
    };
}

impl Default for Vocabulary {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How one review is run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Prompt<'a> {
    pub vocab: Vocabulary,
    /// What Enter records at the explanation prompt, when set (an
    /// explicit explanation still wins) — for flows where it is usually
    /// the same thing, a destination file say.
    pub default_explanation: Option<&'a str>,
    /// Accept `<keep> <note>`: keep with words on record, returned as
    /// the third tuple element.
    pub notes: bool,
}

/// Walk `items`, prompting the reviewer to keep / explain / remove each
/// one (Enter keeps). `summary` renders the finding line shown at the
/// prompt. Returns each item paired with its [`Resolution`], in input
/// order — including [`Resolution::Removed`], so callers can act on
/// removals (e.g. strip them from a report) rather than only seeing the
/// survivors.
///
/// Prompts on stderr and reads stdin. Only call this when interactive.
pub fn resolve_interactive<T>(
    items: Vec<T>,
    summary: impl Fn(&T) -> String,
) -> Result<Vec<(T, Resolution)>, String> {
    let noted = resolve(items, summary, &Prompt::default())?;
    Ok(noted.into_iter().map(|(t, r, _)| (t, r)).collect())
}

/// Walk `items` under `prompt`: the words of its [`Vocabulary`], its
/// default explanation, and notes when asked for. Returns each item
/// with its [`Resolution`] and note, in input order. Prompts on stderr
/// and reads stdin; only call this when interactive.
pub fn resolve<T>(
    items: Vec<T>,
    summary: impl Fn(&T) -> String,
    prompt: &Prompt,
) -> Result<Vec<(T, Resolution, Option<String>)>, String> {
    resolve_with(items, summary, prompt, read_line, &mut std::io::stderr())
}

/// Core resolution loop, with the line reader and prompt sink injected so
/// it can be driven in tests without a real terminal.
fn resolve_with<T>(
    items: Vec<T>,
    summary: impl Fn(&T) -> String,
    prompt: &Prompt,
    mut read: impl FnMut() -> Result<String, String>,
    mut err: impl Write,
) -> Result<Vec<(T, Resolution, Option<String>)>, String> {
    let v = &prompt.vocab;
    let choices = [
        Choice {
            word: v.keep,
            arg: if prompt.notes {
                Arg::Optional("note")
            } else {
                Arg::None
            },
        },
        Choice {
            word: v.explain,
            arg: Arg::Required(v.explanation),
        },
        Choice {
            word: v.remove,
            arg: Arg::None,
        },
    ];
    let menu = Menu {
        choices: &choices,
        default: Some(v.keep.key),
        all: false,
        quit: false,
        default_arg: prompt.default_explanation,
    };
    let total = items.len();
    let mut out = Vec::with_capacity(total);
    for (i, item) in items.into_iter().enumerate() {
        let answer = ask_with(
            &format!("[{}/{total}] {}", i + 1, summary(&item)),
            &menu,
            &mut read,
            &mut err,
        )?;
        let (resolution, note) = match answer {
            Answer::Pick { key, arg } if key == v.keep.key => (Resolution::Keep, arg),
            Answer::Pick { key, arg } if key == v.explain.key => {
                (Resolution::Explained(arg.unwrap_or_default()), None)
            }
            Answer::Pick { .. } => (Resolution::Removed, None),
            // Neither `a` nor `q` is open here; end of input keeps.
            Answer::All | Answer::Quit => (Resolution::Keep, None),
        };
        out.push((item, resolution, note));
    }
    Ok(out)
}

/// Read one line from stdin.
/// Read one line from stdin; "end of input" at EOF, which `ask` turns
/// into [`Answer::Quit`].
fn read_line() -> Result<String, String> {
    let mut line = String::new();
    let n = std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("reading answer: {e}"))?;
    if n == 0 {
        return Err("end of input".to_string());
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed canned answers; the end of the list is the end of input.
    fn reader(lines: &[&str]) -> impl FnMut() -> Result<String, String> {
        let mut it = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .into_iter();
        move || it.next().ok_or_else(|| "end of input".to_string())
    }

    const ACT: [Choice; 5] = [
        Choice::new('y', "orphan"),
        Choice::with('g', "give", Arg::Required("user")),
        Choice::with('u', "uncull", Arg::Optional("inventory")),
        Choice::new('s', "skip"),
        Choice::new('q', "quit"),
    ];

    fn act_menu() -> Menu<'static> {
        Menu {
            choices: &ACT,
            default: Some('s'),
            all: false,
            quit: false,
            default_arg: None,
        }
    }

    #[test]
    fn labels_read_as_the_tools_print_them() {
        assert_eq!(Word::new('k', "keep").label(), "(k)eep");
        assert_eq!(Word::new('y', "orphan").label(), "(y) orphan");
        assert_eq!(ACT[1].label(), "(g)ive <user>");
        assert_eq!(ACT[2].label(), "(u)ncull [inventory]");
        assert_eq!(
            menu_line(&act_menu()),
            "  (y) orphan / (g)ive <user> / (u)ncull [inventory] / (s)kip / (q)uit [s]: "
        );
        let m = Menu {
            choices: &ACT[..2],
            default: None,
            all: true,
            quit: true,
            default_arg: None,
        };
        assert_eq!(
            menu_line(&m),
            "  (y) orphan / (g)ive <user> / (a)ll / (q)uit: "
        );
    }

    #[test]
    fn parse_answer_takes_letters_words_and_arguments() {
        let m = act_menu();
        let pick = |key, arg: Option<&str>| Answer::Pick {
            key,
            arg: arg.map(str::to_string),
        };
        for line in ["", "  ", "s", "Skip"] {
            assert_eq!(parse_answer(line, &m), Some(pick('s', None)), "{line:?}");
        }
        assert_eq!(parse_answer("y", &m), Some(pick('y', None)));
        assert_eq!(parse_answer("ORPHAN", &m), Some(pick('y', None)));
        assert_eq!(
            parse_answer("g decathorpe", &m),
            Some(pick('g', Some("decathorpe")))
        );
        assert_eq!(
            parse_answer("G  someone ", &m),
            Some(pick('g', Some("someone")))
        );
        // A required argument may be missing here: `ask` prompts for it.
        assert_eq!(parse_answer("g", &m), Some(pick('g', None)));
        assert_eq!(parse_answer("u", &m), Some(pick('u', None)));
        assert_eq!(
            parse_answer("u keep.toml", &m),
            Some(pick('u', Some("keep.toml")))
        );
        // An answer without an argument takes none.
        assert_eq!(parse_answer("orphan it", &m), None);
        assert_eq!(parse_answer("x", &m), None);
        // `a` and `q` only where the menu opens them (and a choice wins).
        assert_eq!(parse_answer("a", &m), None);
        assert_eq!(parse_answer("q", &m), Some(pick('q', None)));
        let open = Menu {
            all: true,
            quit: true,
            choices: &ACT[..1],
            ..act_menu()
        };
        assert_eq!(parse_answer("a", &open), Some(Answer::All));
        assert_eq!(parse_answer("quit", &open), Some(Answer::Quit));
        let no_default = Menu {
            default: None,
            ..act_menu()
        };
        assert_eq!(parse_answer("", &no_default), None);
    }

    #[test]
    fn ask_prompts_for_a_missing_required_argument() {
        let mut err = Vec::new();
        let a = ask_with(
            "pkg",
            &act_menu(),
            &mut reader(&["g", "", "alice"]),
            &mut err,
        )
        .unwrap();
        assert_eq!(
            a,
            Answer::Pick {
                key: 'g',
                arg: Some("alice".to_string())
            }
        );
        let shown = String::from_utf8(err).unwrap();
        assert!(shown.contains("    user: "), "{shown}");
        assert!(shown.contains("a user is required"), "{shown}");
        // With a default, Enter takes it.
        let m = Menu {
            default_arg: Some("bob"),
            ..act_menu()
        };
        let a = ask_with("pkg", &m, &mut reader(&["g", ""]), &mut Vec::new()).unwrap();
        assert_eq!(
            a,
            Answer::Pick {
                key: 'g',
                arg: Some("bob".to_string())
            }
        );
        // Junk re-asks; end of input quits.
        let a = ask_with(
            "pkg",
            &act_menu(),
            &mut reader(&["x", "zz"]),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(a, Answer::Quit);
    }

    #[test]
    fn resolve_keeps_explains_removes_in_order() {
        let items = vec!["a", "b", "c", "d"];
        let out = resolve_with(
            items,
            |s| s.to_string(),
            &Prompt::default(),
            reader(&["k", "e because", "r", ""]),
            &mut Vec::new(),
        )
        .unwrap();
        let got: Vec<(&str, Resolution)> = out.into_iter().map(|(t, r, _)| (t, r)).collect();
        assert_eq!(
            got,
            vec![
                ("a", Resolution::Keep),
                ("b", Resolution::Explained("because".to_string())),
                ("c", Resolution::Removed),
                ("d", Resolution::Keep),
            ]
        );
    }

    #[test]
    fn a_vocabulary_renames_every_answer() {
        let cull = Vocabulary {
            keep: Word::new('c', "cull"),
            explain: Word::new('e', "essential"),
            remove: Word::new('s', "skip"),
            explanation: "inventory",
        };
        let prompt = Prompt {
            vocab: cull,
            notes: true,
            ..Prompt::default()
        };
        let mut err = Vec::new();
        let out = resolve_with(
            vec!["x", "y", "z", "w"],
            |s| s.to_string(),
            &prompt,
            reader(&["c old toy", "essential keep.toml", "s", "k"]),
            &mut err,
        )
        .unwrap();
        assert_eq!(out[0].1, Resolution::Keep);
        assert_eq!(out[0].2.as_deref(), Some("old toy"));
        assert_eq!(out[1].1, Resolution::Explained("keep.toml".to_string()));
        assert_eq!(out[2].1, Resolution::Removed);
        // `k` means nothing here: the reader ran out, which keeps.
        assert_eq!(out[3].1, Resolution::Keep);
        let shown = String::from_utf8(err).unwrap();
        assert!(
            shown.contains("(c)ull [note] / (e)ssential <inventory> / (s)kip [c]: "),
            "{shown}"
        );
    }

    #[test]
    fn a_note_rides_on_keep_only_where_notes_are_on() {
        let out = resolve_with(
            vec!["x"],
            |s| s.to_string(),
            &Prompt::default(),
            reader(&["k not used anymore", "k"]),
            &mut Vec::new(),
        )
        .unwrap();
        // Without notes, `k <text>` is junk and re-asks; the plain `k` keeps.
        assert_eq!(out[0].1, Resolution::Keep);
        assert_eq!(out[0].2, None);
    }

    #[test]
    fn blank_explanation_takes_the_default_when_one_is_set() {
        let mut err = Vec::new();
        let out = resolve_with(
            vec!["x"],
            |s| s.to_string(),
            &Prompt {
                default_explanation: Some("default.toml"),
                ..Prompt::default()
            },
            reader(&["e", ""]),
            &mut err,
        )
        .unwrap();
        assert_eq!(out[0].1, Resolution::Explained("default.toml".to_string()));
        let shown = String::from_utf8(err).unwrap();
        assert!(shown.contains("explanation [default.toml]: "), "{shown}");
    }

    #[test]
    fn resolve_reasks_on_junk_and_blank_explanation() {
        let mut err = Vec::new();
        let out = resolve_with(
            vec!["x"],
            |s| s.to_string(),
            &Prompt::default(),
            reader(&["zzz", "e", "", "fine"]),
            &mut err,
        )
        .unwrap();
        assert_eq!(out[0].1, Resolution::Explained("fine".to_string()));
        let shown = String::from_utf8(err).unwrap();
        assert!(shown.contains("answer one of k, e, r"), "{shown}");
        assert!(shown.contains("a explanation is required"), "{shown}");
    }
}
