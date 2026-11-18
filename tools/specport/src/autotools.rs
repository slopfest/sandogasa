// SPDX-License-Identifier: Apache-2.0 OR MIT

//! autotools to Meson: rewrite a spec's `%configure` invocations as
//! `%meson`, its make steps as `%meson_build`, `%meson_install` and
//! `%meson_test`, its `%prep` autoreconf away, and its autotools
//! BuildRequires as `meson`.
//!
//! Each `%configure` keeps its `%if`/`%ifarch` structure, and the old
//! invocation stays above the new one in a comment, with a `FIXME` line
//! for each option without a clean Meson counterpart: the person
//! converting checks those and deletes the block. `%meson` passes
//! `--auto-features=enabled` (Fedora, EL9 and EL10 alike), so a feature
//! option nothing sets would be required; each is set to `disabled`,
//! and listed in the comment, for the person to enable the ones the
//! package wants.

use std::collections::{BTreeMap, BTreeSet};

use crate::meson_options::{Kind, MesonOption};

/// What the new source tree says about its build.
pub struct Source {
    pub options: Vec<MesonOption>,
    /// `meson.build`'s text, searched for where an autotools path
    /// option went.
    pub meson_build: String,
}

/// The converted spec, and what a person has to check in it.
#[derive(Debug, Default, serde::Serialize)]
pub struct Converted {
    pub spec: String,
    /// One line per change made or left for a person, `FIXME` first.
    pub notes: Vec<String>,
}

/// Meson's own directory options, which `%meson` sets as autotools'
/// `%configure` does.
const DIRS: &[&str] = &[
    "prefix",
    "bindir",
    "datadir",
    "includedir",
    "infodir",
    "libdir",
    "libexecdir",
    "localedir",
    "localstatedir",
    "mandir",
    "sbindir",
    "sharedstatedir",
    "sysconfdir",
];

/// autotools options with no meaning for Meson.
const AUTOTOOLS_ONLY: &[&str] = &[
    "disable-dependency-tracking",
    "disable-libtool-lock",
    "disable-maintainer-mode",
    "disable-rpath",
    "disable-silent-rules",
    "disable-static",
    "enable-dependency-tracking",
    "enable-maintainer-mode",
    "enable-shared",
    "enable-silent-rules",
];

/// BuildRequires a Meson build does not need.
const AUTOTOOLS_BRS: &[&str] = &["autoconf", "autoconf-archive", "automake", "libtool"];

/// Commands that generate an autotools build, dropped from scripts.
const GENERATORS: &[&str] = &[
    "aclocal",
    "autoconf",
    "autogen.sh",
    "automake",
    "autopoint",
    "autoreconf",
    "gtkdocize",
    "intltoolize",
    "libtoolize",
];

/// What one `%configure` argument becomes.
#[derive(Debug, PartialEq)]
enum Mapping {
    /// `-Dname=value`.
    Set { name: String, value: String },
    /// Nothing, for the reason given.
    Drop(String),
    /// Nothing, and a person has to decide: the reason, and the Meson
    /// option it concerns, when there is one.
    Fixme(String, Option<String>),
}

/// `s` with `_` as `-`, lower case: how option names are compared.
fn norm(s: &str) -> String {
    s.to_ascii_lowercase().replace('_', "-")
}

/// Options whose names share a word with `name`, or a word that is the
/// initials of `name`'s words (`non-standard` and `amp-ns-decode`).
fn candidates(name: &str, options: &[MesonOption]) -> Vec<String> {
    let words: Vec<String> = norm(name)
        .split('-')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let initials: String = if words.len() > 1 {
        words.iter().filter_map(|w| w.chars().next()).collect()
    } else {
        String::new()
    };
    let mut out: Vec<String> = options
        .iter()
        .filter(|o| {
            norm(&o.name).split('-').any(|w| {
                (w.len() >= 3 && words.iter().any(|x| x == w))
                    || (!initials.is_empty() && w == initials)
            })
        })
        .map(|o| o.name.clone())
        .collect();
    out.sort();
    out
}

/// The option `name` names, as given or with an `enable-`/`with-`
/// prefix, as some projects keep.
fn find<'a>(name: &str, options: &'a [MesonOption]) -> Option<&'a MesonOption> {
    let n = norm(name);
    options.iter().find(|o| norm(&o.name) == n).or_else(|| {
        options.iter().find(|o| {
            let on = norm(&o.name);
            on == format!("enable-{n}") || on == format!("with-{n}")
        })
    })
}

/// What `--enable-X[=v]`, `--disable-X`, `--with-X[=v]` or
/// `--without-X` becomes for `option`: `positive` for enable/with,
/// `literal` the value given, if any.
fn value_for(
    option: &MesonOption,
    positive: bool,
    literal: Option<&str>,
) -> Result<String, String> {
    let yes = literal.is_none_or(|v| v == "yes");
    let no = !positive || literal == Some("no");
    let pick = |pos: &[&str], neg: &[&str]| -> Option<String> {
        let wanted = if no { neg } else { pos };
        wanted
            .iter()
            .find(|w| option.choices.iter().any(|c| c == *w))
            .map(|w| w.to_string())
    };
    match option.kind {
        Kind::Feature => match (no, yes, literal) {
            (true, _, _) => Ok("disabled".into()),
            (false, true, _) => Ok("enabled".into()),
            (false, false, Some("auto")) => Ok("auto".into()),
            _ => Err(format!(
                "a feature takes enabled, disabled or auto, not {literal:?}"
            )),
        },
        Kind::Boolean => match (no, yes) {
            (true, _) => Ok("false".into()),
            (false, true) => Ok("true".into()),
            _ => Err(format!("a boolean takes true or false, not {literal:?}")),
        },
        Kind::Combo => match literal {
            Some(v) if v != "yes" && v != "no" && option.choices.iter().any(|c| c == v) => {
                Ok(v.to_string())
            }
            _ => pick(
                &["enabled", "true", "yes"],
                &["disabled", "false", "no", "none"],
            )
            .ok_or_else(|| format!("choose one of {}", option.choices.join(", "))),
        },
        Kind::String | Kind::Integer | Kind::Array => match literal {
            Some(v) if positive => Ok(v.to_string()),
            _ => Err(format!(
                "a {} option takes a value",
                format!("{:?}", option.kind).to_ascii_lowercase()
            )),
        },
        Kind::Other(ref t) => Err(format!("an option of type {t}")),
    }
}

/// What `arg`, one `%configure` argument, becomes.
fn map_arg(arg: &str, source: &Source) -> Mapping {
    let Some(flag) = arg.strip_prefix("--") else {
        return Mapping::Fixme(format!("{arg}: not an option; carry it over by hand"), None);
    };
    let (key, literal) = match flag.split_once('=') {
        Some((k, v)) => (k, Some(unquote(v))),
        None => (flag, None),
    };
    if AUTOTOOLS_ONLY.contains(&key) {
        return Mapping::Drop(format!("{arg}: autotools only"));
    }
    if DIRS.contains(&key) {
        return match literal {
            Some(v) if v == format!("%{{_{key}}}") || v == format!("%{{_{key}}}/") => {
                Mapping::Drop(format!("{arg}: %meson sets it"))
            }
            Some(v) => Mapping::Set {
                name: key.to_string(),
                value: v.to_string(),
            },
            None => Mapping::Fixme(format!("{arg}: a directory with no value"), None),
        };
    }
    let (positive, name) = if let Some(n) = key.strip_prefix("enable-") {
        (true, n)
    } else if let Some(n) = key.strip_prefix("with-") {
        (true, n)
    } else if let Some(n) = key.strip_prefix("disable-") {
        (false, n)
    } else if let Some(n) = key.strip_prefix("without-") {
        (false, n)
    } else {
        return Mapping::Fixme(format!("{arg}: no Meson counterpart known"), None);
    };
    match find(name, &source.options) {
        Some(option) => match value_for(option, positive, literal) {
            Ok(value) => Mapping::Set {
                name: option.name.clone(),
                value,
            },
            Err(why) => Mapping::Fixme(
                format!("{arg}: Meson's {}: {why}", option.name),
                Some(option.name.clone()),
            ),
        },
        None => {
            let mut why = format!("{arg}: no Meson option {name}");
            let found = candidates(name, &source.options);
            if !found.is_empty() {
                why.push_str(&format!("; candidates: {}", found.join(", ")));
            }
            let upper = name.to_ascii_uppercase().replace('-', "_");
            if let Some((n, _)) = source
                .meson_build
                .lines()
                .enumerate()
                .find(|(_, l)| l.contains(&upper) || l.contains(name))
            {
                why.push_str(&format!("; meson.build mentions it at line {}", n + 1));
            }
            Mapping::Fixme(why, None)
        }
    }
}

/// `v` without the quotes around it, if it has them.
fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// `-Dname=value`, the value quoted for the shell when it has spaces.
fn define(name: &str, value: &str) -> String {
    if value.contains(char::is_whitespace) {
        format!("-D{name}='{value}'")
    } else {
        format!("-D{name}={value}")
    }
}

/// `%{?macro:inner}` or `%{!?macro:inner}`: whether it is negated, the
/// macro, and what it expands to.
fn macro_cond(token: &str) -> Option<(bool, &str, &str)> {
    let body = token.strip_prefix("%{")?.strip_suffix('}')?;
    let (negated, rest) = match body.strip_prefix("!?") {
        Some(r) => (true, r),
        None => (false, body.strip_prefix('?')?),
    };
    let (name, inner) = rest.split_once(':')?;
    Some((negated, name, inner))
}

/// One line of a `%configure` invocation after the first.
#[derive(Debug)]
enum Line {
    Args {
        indent: String,
        args: Vec<String>,
    },
    /// `%if`, `%ifarch`, `%else`, `%elif`, `%endif` and the like.
    Cond(String),
}

/// Whether `line` is a spec conditional, and which kind: `if`, `elif`,
/// `else` or `endif`.
fn cond_kind(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    let word = t.split_whitespace().next()?;
    match word {
        "%endif" => Some("endif"),
        "%else" => Some("else"),
        w if w.starts_with("%elif") => Some("elif"),
        w if w.starts_with("%if") => Some("if"),
        _ => None,
    }
}

/// The arguments on `line`, without a trailing continuation backslash:
/// split at whitespace, except inside quotes and `%{…}`.
fn args_of(line: &str) -> Vec<String> {
    let text = line.trim_end().trim_end_matches('\\');
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quote, mut braces) = (None, 0usize);
    let mut prev = '\0';
    for c in text.chars() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() && braces == 0 => quote = Some(c),
            '{' if prev == '%' && quote.is_none() => braces += 1,
            '}' if braces > 0 && quote.is_none() => braces -= 1,
            c if c.is_whitespace() && quote.is_none() && braces == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                prev = c;
                continue;
            }
            _ => {}
        }
        cur.push(c);
        prev = c;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `line` for a comment: `%` doubled so rpm leaves it alone, and no
/// trailing backslash.
fn commented(line: &str) -> String {
    line.trim_end()
        .trim_end_matches('\\')
        .trim_end()
        .replace('%', "%%")
}

fn continues(line: &str) -> bool {
    line.trim_end().ends_with('\\')
}

/// A `%configure` invocation: its first line's index, its lines.
struct Invocation {
    indent: String,
    first_args: Vec<String>,
    lines: Vec<Line>,
    raw: Vec<String>,
}

/// The invocation starting at `lines[start]`, and the index after it.
fn read_invocation(lines: &[&str], start: usize) -> (Invocation, usize) {
    let first = lines[start];
    let indent: String = first.chars().take_while(|c| c.is_whitespace()).collect();
    let rest = first.trim_start();
    let rest = rest
        .strip_prefix("%configure")
        .or_else(|| rest.strip_prefix("%{configure}"))
        .unwrap_or(rest);
    let mut inv = Invocation {
        indent,
        first_args: args_of(rest),
        lines: Vec::new(),
        raw: vec![first.to_string()],
    };
    let mut i = start + 1;
    let mut open = continues(first);
    while open && i < lines.len() {
        let line = lines[i];
        inv.raw.push(line.to_string());
        if cond_kind(line).is_some() {
            inv.lines.push(Line::Cond(line.to_string()));
        } else {
            inv.lines.push(Line::Args {
                indent: line.chars().take_while(|c| c.is_whitespace()).collect(),
                args: args_of(line),
            });
            open = continues(line);
        }
        i += 1;
    }
    (inv, i)
}

/// The value that turns `option` off, where there is one.
fn off(option: &MesonOption) -> Option<&'static str> {
    match option.kind {
        Kind::Feature => Some("disabled"),
        Kind::Boolean => Some("false"),
        _ => None,
    }
}

/// `items` joined into lines of at most about 72 columns, each with
/// `indent` before it.
fn wrap(indent: &str, items: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for item in items {
        if !cur.is_empty() && cur.len() + item.len() + 1 > 64 {
            out.push(format!("{indent}{cur}"));
            cur.clear();
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(item);
    }
    if !cur.is_empty() {
        out.push(format!("{indent}{cur}"));
    }
    out
}

/// One `-D` item of the new invocation: its text and the options it
/// sets.
struct Item {
    text: String,
    names: Vec<String>,
}

/// What the arguments of one invocation map to, with what was dropped
/// and what a person has to check.
struct Mapper<'a> {
    source: &'a Source,
    fixmes: Vec<String>,
    dropped: Vec<String>,
    /// Options an argument named, even where the value did not map:
    /// they are not disabled as unset.
    mentioned: BTreeSet<String>,
    /// For each (macro, option) set inside `%{?macro:…}` or
    /// `%{!?macro:…}`, which of the two set it (`true` for `!?`).
    by_macro: BTreeMap<(String, String), BTreeSet<bool>>,
}

impl Mapper<'_> {
    /// The options `args` set, outside any macro conditional.
    fn sets(&mut self, args: &[String]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for arg in args {
            match map_arg(arg, self.source) {
                Mapping::Set { name, value } => {
                    self.mentioned.insert(name.clone());
                    out.push((name, value));
                }
                Mapping::Drop(why) => self.dropped.push(why),
                Mapping::Fixme(why, name) => {
                    self.fixmes.push(why);
                    self.mentioned.extend(name);
                }
            }
        }
        out
    }

    /// First pass over every argument: which options each macro
    /// conditional sets, and with which polarity.
    fn scan(&mut self, args: &[String]) {
        for arg in args {
            if let Some((negated, name, inner)) = macro_cond(arg) {
                for a in args_of(inner) {
                    if let Mapping::Set { name: opt, .. } = map_arg(&a, self.source) {
                        self.by_macro
                            .entry((name.to_string(), opt))
                            .or_default()
                            .insert(negated);
                    }
                }
            }
        }
    }

    /// The items `args` become. A macro conditional's options are
    /// mapped inside it, and an option it sets on one side only is
    /// turned off on the other.
    fn items(&mut self, args: &[String]) -> Vec<Item> {
        let mut out = Vec::new();
        for arg in args {
            let Some((negated, name, inner)) = macro_cond(arg) else {
                for (n, v) in self.sets(std::slice::from_ref(arg)) {
                    out.push(Item {
                        text: define(&n, &v),
                        names: vec![n],
                    });
                }
                continue;
            };
            let sets = self.sets(&args_of(inner));
            if sets.is_empty() {
                continue;
            }
            let (here, there) = if negated { ("!?", "?") } else { ("?", "!?") };
            let mut text = format!(
                "%{{{here}{name}:{}}}",
                sets.iter()
                    .map(|(n, v)| define(n, v))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let mut other = Vec::new();
            for (n, _) in &sets {
                let both = self
                    .by_macro
                    .get(&(name.to_string(), n.clone()))
                    .is_some_and(|p| p.len() == 2);
                if both {
                    continue;
                }
                match self
                    .source
                    .options
                    .iter()
                    .find(|o| &o.name == n)
                    .and_then(off)
                {
                    Some(v) => other.push(define(n, v)),
                    None => self.fixmes.push(format!(
                        "{n} is set only when %{{{here}{name}}}; otherwise it keeps its default"
                    )),
                }
            }
            if !other.is_empty() {
                text.push_str(&format!(" %{{{there}{name}:{}}}", other.join(" ")));
            }
            out.push(Item {
                text,
                names: sets.into_iter().map(|(n, _)| n).collect(),
            });
        }
        out
    }
}

/// The `%meson` lines replacing `inv`, comment block first, and the
/// notes for it.
fn convert_invocation(inv: &Invocation, source: &Source) -> (Vec<String>, Vec<String>) {
    let mut m = Mapper {
        source,
        fixmes: Vec::new(),
        dropped: Vec::new(),
        mentioned: BTreeSet::new(),
        by_macro: BTreeMap::new(),
    };
    m.scan(&inv.first_args);
    for line in &inv.lines {
        if let Line::Args { args, .. } = line {
            m.scan(args);
        }
    }
    let arg_indent = inv
        .lines
        .iter()
        .find_map(|l| match l {
            Line::Args { indent, .. } => Some(indent.clone()),
            Line::Cond(_) => None,
        })
        .unwrap_or_else(|| format!("{}\t", inv.indent));

    // Where each option is set: unconditionally (a macro conditional
    // balances itself, so it counts), or in which branch of which
    // top-level %if group. Deeper conditionals count as setting the
    // option, without balancing.
    struct Group {
        branches: Vec<BTreeSet<String>>,
        has_else: bool,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut unconditional: BTreeSet<String> = BTreeSet::new();
    let mut nested: BTreeSet<String> = BTreeSet::new();
    // Mapped lines, in order: Some(items) for an argument line, None
    // for a conditional.
    let mut mapped: Vec<Option<Vec<Item>>> = Vec::new();
    let first_items = m.items(&inv.first_args);
    unconditional.extend(first_items.iter().flat_map(|i| i.names.clone()));
    let mut depth = 0usize;
    for line in &inv.lines {
        match line {
            Line::Cond(raw) => {
                match cond_kind(raw) {
                    Some("if") => {
                        depth += 1;
                        if depth == 1 {
                            groups.push(Group {
                                branches: vec![BTreeSet::new()],
                                has_else: false,
                            });
                        }
                    }
                    Some(k @ ("elif" | "else")) if depth == 1 => {
                        let g = groups.last_mut().expect("open group");
                        g.branches.push(BTreeSet::new());
                        g.has_else |= k == "else";
                    }
                    Some("endif") => depth = depth.saturating_sub(1),
                    _ => {}
                }
                mapped.push(None);
            }
            Line::Args { args, .. } => {
                let items = m.items(args);
                for n in items.iter().flat_map(|i| i.names.iter()) {
                    match depth {
                        0 => {
                            unconditional.insert(n.clone());
                        }
                        1 => {
                            groups
                                .last_mut()
                                .and_then(|g| g.branches.last_mut())
                                .expect("open branch")
                                .insert(n.clone());
                        }
                        _ => {
                            nested.insert(n.clone());
                        }
                    }
                }
                mapped.push(Some(items));
            }
        }
    }
    for n in &nested {
        m.fixmes.push(format!(
            "{n} is set in a nested conditional: check that every branch sets it"
        ));
    }

    // Per group, what each branch has to turn off: what another branch
    // turns on. A group without %else has one more branch, the one where
    // none of its conditions hold, which gets an %else.
    let mut balance: Vec<Vec<Vec<String>>> = Vec::new();
    for g in &groups {
        let slots = g.branches.len() + usize::from(!g.has_else);
        let mut per: Vec<Vec<String>> = vec![Vec::new(); slots];
        let all: BTreeSet<&String> = g.branches.iter().flatten().collect();
        for name in all {
            if unconditional.contains(name) {
                continue;
            }
            let missing: Vec<usize> = (0..slots)
                .filter(|b| !g.branches.get(*b).is_some_and(|s| s.contains(name)))
                .collect();
            if missing.is_empty() {
                continue;
            }
            let Some(value) = source
                .options
                .iter()
                .find(|o| &o.name == name)
                .and_then(off)
            else {
                m.fixmes.push(format!(
                    "{name} is set in only some branches; the others leave it at its default"
                ));
                continue;
            };
            for b in missing {
                per[b].push(define(name, value));
            }
        }
        balance.push(per);
    }

    // The new invocation. Each entry: (line, is an argument line at
    // depth 0).
    let first_line: Vec<String> = first_items.iter().map(|i| i.text.clone()).collect();
    let head = if first_line.is_empty() {
        format!("{}%meson", inv.indent)
    } else {
        format!("{}%meson {}", inv.indent, first_line.join(" "))
    };
    let mut out: Vec<(String, bool)> = Vec::new();
    let (mut depth, mut group, mut branch) = (0usize, 0usize, 0usize);
    for (line, items) in inv.lines.iter().zip(&mapped) {
        match (line, items) {
            (Line::Cond(raw), _) => {
                let kind = cond_kind(raw);
                if depth == 1 && matches!(kind, Some("elif" | "else" | "endif")) {
                    let per = &balance[group - 1];
                    for l in wrap(&arg_indent, &per[branch]) {
                        out.push((l, false));
                    }
                    if kind == Some("endif") && !groups[group - 1].has_else {
                        let fallback = &per[per.len() - 1];
                        if !fallback.is_empty() {
                            out.push(("%else".to_string(), false));
                            for l in wrap(&arg_indent, fallback) {
                                out.push((l, false));
                            }
                        }
                    }
                }
                match kind {
                    Some("if") => {
                        depth += 1;
                        if depth == 1 {
                            group += 1;
                            branch = 0;
                        }
                    }
                    Some("elif" | "else") if depth == 1 => branch += 1,
                    Some("endif") => depth = depth.saturating_sub(1),
                    _ => {}
                }
                out.push((raw.clone(), false));
            }
            (Line::Args { indent, .. }, Some(items)) if !items.is_empty() => {
                let texts: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
                out.push((format!("{indent}{}", texts.join(" ")), depth == 0));
            }
            _ => {}
        }
    }
    let set_anywhere: BTreeSet<String> = unconditional
        .iter()
        .chain(nested.iter())
        .chain(groups.iter().flat_map(|g| g.branches.iter().flatten()))
        .chain(m.mentioned.iter())
        .cloned()
        .collect();
    let unset: Vec<String> = source
        .options
        .iter()
        .filter(|o| o.auto_feature() && !set_anywhere.contains(&o.name))
        .map(|o| define(&o.name, "disabled"))
        .collect();
    for l in wrap(&arg_indent, &unset) {
        out.push((l, true));
    }
    // Every line continues but the last, which has to be an argument
    // line outside the conditionals; %{nil} ends it otherwise. With
    // nothing after the first line, it is the whole invocation.
    if !out.is_empty() && !out.last().is_some_and(|(_, top)| *top) {
        out.push((format!("{arg_indent}%{{nil}}"), true));
    }
    let last = out.len().saturating_sub(1);
    let mut lines: Vec<String> = Vec::new();
    lines.push(if out.is_empty() {
        head
    } else {
        format!("{head} \\")
    });
    for (i, (l, _)) in out.iter().enumerate() {
        if cond_kind(l).is_some() || i == last {
            lines.push(l.clone());
        } else {
            lines.push(format!("{l} \\"));
        }
    }

    // The comment block above it.
    let ind = &inv.indent;
    let mut comment = vec![format!(
        "{ind}# specport: converted from autotools to Meson; the old invocation was:"
    )];
    comment.extend(
        inv.raw
            .iter()
            .map(|r| format!("{ind}#   {}", commented(r.trim_start()))),
    );
    for f in &m.fixmes {
        comment.push(format!("{ind}# FIXME specport: {}", commented(f)));
    }
    if !unset.is_empty() {
        comment.push(format!(
            "{ind}# specport: %%meson passes --auto-features=enabled, which would require \
             the features nothing set: they are disabled at the end; enable the ones wanted"
        ));
    }
    let defaults: Vec<String> = source
        .options
        .iter()
        .filter(|o| o.kind != Kind::Feature && !set_anywhere.contains(&o.name))
        .map(|o| match (&o.value, o.choices.is_empty()) {
            (Some(v), true) => format!("{}={v}", o.name),
            (Some(v), false) => format!("{}={v} ({})", o.name, o.choices.join(", ")),
            (None, _) => o.name.clone(),
        })
        .collect();
    if !defaults.is_empty() {
        comment.push(format!(
            "{ind}# specport: left at their defaults: {}",
            commented(&defaults.join("; "))
        ));
    }

    let mut notes: Vec<String> = m.fixmes.iter().map(|f| format!("FIXME: {f}")).collect();
    notes.extend(m.dropped.iter().map(|d| format!("dropped {d}")));
    if !unset.is_empty() {
        notes.push(format!(
            "disabled {} feature(s) nothing set, which --auto-features=enabled would require",
            unset.len()
        ));
    }
    comment.extend(lines);
    (comment, notes)
}

/// A spec section's name, when `line` starts one.
fn section(line: &str) -> Option<&str> {
    let word = line.split_whitespace().next()?;
    const SECTIONS: &[&str] = &[
        "%build",
        "%changelog",
        "%check",
        "%conf",
        "%description",
        "%files",
        "%generate_buildrequires",
        "%install",
        "%package",
        "%post",
        "%postun",
        "%pre",
        "%prep",
        "%pretrans",
        "%posttrans",
        "%preun",
    ];
    SECTIONS.contains(&word).then_some(word)
}

/// Whether the words of a make command are only variables and flags
/// (`%{?_smp_mflags}`, `V=1`, `-j4`), with `target`, if any, the one
/// other word.
fn make_target(words: &[&str]) -> Option<Option<String>> {
    let mut target = None;
    for w in words {
        if w.contains('=')
            || w.starts_with('-')
            || w.starts_with("%{?_smp")
            || w.starts_with("%{_smp")
        {
            continue;
        }
        if target.is_some() {
            return None;
        }
        target = Some(w.to_string());
    }
    Some(target)
}

/// The Meson step replacing a make command in `section`, if `line` is
/// one.
fn make_replacement(line: &str) -> Option<&'static str> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let (head, rest) = words.split_first()?;
    let target = match *head {
        "make" | "%{__make}" | "%make_build" | "%{make_build}" => make_target(rest)?,
        "%make_install" | "%{make_install}" => {
            return make_target(rest)?.is_none().then_some("%meson_install");
        }
        _ => return None,
    };
    match target.as_deref() {
        None | Some("all") => Some("%meson_build"),
        Some("install") => Some("%meson_install"),
        Some("check") | Some("test") => Some("%meson_test"),
        _ => None,
    }
}

/// Whether `line` runs an autotools generator.
fn is_generator(line: &str) -> bool {
    line.split_whitespace()
        .find(|w| !w.contains('='))
        .is_some_and(|cmd| {
            let base = cmd.rsplit('/').next().unwrap_or(cmd);
            GENERATORS.contains(&base)
        })
}

/// Whether anything in `lines` outside BuildRequires still runs make.
fn uses_make(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        let t = l.trim_start();
        !t.starts_with('#')
            && !t.starts_with("BuildRequires")
            && t.split_whitespace().any(|w| {
                w == "make"
                    || w.starts_with("%make_")
                    || w.starts_with("%{make_")
                    || w == "%{__make}"
            })
    })
}

/// `spec` converted from autotools to Meson, using what `source` says
/// about the new build.
pub fn convert(spec: &str, source: &Source) -> Converted {
    let lines: Vec<&str> = spec.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut invocations = 0;
    let mut unconverted: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if let Some(s) = section(trimmed) {
            current = Some(s.to_string());
        }
        let first = trimmed.split_whitespace().next().unwrap_or("");
        let is_configure = |w: &str| w == "%configure" || w == "%{configure}";
        if !is_configure(first)
            && !trimmed.starts_with('#')
            && trimmed.split_whitespace().any(is_configure)
        {
            unconverted.push(format!(
                "FIXME: `{trimmed}`: %configure after other words (an environment \
                 variable?) is left for a person"
            ));
        }
        if is_configure(first) {
            let (inv, next) = read_invocation(&lines, i);
            let (new, n) = convert_invocation(&inv, source);
            out.extend(new);
            notes.extend(n);
            invocations += 1;
            i = next;
            continue;
        }
        let scripted = matches!(
            current.as_deref(),
            Some("%prep" | "%conf" | "%build" | "%install" | "%check")
        );
        if scripted && !trimmed.starts_with('#') {
            if is_generator(trimmed) {
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                out.push(format!(
                    "{indent}# specport: dropped `{}`: Meson generates no configure script",
                    commented(trimmed)
                ));
                notes.push(format!("dropped `{trimmed}`"));
                i += 1;
                continue;
            }
            if let Some(step) = make_replacement(trimmed) {
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                out.push(format!("{indent}{step}"));
                notes.push(format!("replaced `{trimmed}` with {step}"));
                i += 1;
                continue;
            }
        }
        out.push(line.to_string());
        i += 1;
    }
    if invocations == 0 {
        let mut notes =
            vec!["FIXME: no %configure line found to convert; nothing changed".to_string()];
        notes.extend(unconverted);
        return Converted {
            spec: spec.to_string(),
            notes,
        };
    }
    notes.extend(unconverted);

    // BuildRequires: the autotools go, make too when nothing runs it,
    // and meson comes in after the last BuildRequires.
    let mut drop: Vec<&str> = AUTOTOOLS_BRS.to_vec();
    if !uses_make(&out) {
        drop.push("make");
    }
    let mut result: Vec<String> = Vec::new();
    let mut last_br = None;
    let mut has_meson = false;
    for line in out {
        let Some(value) = line.strip_prefix("BuildRequires:") else {
            result.push(line);
            continue;
        };
        let names: Vec<&str> = value
            .split([' ', '\t', ','])
            .filter(|w| !w.is_empty())
            .collect();
        if names.contains(&"meson") {
            has_meson = true;
        }
        let simple = names.iter().all(|n| {
            !n.starts_with(['<', '>', '=']) && !n.chars().all(|c| c.is_ascii_digit() || c == '.')
        });
        let (gone, kept): (Vec<&str>, Vec<&str>) = names.iter().partition(|n| drop.contains(n));
        if gone.is_empty() || !simple {
            if !gone.is_empty() {
                notes.push(format!("FIXME: {line}: drop {} by hand", gone.join(", ")));
            }
            result.push(line);
            last_br = Some(result.len() - 1);
            continue;
        }
        result.push(format!(
            "# specport: dropped BuildRequires: {}",
            gone.join(" ")
        ));
        notes.push(format!("dropped BuildRequires: {}", gone.join(" ")));
        if !kept.is_empty() {
            let pad: String = value.chars().take_while(|c| c.is_whitespace()).collect();
            result.push(format!("BuildRequires:{pad}{}", kept.join(" ")));
        }
        last_br = Some(result.len() - 1);
    }
    if !has_meson && invocations > 0 {
        let at = last_br.map_or(0, |i| i + 1);
        let pad = result
            .get(last_br.unwrap_or(0))
            .and_then(|l| l.strip_prefix("BuildRequires:"))
            .map(|v| {
                v.chars()
                    .take_while(|c| c.is_whitespace())
                    .collect::<String>()
            })
            .unwrap_or_else(|| " ".to_string());
        result.insert(at, format!("BuildRequires:{pad}meson"));
        notes.push("added BuildRequires: meson".to_string());
    }

    // One note per finding, though each invocation reports its own.
    let mut seen = BTreeSet::new();
    notes.retain(|n| seen.insert(n.clone()));
    notes.sort_by_key(|n| !n.starts_with("FIXME"));
    let mut spec = result.join("\n");
    spec.push('\n');
    Converted { spec, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meson_options::parse;

    fn source(options: &str) -> Source {
        Source {
            options: parse(options).unwrap(),
            meson_build: "conf.set('SYSCONFDEFDIR', get_option('sysconfdir') / 'sysconfig')\n"
                .to_string(),
        }
    }

    const OPTIONS: &str = "\
option('enable-arch', type : 'combo', choices : ['auto', 'all', 'x86', 'arm'], value : 'all')
option('aer', type : 'feature')
option('arm', type : 'feature')
option('cxl', type : 'feature')
option('debug-sql', type : 'feature', value : 'disabled')
option('mysql', type : 'feature')
option('openbmc-unified-sel', type : 'feature')
option('amp-ns-decode', type : 'feature')
option('hisi-ns-decode', type : 'feature')
option('docs', type : 'boolean', value : false)
option('db-path', type : 'string', value : '/var/lib/x.db')
";

    #[test]
    fn an_option_maps_by_its_type() {
        let s = source(OPTIONS);
        let set = |arg: &str| match map_arg(arg, &s) {
            Mapping::Set { name, value } => format!("{name}={value}"),
            other => format!("{other:?}"),
        };
        assert_eq!(set("--enable-aer"), "aer=enabled");
        assert_eq!(set("--disable-cxl"), "cxl=disabled");
        assert_eq!(set("--with-mysql=no"), "mysql=disabled");
        assert_eq!(set("--enable-docs"), "docs=true");
        assert_eq!(set("--without-docs"), "docs=false");
        assert_eq!(set("--with-db-path=/srv/x.db"), "db-path=/srv/x.db");
        assert_eq!(set("--enable-arch=arm"), "enable-arch=arm");
        assert_eq!(set("--with-db_path=/srv/x.db"), "db-path=/srv/x.db");
        assert_eq!(
            set("--libexecdir=%{_libexecdir}/x"),
            "libexecdir=%{_libexecdir}/x"
        );
        assert!(matches!(map_arg("--disable-static", &s), Mapping::Drop(_)));
        assert!(matches!(
            map_arg("--sysconfdir=%{_sysconfdir}", &s),
            Mapping::Drop(_)
        ));
        let fixme = |arg: &str| match map_arg(arg, &s) {
            Mapping::Fixme(why, _) => why,
            other => panic!("{arg}: {other:?}"),
        };
        assert!(fixme("--enable-aer=maybe").contains("feature takes"));
        assert!(fixme("--enable-docs=maybe").contains("boolean takes"));
        assert!(fixme("--enable-db-path").contains("takes a value"));
        assert!(fixme("--enable-arch").contains("choose one of"));
        assert!(fixme("CFLAGS=-O0").contains("not an option"));
        assert!(fixme("--program-prefix=x").contains("no Meson counterpart"));
        assert!(fixme("--libdir").contains("no value"));
        let ns = fixme("--enable-non-standard");
        assert!(
            ns.contains("candidates: amp-ns-decode, hisi-ns-decode"),
            "{ns}"
        );
        let dir = fixme("--with-sysconfdefdir=%{_sysconfdir}/sysconfig");
        assert!(dir.contains("meson.build mentions it at line 1"), "{dir}");
    }

    #[test]
    fn make_steps_become_meson_steps() {
        assert_eq!(
            make_replacement("make %{?_smp_mflags}"),
            Some("%meson_build")
        );
        assert_eq!(make_replacement("%make_build V=1"), Some("%meson_build"));
        assert_eq!(
            make_replacement("make install DESTDIR=%{buildroot}"),
            Some("%meson_install")
        );
        assert_eq!(make_replacement("%make_install"), Some("%meson_install"));
        assert_eq!(make_replacement("%{make_install} foo"), None);
        assert_eq!(make_replacement("make check"), Some("%meson_test"));
        assert_eq!(make_replacement("%make_build -C docs html"), None);
        assert_eq!(make_replacement("make docs html"), None);
        assert_eq!(make_replacement("install -m 644 x y"), None);
        assert!(is_generator("autoreconf -vfi"));
        assert!(is_generator("NOCONFIGURE=1 ./autogen.sh"));
        assert!(!is_generator("%autosetup -p1"));
    }

    const SPEC: &str = "\
Name: rasdaemon
BuildRequires:\t\tmake
BuildRequires:\t\tgcc
BuildRequires:\t\tautoconf automake libtool
BuildRequires:\t\tsqlite-devel

%prep
%autosetup -p1
autoreconf -vfi

%build
%ifarch aarch64
%configure --enable-aer --enable-arm \\
\t   --enable-non-standard --enable-hisi-ns-decode \\
%if 0%{?facebook}
\t   --enable-openbmc-unified-sel \\
%endif
\t   --with-sysconfdefdir=%{_sysconfdir}/sysconfig
%else
%configure --enable-aer \\
\t   --enable-cxl \\
\t   --with-sysconfdefdir=%{_sysconfdir}/sysconfig
%endif
make %{?_smp_mflags}

%install
make install DESTDIR=%{buildroot}

%files
";

    #[test]
    fn rasdaemons_spec_is_converted_with_its_structure_kept() {
        let c = convert(SPEC, &source(OPTIONS));
        let spec = &c.spec;
        // The Arm invocation: options mapped, the Facebook-only one
        // balanced with an %else, unset features disabled last.
        assert!(
            spec.contains(
                "%meson -Daer=enabled -Darm=enabled \\\n\
                 \t   -Dhisi-ns-decode=enabled \\\n\
                 %if 0%{?facebook}\n\
                 \t   -Dopenbmc-unified-sel=enabled \\\n\
                 %else\n\
                 \t   -Dopenbmc-unified-sel=disabled \\\n\
                 %endif\n\
                 \t   -Dcxl=disabled -Dmysql=disabled -Damp-ns-decode=disabled\n\
                 %else\n"
            ),
            "{spec}"
        );
        // The other one has no conditional and disables the Arm options.
        assert!(
            spec.contains(
                "%meson -Daer=enabled \\\n\
                 \t   -Dcxl=enabled \\\n\
                 \t   -Darm=disabled -Dmysql=disabled -Dopenbmc-unified-sel=disabled \\\n\
                 \t   -Damp-ns-decode=disabled -Dhisi-ns-decode=disabled\n\
                 %endif\n\
                 %meson_build\n"
            ),
            "{spec}"
        );
        // The old invocation stays, escaped, with what to check.
        assert!(
            spec.contains("#   %%configure --enable-aer --enable-arm\n"),
            "{spec}"
        );
        assert!(spec.contains("#   %%if 0%%{?facebook}\n"), "{spec}");
        assert!(spec.contains(
            "# FIXME specport: --enable-non-standard: no Meson option non-standard; \
             candidates: amp-ns-decode, hisi-ns-decode"
        ));
        assert!(spec.contains("left at their defaults: enable-arch=all (auto, all, x86, arm); docs=false; db-path=/var/lib/x.db"), "{spec}");
        assert!(spec.contains("# specport: dropped `autoreconf -vfi`"));
        assert!(spec.contains("%meson_install\n"));
        // make is gone with the autotools; meson is in.
        assert!(
            spec.contains(
                "# specport: dropped BuildRequires: make\nBuildRequires:\t\tgcc\n\
             # specport: dropped BuildRequires: autoconf automake libtool\n\
             BuildRequires:\t\tsqlite-devel\nBuildRequires:\t\tmeson\n"
            ),
            "{spec}"
        );
        // No line of the new invocations ends in a dangling backslash
        // before a section or a make step.
        assert!(!spec.contains("\\\n\n"));
        assert_eq!(c.notes[0].split(':').next(), Some("FIXME"));
        assert!(c.notes.iter().any(|n| n == "added BuildRequires: meson"));
    }

    #[test]
    fn an_invocation_ending_in_a_conditional_is_closed() {
        let spec = "%build\n%configure \\\n%if 1\n  --enable-aer \\\n%endif\n  --disable-static\n";
        let s = source("option('aer', type : 'feature')\n");
        let c = convert(spec, &s);
        assert!(
            c.spec.contains(
                "%meson \\\n%if 1\n  -Daer=enabled \\\n%else\n  -Daer=disabled \\\n%endif\n  %{nil}\n"
            ),
            "{}",
            c.spec
        );
    }

    #[test]
    fn a_spec_without_configure_says_so() {
        let spec = "BuildRequires: autoconf\n%build\nCFLAGS=-O0 %configure\nmake\n";
        let c = convert(spec, &source(OPTIONS));
        assert_eq!(
            c.notes[0],
            "FIXME: no %configure line found to convert; nothing changed"
        );
        assert!(c.notes[1].contains("after other words"), "{:?}", c.notes);
        // Not half converted: the spec is as it was.
        assert_eq!(c.spec, spec);
    }

    #[test]
    fn a_versioned_buildrequires_is_left_for_a_person() {
        let spec = "BuildRequires: autoconf >= 2.69\n%build\n%configure\n";
        let c = convert(spec, &source(OPTIONS));
        assert!(c.spec.contains("BuildRequires: autoconf >= 2.69\n"));
        assert!(
            c.notes.iter().any(|n| n.contains("drop autoconf by hand")),
            "{:?}",
            c.notes
        );
    }

    #[test]
    fn an_elif_group_without_else_gets_one() {
        let spec = "%build\n%configure \\\n%ifarch x86_64\n  --enable-aer \\\n\
                    %elifarch aarch64\n  --enable-arm \\\n%endif\n  --disable-static\n";
        let s = source("option('aer', type : 'feature')\noption('arm', type : 'feature')\n");
        let c = convert(spec, &s);
        assert!(
            c.spec.contains(
                "%ifarch x86_64\n  -Daer=enabled \\\n  -Darm=disabled \\\n\
                 %elifarch aarch64\n  -Darm=enabled \\\n  -Daer=disabled \\\n\
                 %else\n  -Daer=disabled -Darm=disabled \\\n%endif\n  %{nil}\n"
            ),
            "{}",
            c.spec
        );
    }

    #[test]
    fn an_option_every_branch_sets_needs_no_balancing() {
        let spec = "%build\n%configure \\\n%ifarch aarch64\n  --enable-arch=arm \\\n\
                    %else\n  --enable-arch=x86 \\\n%endif\n  --enable-aer\n";
        let c = convert(spec, &source(OPTIONS));
        assert!(
            !c.notes.iter().any(|n| n.contains("only some branches")),
            "{:?}",
            c.notes
        );
        assert!(
            c.spec
                .contains("  -Denable-arch=arm \\\n%else\n  -Denable-arch=x86 \\\n")
        );
    }

    #[test]
    fn a_feature_whose_value_did_not_map_is_not_disabled() {
        let spec = "%build\n%configure --with-mysql=/usr/lib64/mysql\n";
        let c = convert(spec, &source(OPTIONS));
        assert!(!c.spec.contains("-Dmysql=disabled"), "{}", c.spec);
        assert!(c.notes.iter().any(|n| n.contains("feature takes")));
    }

    #[test]
    fn a_bcond_argument_is_mapped_inside_its_macro_and_balanced() {
        let spec = "%build\n%configure %{?with_aer:--enable-aer} \\\n\
                    \t%{?with_docs:--enable-docs} %{!?with_docs:--disable-docs} \\\n\
                    \t--with-db-path=\"/srv/my db\"\n";
        let c = convert(spec, &source(OPTIONS));
        assert!(
            c.spec.contains(
                "%meson %{?with_aer:-Daer=enabled} %{!?with_aer:-Daer=disabled} \\\n\
                 \t%{?with_docs:-Ddocs=true} %{!?with_docs:-Ddocs=false} \\\n\
                 \t-Ddb-path='/srv/my db' \\\n"
            ),
            "{}",
            c.spec
        );
        assert!(!c.spec.contains("-Daer=disabled -D"), "{}", c.spec);
        // A combo has no off value to balance with.
        let c = convert(
            "%build\n%configure %{?with_arm:--enable-arch=arm}\n",
            &source(OPTIONS),
        );
        assert!(
            c.notes.iter().any(|n| n.contains("is set only when")),
            "{:?}",
            c.notes
        );
    }
}
