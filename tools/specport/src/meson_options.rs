// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A project's Meson options, read from `meson_options.txt` or
//! `meson.options`: each `option('name', type : 'feature', value : …,
//! choices : […], description : '…')` call.

/// What kind of value an option takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `enabled`, `disabled` or `auto`.
    Feature,
    Boolean,
    /// One of `choices`.
    Combo,
    String,
    Integer,
    Array,
    Other(String),
}

/// One option a project declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MesonOption {
    pub name: String,
    pub kind: Kind,
    /// The default as written, `None` when the file gives none (a
    /// feature then defaults to `auto`).
    pub value: Option<String>,
    pub choices: Vec<String>,
    pub description: String,
}

impl MesonOption {
    /// Whether a feature would be required when nothing sets it, under
    /// `--auto-features=enabled` (which `%meson` passes): one that
    /// defaults to `auto`.
    pub fn auto_feature(&self) -> bool {
        self.kind == Kind::Feature && self.value.as_deref().is_none_or(|v| v == "auto")
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Str(String),
    Ident(String),
    Punct(char),
}

/// The tokens of a Meson file: strings (single-quoted, `'''` multiline
/// ones included), identifiers and numbers as identifiers, and
/// punctuation; comments are dropped.
fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '\'' {
            let triple = chars[i..].starts_with(&['\'', '\'', '\'']);
            let quote = if triple { 3 } else { 1 };
            i += quote;
            let mut s = String::new();
            loop {
                if i >= chars.len() {
                    return Err("unterminated string".into());
                }
                if triple && chars[i..].starts_with(&['\'', '\'', '\'']) {
                    i += 3;
                    break;
                }
                if !triple && chars[i] == '\'' {
                    i += 1;
                    break;
                }
                if !triple && chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                s.push(chars[i]);
                i += 1;
            }
            out.push(Token::Str(s));
        } else if c.is_alphanumeric() || c == '_' || c == '-' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || "_-.".contains(chars[i])) {
                i += 1;
            }
            out.push(Token::Ident(chars[start..i].iter().collect()));
        } else if c.is_whitespace() {
            i += 1;
        } else {
            out.push(Token::Punct(c));
            i += 1;
        }
    }
    Ok(out)
}

/// A keyword argument's value: a string, a bare word (`true`, a
/// number), or a list of them.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    One(String),
    List(Vec<String>),
}

fn value(toks: &[Token], i: &mut usize) -> Result<Value, String> {
    match toks.get(*i) {
        Some(Token::Str(s)) | Some(Token::Ident(s)) => {
            *i += 1;
            Ok(Value::One(s.clone()))
        }
        Some(Token::Punct('{')) => {
            // A dict, as `deprecated` may take: skipped, nothing here
            // reads one.
            let mut depth = 0usize;
            loop {
                match toks.get(*i) {
                    Some(Token::Punct('{')) => depth += 1,
                    Some(Token::Punct('}')) => {
                        depth -= 1;
                        if depth == 0 {
                            *i += 1;
                            return Ok(Value::One(String::new()));
                        }
                    }
                    None => return Err("unterminated dict".into()),
                    _ => {}
                }
                *i += 1;
            }
        }
        Some(Token::Punct('[')) => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                match toks.get(*i) {
                    Some(Token::Punct(']')) => {
                        *i += 1;
                        return Ok(Value::List(items));
                    }
                    Some(Token::Punct(',')) => *i += 1,
                    Some(Token::Str(s)) | Some(Token::Ident(s)) => {
                        items.push(s.clone());
                        *i += 1;
                    }
                    other => return Err(format!("unexpected {other:?} in a list")),
                }
            }
        }
        other => Err(format!("unexpected {other:?} for a value")),
    }
}

/// The options `text`, a `meson_options.txt` or `meson.options`,
/// declares, in the order it declares them.
pub fn parse(text: &str) -> Result<Vec<MesonOption>, String> {
    let toks = tokens(text)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i] != Token::Ident("option".into()) || toks.get(i + 1) != Some(&Token::Punct('(')) {
            i += 1;
            continue;
        }
        i += 2;
        let Some(Token::Str(name)) = toks.get(i) else {
            return Err("an option() without a name".into());
        };
        let name = name.clone();
        i += 1;
        let mut kind = None;
        let (mut default, mut choices, mut description) = (None, Vec::new(), String::new());
        loop {
            match toks.get(i) {
                Some(Token::Punct(')')) => {
                    i += 1;
                    break;
                }
                Some(Token::Punct(',')) => i += 1,
                Some(Token::Ident(key)) if toks.get(i + 1) == Some(&Token::Punct(':')) => {
                    let key = key.clone();
                    i += 2;
                    let v = value(&toks, &mut i)?;
                    match (key.as_str(), v) {
                        ("type", Value::One(t)) => kind = Some(t),
                        ("value", Value::One(v)) => default = Some(v),
                        ("value", Value::List(v)) => default = Some(v.join(",")),
                        ("choices", Value::List(c)) => choices = c,
                        ("description", Value::One(d)) => description = d,
                        _ => {}
                    }
                }
                other => return Err(format!("option {name}: unexpected {other:?}")),
            }
        }
        let kind = match kind.as_deref() {
            Some("feature") => Kind::Feature,
            Some("boolean") => Kind::Boolean,
            Some("combo") => Kind::Combo,
            Some("string") => Kind::String,
            Some("integer") => Kind::Integer,
            Some("array") => Kind::Array,
            Some(other) => Kind::Other(other.to_string()),
            None => return Err(format!("option {name} has no type")),
        };
        out.push(MesonOption {
            name,
            kind,
            value: default,
            choices,
            description,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_read_with_their_types_defaults_and_choices() {
        let text = "\
# SPDX-License-Identifier: GPL-2.0-only
option('disable-all', type : 'boolean', value: false,
        description: 'disable all features. Useful to check broken deps')
option('enable-arch', type : 'combo', choices : ['auto', 'all', 'x86'],
       value : 'all', description : 'select the family')
option('sqlite3-database', type: 'string',
       value: '/var/lib/rasdaemon/ras-mc_event.db',
       description: 'SQLite3 database path (default: /var/lib/x.db)',
)
option('aer', type : 'feature', description: 'enable PCIe AER events')
option('debug-sql', type : 'feature', value : 'disabled',
        description: 'it\\'s quoted')
option('jobs', type : 'integer', min : 1, value : 4)
option('list', type : 'array', value : ['a', 'b'],
       deprecated : {'old' : 'a', 'nested' : {'x' : 'y'}})
option('doc', type : 'feature', description : '''multi
line''')
";
        let opts = parse(text).unwrap();
        let names: Vec<&str> = opts.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "disable-all",
                "enable-arch",
                "sqlite3-database",
                "aer",
                "debug-sql",
                "jobs",
                "list",
                "doc"
            ]
        );
        assert_eq!(opts[0].kind, Kind::Boolean);
        assert_eq!(opts[0].value.as_deref(), Some("false"));
        assert_eq!(opts[1].choices, ["auto", "all", "x86"]);
        assert_eq!(opts[1].value.as_deref(), Some("all"));
        assert_eq!(
            opts[2].value.as_deref(),
            Some("/var/lib/rasdaemon/ras-mc_event.db")
        );
        assert!(opts[3].auto_feature());
        assert!(!opts[4].auto_feature());
        assert_eq!(opts[5].kind, Kind::Integer);
        assert_eq!(opts[6].value.as_deref(), Some("a,b"));
        assert_eq!(opts[4].description, "it's quoted");
        assert_eq!(opts[7].description, "multi\nline");
    }

    #[test]
    fn a_malformed_file_is_an_error() {
        assert!(parse("option('x', type : 'feature'").is_err());
        assert!(parse("option('x', type : 'feature', deprecated : {'a' : 'b'").is_err());
        assert!(parse("option(type : 'feature')").is_err());
        assert!(parse("option('x', description : 'no type')").is_err());
        assert!(parse("option('x', type : 'feature', description : 'open)").is_err());
        assert_eq!(
            parse("option('x', type : 'frob')").unwrap()[0].kind,
            Kind::Other("frob".into())
        );
    }
}
